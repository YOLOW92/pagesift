//! 出口池：部署者配置的多个出口（机房代理、住宅代理、移动代理、直连）之间按统计自动选择，
//! 某出口被某站点按 IP 信誉拒绝时自动换走。
//!
//! **护栏**（由调用方的结构保证，池子本身不放宽）：
//! - 礼貌预算按站点算、所有出口共享：换出口的重试照样经过同一个站点限速器，对站点的总速率不变；
//! - 429、带 Retry-After 的 503、限速类拦截页一律退避（[`Action::Backoff`]），不换出口；
//! - robots.txt 按站点缓存，与出口无关（[`crate::RobotsCache`] 走默认出口）；
//! - 不破解验证码、不伪装指纹，User-Agent 不随出口变化；挑战页最多换一次出口，仍出现就如实报告；
//! - 带 Cookie 的请求固定在该站点第一次带 Cookie 时用的出口上，从不换出口。
//!
//! **选择**：Thompson 采样（W. R. Thompson, Biometrika 25, 1933）。每个出口从
//! `P(被拒 | 站点, 出口)` 的 Beta 后验抽一个值 `p̃_e`，取
//! `argmin_e  c·p̃_e + λ_e`：`c` 是一次被拒的代价（白抓一次加重抓一次，按服务时间计为 2），
//! `λ_e` 是出口的影子价格 `W/(1−ρ_e)`（[`pagesift_stat::decide::shadow_price`]，单位同样是服务时间，`W = 1`），
//! `ρ_e` 由该出口的在途请求数推出。后验按"站点 → 注册域 → 出口类别"收缩（见 [`stats`]）。
//!
//! **遗忘**：计数用折扣 Thompson 采样（V. Raj, S. Kalyani, "Taming Non-stationary Bandits:
//! A Bayesian Approach", arXiv:1707.09727, 2017）：同一站点每来一次观测，该站点所有出口的计数先乘
//! `γ`，再把这次结果加到所用的出口上。被拒过的出口在别的出口继续工作期间证据逐渐变弱，后验的
//! 不确定性回升，它会被重新抽到；站点解封后自然回到原出口，不需要定时器。

use std::sync::atomic::{AtomicUsize, Ordering};

use pagesift_stat::Delta;
use pagesift_stat::beta::{BetaCounts, Posterior, wilson};
use pagesift_stat::decide::shadow_price;
use pagesift_stat::rng::Rng;
use serde::{Deserialize, Serialize};

mod stats;
#[cfg(test)]
mod tests;

use stats::Stats;

/// 出口类别。旧配置没写 `class` 时是 `direct`。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Default, Deserialize, Serialize)]
#[serde(rename_all = "lowercase")]
pub enum EgressClass {
    /// 机房（数据中心）代理。
    Datacenter,
    /// 住宅代理。
    Residential,
    /// 移动网络代理。
    Mobile,
    /// 不经代理（或经部署者的企业代理，与默认出口相同）。
    #[default]
    Direct,
}

impl EgressClass {
    /// [推导] 类别数，等于 [`EgressClass`] 的变体数
    pub const COUNT: usize = 4;

    pub fn as_str(self) -> &'static str {
        match self {
            Self::Datacenter => "datacenter",
            Self::Residential => "residential",
            Self::Mobile => "mobile",
            Self::Direct => "direct",
        }
    }

    pub fn parse(s: &str) -> Option<Self> {
        match s {
            "datacenter" => Some(Self::Datacenter),
            "residential" => Some(Self::Residential),
            "mobile" => Some(Self::Mobile),
            "direct" => Some(Self::Direct),
            _ => None,
        }
    }

    fn index(self) -> usize {
        self as usize
    }
}

/// 请求的站点：主机名和它的注册域（调用方给，例如 `pagesift_content::site_key`）。
#[derive(Debug, Clone, Copy)]
pub struct Site<'a> {
    pub host: &'a str,
    pub domain: &'a str,
}

/// 一次抓取在某个出口上的结果，按拦截分类归类。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Outcome {
    /// 拿到了正常内容。
    Served,
    /// 限速：429、带 Retry-After 的 503、限速类拦截页。是站点在控制节奏，不怪出口。
    Throttled,
    /// WAF 或访问控制直接拒绝（403、拦截页 `block`）。
    Blocked,
    /// 挑战页或验证码（拦截页 `challenge`、`captcha`）。
    Challenge,
    /// 连接失败、超时、上游错误：出口或站点不可达。
    Unreachable,
    /// 与出口无关的结果：登录墙、中间页、451、站点 5xx、调用方的错误、主机冷却中。
    Neutral,
}

impl Outcome {
    /// 按拦截页识别的结果归类（`kind` 是 `BarrierKind::as_str`，`rule_id` 是命中的规则）。
    /// 识别器没有单独的"限速"类别：规则 id 里带 `rate` 的（如"rate limited"页）按限速处理。
    pub fn from_barrier(kind: &str, rule_id: &str) -> Outcome {
        if rule_id.contains("rate") {
            return Outcome::Throttled;
        }
        match kind {
            "challenge" | "captcha" => Outcome::Challenge,
            "block" => Outcome::Blocked,
            _ => Outcome::Neutral,
        }
    }

    /// 按拿到的响应状态归类（没有识别出拦截页时）。
    pub fn from_status(status: u16, retry_after: bool) -> Outcome {
        // [协议] 状态码按 RFC 9110 §15（429 见 RFC 6585 §4，503 + Retry-After 见 RFC 9110 §15.6.4）
        match status {
            429 => Outcome::Throttled, // [协议] RFC 6585 §4 Too Many Requests
            503 if retry_after => Outcome::Throttled, // [协议] RFC 9110 §15.6.4
            403 => Outcome::Blocked,   // [协议] RFC 9110 §15.5.4 Forbidden
            // [协议] 4xx、5xx（RFC 9110 §15.5、§15.6）。401 要凭证、451 是法律原因（RFC 7725），都不是出口的信誉问题
            400..=599 => Outcome::Neutral,
            _ => Outcome::Served,
        }
    }

    /// 计入被拒后验的方向：`Some(true)` 被拒、`Some(false)` 成功、`None` 不计。
    fn denial(self) -> Option<bool> {
        match self {
            Outcome::Served => Some(false),
            Outcome::Blocked | Outcome::Challenge | Outcome::Unreachable => Some(true),
            Outcome::Throttled | Outcome::Neutral => None,
        }
    }
}

/// 报告结果后池子给的处理办法。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Action {
    /// 照原样返回结果。
    Keep,
    /// 限速：退避由礼貌层记在站点上，不换出口。
    Backoff,
    /// 换一次出口重试（[`EgressPool::choose_retry`]）。`other_class` 时优先换到不同类别（挑战页）。
    Switch { other_class: bool },
    /// 试过的出口都被这个站点拒绝：如实报告，不再换。
    SiteWide,
}

/// 池子的设置。
#[derive(Debug, Clone)]
pub struct PoolConfig {
    pub delta: Delta,
    /// 统计表最多占多少字节（按条目估算）；超出时样本少的站点先淘汰。
    pub memory_budget: usize,
    /// 抽样种子；`None` 用时间熵。测试用固定种子复现。
    pub seed: Option<u64>,
}

impl Default for PoolConfig {
    fn default() -> Self {
        Self {
            delta: Delta::DEFAULT,
            // [策略] 默认 8 MiB：4 个出口时一个站点条目约 150 字节，够记五万个以上站点，
            // 是 robots 缓存、退避表清理门槛（4096 个源）的十倍以上；部署者可改
            memory_budget: 8 * 1024 * 1024,
            seed: None,
        }
    }
}

/// 独占缓存行的在途计数（[推导] 128 字节：相邻缓存行成对预取，按两行对齐）。
#[repr(align(128))]
#[derive(Default)]
struct Load(AtomicUsize);

struct Member {
    name: String,
    class: EgressClass,
    inflight: Load,
}

/// 出口池。成员固定（启动时由配置给出），统计在运行中学习。
pub struct EgressPool {
    members: Box<[Member]>,
    stats: Stats,
    delta: Delta,
}

/// [推导] 一次被拒的代价，以服务时间为单位：白抓的一次加上重抓的一次
const DENIAL_COST: f64 = 2.0;

impl EgressPool {
    /// `members`：（名字，类别），顺序即出口编号。
    pub fn new(members: Vec<(String, EgressClass)>, config: PoolConfig) -> Self {
        let members: Box<[Member]> = members
            .into_iter()
            .map(|(name, class)| Member {
                name,
                class,
                inflight: Load::default(),
            })
            .collect();
        let classes: Vec<EgressClass> = members.iter().map(|m| m.class).collect();
        Self {
            stats: Stats::new(classes, &config),
            members,
            delta: config.delta,
        }
    }

    pub fn len(&self) -> usize {
        self.members.len()
    }

    pub fn is_empty(&self) -> bool {
        self.members.is_empty()
    }

    pub fn name(&self, id: usize) -> &str {
        &self.members[id].name
    }

    pub fn class(&self, id: usize) -> EgressClass {
        self.members[id].class
    }

    /// 按名字找出口编号（用户指定出口时，结果仍记进统计）。
    pub fn position(&self, name: &str) -> Option<usize> {
        self.members.iter().position(|m| m.name == name)
    }

    /// 给一次请求选出口。`cookies`：请求带 Cookie（会话固定在同一出口）；
    /// `capacity`：全局并发上限，用来把在途数换算成利用率。
    pub fn choose(&self, site: Site<'_>, cookies: bool, capacity: usize) -> Pick<'_> {
        let (id, sticky) = self
            .stats
            .pick(site, cookies, |post, rng| {
                self.argmin(post, rng, capacity, |_| true)
            })
            .expect("egress pool has members");
        self.lease(id, sticky, false)
    }

    /// 换出口重试：不再用 `prev` 的出口；`other_class` 时有不同类别的出口就只在它们里选。
    /// 带 Cookie 的、已经是重试的请求不换（返回 `None`）。
    pub fn choose_retry(
        &self,
        site: Site<'_>,
        prev: &Pick<'_>,
        other_class: bool,
        capacity: usize,
    ) -> Option<Pick<'_>> {
        if prev.sticky || prev.retry {
            return None;
        }
        let avoid = prev.id;
        let avoid_class = self.members[avoid].class;
        let differs = other_class && self.members.iter().any(|m| m.class != avoid_class);
        let (id, _) = self.stats.pick(site, false, |post, rng| {
            self.argmin(post, rng, capacity, |e| {
                e != avoid && (!differs || self.members[e].class != avoid_class)
            })
        })?;
        Some(self.lease(id, false, true))
    }

    /// 用户指定的出口：结果照样记进统计，但不会换出口（按重试处理）。
    pub fn pin(&self, id: usize) -> Pick<'_> {
        self.lease(id, false, true)
    }

    fn lease(&self, id: usize, sticky: bool, retry: bool) -> Pick<'_> {
        self.members[id].inflight.0.fetch_add(1, Ordering::Relaxed);
        Pick {
            pool: self,
            id,
            sticky,
            retry,
        }
    }

    /// Thompson 抽样加影子价格取最小；`allowed` 过滤候选。饱和出口的价格是无穷，
    /// 只有所有候选都饱和时才会选到（取第一个）。
    fn argmin(
        &self,
        posterior: &dyn Fn(usize) -> Posterior,
        rng: &mut Rng,
        capacity: usize,
        allowed: impl Fn(usize) -> bool,
    ) -> Option<usize> {
        // 出口自身的连接上限未知：利用率按全局并发在各出口间的均分算
        let share = (capacity as f64 / self.members.len() as f64).max(1.0);
        let mut best: Option<(f64, usize)> = None;
        for e in (0..self.members.len()).filter(|&e| allowed(e)) {
            let rho = self.members[e].inflight.0.load(Ordering::Relaxed) as f64 / share;
            let score = DENIAL_COST * posterior(e).sample(rng) + shadow_price(rho, 1.0);
            if best.is_none_or(|(s, _)| score < s) {
                best = Some((score, e));
            }
        }
        best.map(|(_, e)| e)
    }

    /// 报告 `pick` 这次的结果，按拦截分类给出处理办法。
    pub fn report(&self, site: Site<'_>, pick: &Pick<'_>, outcome: Outcome) -> Action {
        let Some(denied) = outcome.denial() else {
            return if outcome == Outcome::Throttled {
                Action::Backoff
            } else {
                Action::Keep
            };
        };
        let switchable = !pick.sticky && !pick.retry && self.members.len() > 1;
        self.stats
            .observe(site, pick.id, denied, |cells| match outcome {
                _ if !denied => Action::Keep,
                Outcome::Challenge if self.site_wide(cells) => Action::SiteWide,
                Outcome::Challenge if switchable => Action::Switch { other_class: true },
                _ if switchable && self.reputation(cells, pick.id) => {
                    Action::Switch { other_class: false }
                }
                _ if self.site_wide(cells) => Action::SiteWide,
                _ => Action::Keep,
            })
    }

    /// IP 信誉：（站点，出口）的被拒率显著高于该站点在其他出口上的被拒率。
    ///
    /// 两个比例之差的检验用 Agresti–Caffo 的"每组加一次成功一次失败"
    /// （A. Agresti, B. Caffo, The American Statistician 54(4), 2000）：`p̃ = (k+1)/(n+2)`、
    /// `Var = p̃(1−p̃)/(n+2)`，与 `Beta(1,1)` 先验的后验均值一致；差值超过 `z(1−δ)·SE` 才算显著（单侧）。
    /// 这里只用站点自己的计数，不用收缩后的后验：收缩会把单站的封禁冲淡，而检验问的正是这个站点。
    fn reputation(&self, cells: &[BetaCounts], e: usize) -> bool {
        let mut others = BetaCounts::default();
        for (i, c) in cells.iter().enumerate() {
            if i != e {
                others.success += c.success;
                others.failure += c.failure;
            }
        }
        if others.trials() <= 0.0 {
            return false;
        }
        let (p1, v1) = adjusted(&cells[e]);
        let (p2, v2) = adjusted(&others);
        p1 - p2 > self.delta.z() * (v1 + v2).sqrt()
    }

    /// 全站拒绝：至少两个出口试过，每个试过的出口的被拒率 Wilson 下界都超过 ½
    /// （"多数请求被拒"在 δ 水平上成立；Wilson, JASA 22, 1927）。
    fn site_wide(&self, cells: &[BetaCounts]) -> bool {
        let tried: Vec<&BetaCounts> = cells.iter().filter(|c| c.trials() > 0.0).collect();
        tried.len() >= 2
            && tried
                .iter()
                .all(|c| wilson(c.success, c.trials(), self.delta).0 > 0.5)
    }

    /// 某站点在某出口上的被拒率后验均值（诊断与测试用）。
    pub fn denial_mean(&self, site: Site<'_>, id: usize) -> f64 {
        self.stats.posterior_of(site, id).mean()
    }
}

/// Agresti–Caffo 调整后的比例和方差。
fn adjusted(c: &BetaCounts) -> (f64, f64) {
    let n = c.trials() + 2.0;
    let p = (c.success + 1.0) / n;
    (p, p * (1.0 - p) / n)
}

/// 选中的出口。持有期间计入该出口的在途数，丢弃时归还。
pub struct Pick<'p> {
    pool: &'p EgressPool,
    id: usize,
    sticky: bool,
    retry: bool,
}

impl Pick<'_> {
    pub fn id(&self) -> usize {
        self.id
    }

    pub fn name(&self) -> &str {
        self.pool.name(self.id)
    }

    pub fn class(&self) -> EgressClass {
        self.pool.class(self.id)
    }

    /// Cookie 会话固定的出口。
    pub fn sticky(&self) -> bool {
        self.sticky
    }

    /// 这是换出口后的重试（或用户指定的出口）。
    pub fn is_retry(&self) -> bool {
        self.retry
    }
}

impl Drop for Pick<'_> {
    fn drop(&mut self) {
        self.pool.members[self.id]
            .inflight
            .0
            .fetch_sub(1, Ordering::Relaxed);
    }
}

impl std::fmt::Debug for Pick<'_> {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Pick")
            .field("egress", &self.name())
            .field("sticky", &self.sticky)
            .field("retry", &self.retry)
            .finish()
    }
}

impl std::fmt::Debug for EgressPool {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_list()
            .entries(self.members.iter().map(|m| (&m.name, m.class)))
            .finish()
    }
}
