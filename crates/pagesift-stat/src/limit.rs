//! 并发控制律：每站点的延迟梯度控制与全局在途数的 USL 目标。
//!
//! 两者都是纯计算（不读时钟、不做 I/O），由调用方在完成事件上喂样本：
//! - [`GradientLimit`]：`limit ← limit·(RTT_min/RTT_now) + √limit`。出处：Netflix concurrency-limits 的
//!   Gradient 限制器（`GradientLimit`/`Gradient2Limit`），其思想来自 TCP Vegas
//!   （Brakmo、Peterson，IEEE JSAC 13(8), 1995：用"期望吞吐 − 实际吞吐"即 BaseRTT 与当前 RTT 之比判断排队）。
//! - [`UslLimit`]：目标在途数取通用可扩展性定律峰值 `N*` 的 `1−δ` 置信下界
//!   （Gunther, Guerrilla Capacity Planning, 2007, 第 4–6 章；拟合见 [`crate::usl`]）。
//!
//! 稳定性：只在显著时更新（两样本检验或置信界给出显著性），减小时用精度加权步长
//! `K = P/(P+R)`（Kalman 1960）。

use crate::moments::Moments;
use crate::special::t_quantile;
use crate::usl::{Usl, UslFit};
use crate::{Delta, Stat};

/// 每站点并发的延迟梯度控制器。
///
/// 样本是一次请求从拿到名额到结束的时长（秒），在 `ln RTT` 上统计（服务时间右偏，对数尺度上近似正态）。
///
/// 按并发档位分别统计：名额用满的轮记进它的整数并发档（`levels[k]`），没用满的轮（需求受限，
/// 并发由调用方决定）记进"空闲档"。每档都是累积的、来者不拒（不挑样本，没有偏差，也不会漂移）。
/// 一"轮"是 `⌈limit⌉` 个完成（每个在途名额周转一次，Vegas 每个 RTT 调一次窗口的对应物），每轮调一次：
/// - 名额用满的轮：当前档与它下面最近的、有数据的档（没有就用空闲档）做单侧两样本 t 检验。
///   多加的那些名额让延迟显著升高（有排队）时只收不加：`limit ← min(limit, limit·RTT_min/RTT_now + √limit)`，
///   `RTT_min` 取没有排队时（空闲档或最低档），`RTT_now` 取当前档（几何均值）；不显著（平稳）时
///   比值取 1，`limit ← limit + √limit`。某档本轮比它自己的历史显著快时，丢掉这档的旧历史（站点恢复）。
///   保护：显著升高、且到了没有排队时的 1.5 倍（0.81 的阈值，[`GUARD_RATIO`]）时，至少退回下一档。
/// - 没用满的轮：与空闲档的历史比；显著升高（同样的门槛）时按梯度律收缩（站点变慢了），否则不变
///   ——没有证据说明更多并发有用，同 TCP 只在窗口受限时增长（RFC 7661）。
///
/// 和原文的差别：Vegas 的 BaseRTT 是全程最小值，Gradient2 用长期 EWMA 当基线；前者在重尾服务时间里
/// 随样本数一直下降（比值偏小、并发被压低），后者会被我们自己排出来的队慢慢抬高（棘轮）。这里直接
/// 测"延迟对并发的梯度"：同一时期的相邻档位之差，只反映多加名额造成的排队，站点整体变快变慢
/// 不影响档间差。没有用逐样本 CUSUM 切段（每样本误报率 δ，平均 1/δ 个样本就切一段，太短）。
#[derive(Debug, Clone)]
pub struct GradientLimit {
    limit: f64,
    delta: Delta,
    /// `levels[k]`：名额用满、整数并发为 `k` 的轮的 `ln RTT`。
    levels: Vec<Moments>,
    /// 名额没用满的轮的 `ln RTT`。
    free: Moments,
    /// 本轮的 `ln RTT`。
    round: Moments,
    /// 本轮完成数（含没有有效时长的完成）。
    done: u64,
    saturated: bool,
    /// 本轮收到过过载信号（本轮不增）。
    overloaded: bool,
    /// 过载后还要忽略几个完成里的过载信号：减的那一刻在途的请求（`⌈减前的 limit⌉` 个）
    /// 属于同一次越界，它们一起收到 429 只算一次。
    overload_guard: u64,
    /// 最近一轮显著升高（超过分辨率）。
    elevated: bool,
}

impl GradientLimit {
    /// `prior` 是冷启动并发（先验包 v0：0.81 的现值），没有样本时控制器就停在这里。
    pub fn new(prior: f64, delta: Delta) -> Self {
        Self {
            limit: prior.max(1.0),
            delta,
            levels: Vec::new(),
            free: Moments::default(),
            round: Moments::default(),
            done: 0,
            saturated: false,
            overloaded: false,
            overload_guard: 0,
            elevated: false,
        }
    }

    /// 当前并发（连续值）；整数名额用 [`GradientLimit::permits`]。
    pub fn limit(&self) -> f64 {
        self.limit
    }

    /// 整数名额：`⌊limit⌋`，至少 1。
    pub fn permits(&self) -> usize {
        (self.limit.floor() as usize).max(1)
    }

    /// 没有排队时的 `ln RTT`：空闲档（有两个以上样本时），否则最低的有数据的并发档。
    pub fn baseline(&self) -> &Moments {
        if self.free.count() >= 2 {
            return &self.free;
        }
        self.levels
            .iter()
            .find(|m| m.count() >= 2)
            .unwrap_or(&self.free)
    }

    /// 最近一轮站点在排队：名额用满的轮比下一档显著慢，或没用满的轮比空闲档显著慢且超过决策分辨率。
    pub fn rising(&self) -> bool {
        self.elevated
    }

    /// 新请求时长超过它就算"慢"：没有排队时 `ln RTT` 的 `1−δ` 单侧预测上界
    /// `exp(μ + t(1−δ, n−1)·s·√(1+1/n))`（正态样本对一次新观测的预测区间）。
    /// 少于 2 个样本时没有界（`None`），调用方用自己的上限。
    pub fn slow_after(&self) -> Option<f64> {
        let base = self.baseline();
        let n = base.count();
        let s = base.std_dev();
        if n < 2 || !s.is_finite() {
            return None;
        }
        let nf = n as f64;
        let t = t_quantile(1.0 - self.delta.get(), nf - 1.0);
        Some((base.mean() + t * s * (1.0 + 1.0 / nf).sqrt()).exp())
    }

    /// 站点明确表示过载（429/503）：减一个名额 `limit ← max(1, limit − 1)`（同 0.81，过载时的行为不变）。
    /// 减的那一刻在途的请求（`⌈limit⌉` 个）属于同一次越界，它们的过载信号不再减。返回是否真的减了。
    pub fn overload(&mut self) -> bool {
        self.overloaded = true;
        if self.overload_guard > 0 {
            return false;
        }
        self.overload_guard = self.limit.ceil() as u64;
        self.limit = (self.limit - 1.0).max(1.0);
        true
    }

    /// 记一次完成：`rtt` 秒（非有限或非正时只计数）；`saturated` 表示这次拿名额时名额已用满。
    /// `cap` 是上限（礼貌策略、Crawl-delay 等）。并发的整数名额变了时返回新值。
    pub fn complete(&mut self, rtt: f64, saturated: bool, cap: f64) -> Option<usize> {
        let before = self.permits();
        let cap = cap.max(1.0);
        if rtt.is_finite() && rtt > 0.0 {
            self.round.push(rtt.ln());
        }
        self.saturated |= saturated;
        self.done += 1;
        self.overload_guard = self.overload_guard.saturating_sub(1);
        if (self.done as f64) >= self.limit.ceil() {
            self.end_round();
        }
        self.limit = self.limit.clamp(1.0, cap);
        (self.permits() != before).then(|| self.permits())
    }

    fn end_round(&mut self) {
        let round = std::mem::take(&mut self.round);
        let saturated = self.saturated;
        let grow = saturated && !self.overloaded;
        let k = self.permits();
        self.done = 0;
        self.saturated = false;
        self.overloaded = false;
        if round.count() == 0 {
            // 本轮没有可用的时长（都被取消或都是限流响应）：没有升高的证据
            self.elevated = false;
            if grow {
                self.limit += self.limit.sqrt();
            }
            return;
        }
        let res = resolution(self.limit);
        if !saturated {
            // 需求受限：和空闲档的历史比，站点整体变慢时按梯度律收缩；之后照样并入（不挑样本）
            let cmp = self.compare(&round, &self.free);
            self.elevated = cmp.is_some_and(|(d, c)| d > c.max(res));
            self.free.merge(&round);
            if let Some((d, _)) = cmp.filter(|_| self.elevated) {
                let mut next = self.law(d);
                if d >= GUARD_RATIO.ln() {
                    // 保护：升到 0.81 的阈值就至少减一档
                    next = next.min((k.saturating_sub(1)).max(1) as f64);
                }
                self.limit = self.limit.min(next);
            }
            return;
        }
        if self.levels.len() <= k {
            self.levels.resize_with(k + 1, Moments::default);
        }
        // 这一档变快了（本轮显著低于它自己的历史）：站点恢复或换了状态，旧历史作废（变化点，DESIGN §5.5）
        let faster = self
            .compare(&round, &self.levels[k])
            .is_some_and(|(d, c)| d < -c);
        if faster {
            self.levels[k] = round;
        } else {
            self.levels[k].merge(&round);
        }
        // 参照：下面最近的、有两个以上样本的档；都没有就用空闲档
        let below = self.levels[..k]
            .iter()
            .rev()
            .find(|m| m.count() >= 2)
            .unwrap_or(&self.free);
        // 多加的名额让延迟显著升高（相邻档 t 检验）：有排队，梯度律按相对没有排队时的总升高定新值
        // （同一时期、同一站点的档间差，不漂移）。否则比值取 1。
        let queued = self
            .compare(&self.levels[k], below)
            .is_some_and(|(d, c)| d > c);
        let total = (self.levels[k].mean() - self.baseline().mean()).max(0.0);
        // 名额用满时多加的名额显著带来排队：负载在把站点压慢，调用方据此不再额外加负载（停放慢请求）。
        // 只看名额用满的轮：没用满时的毫秒级抖动不是我们的并发造成的
        self.elevated = queued;
        if queued && total >= GUARD_RATIO.ln() {
            // 保护：延迟升到没有排队时的 1.5 倍（0.81 的阈值），直接退回下一档，不等梯度律慢慢收
            self.limit = self.law(total).min((k.saturating_sub(1)).max(1) as f64);
        } else if queued {
            // 有排队：只按梯度律收，不加（平稳时才往上加）
            self.limit = self.limit.min(self.law(total));
        } else if grow {
            self.limit += self.limit.sqrt();
        }
    }

    /// 梯度律 `limit·RTT_min/RTT_now + √limit`，`d = ln(RTT_now/RTT_min)`。
    fn law(&self, d: f64) -> f64 {
        self.limit * (-d).exp() + self.limit.sqrt()
    }

    /// `a` 相对 `b` 的 `ln RTT` 均值差和单侧临界值 `t(1−δ)·SE`（Welch：两边各用自己的方差；
    /// `a` 只有一个样本时借用 `b` 的方差）。`b` 不到两个样本时信息不足（`None`）。
    fn compare(&self, a: &Moments, b: &Moments) -> Option<(f64, f64)> {
        let (na, nb) = (a.count() as f64, b.count() as f64);
        let vb = b.variance();
        if na < 1.0 || nb < 2.0 || !vb.is_finite() {
            return None;
        }
        let va = if na >= 2.0 { a.variance() } else { vb };
        let se = (va / na + vb / nb).sqrt();
        let t = t_quantile(1.0 - self.delta.get(), nb - 1.0);
        Some((a.mean() - b.mean(), t * se))
    }
}

/// [策略] 先验包 v0：0.81 热连接控制器的阈值——服务时间超过基线 1.5 倍就减一档。梯度律的平衡点允许
/// 约 `√limit` 个请求排队，在"延迟随负载上升"的源站上比 0.81 宽松；这道保护让过载时的行为不比 0.81 松。
const GUARD_RATIO: f64 = 1.5;

/// [推导] 决策分辨率 `μ_Δ = ln(1 + 1/√limit)`：梯度律的平衡点是 `RTT_now/RTT_min = 1/(1−1/√limit)`，
/// 比 `1 + 1/√limit` 小的上升不会让 `limit·RTT_min/RTT_now + √limit` 低于 `limit`。
fn resolution(limit: f64) -> f64 {
    (1.0 + 1.0 / limit.max(1.0).sqrt()).ln()
}

/// 全局在途数的 USL 控制器。
///
/// 调用方每攒够一个观测窗（建议 `⌈limit⌉` 个完成：在途集合整体周转一次，Little 定律 `L = λW`）调用一次
/// [`UslLimit::window`]，给出窗内平均在途数 `N̄ = Σ W_i / 时长`（Little 定律）和吞吐 `X = 完成数 / 时长`。
///
/// 决策（每窗一次）：
/// - 拟合出有限的峰值、且 `N*` 的 `1−δ` 上界仍低于当前目标（显著越过峰值）：向 `N*` 的 `1−δ` 下界移动，
///   步长 `K = P/(P+R)`，`R` 是峰值估计的方差（delta 方法，见 [`crate::usl`]），`P` 是当前目标的方差；
/// - 下界高于当前目标、或数据里还看不到回落（`κ ≤ 0`、点不够、下界被截到 1 而无法给出标准误）：
///   只在本窗名额用满时向上探测一步 `√limit`（同 [`GradientLimit`] 的加性项），且不超过 `room`；
///   探测的步长并入 `P`（过程噪声），即"在 `N*` 两侧小幅扰动"；
/// - 其余情况不动。
#[derive(Debug, Clone)]
pub struct UslLimit {
    usl: Usl,
    points: u64,
    limit: f64,
    /// 当前目标作为估计量的方差；冷启动时没有信息（无穷大），第一次减小时取 `K = 1/2`
    /// （先验强度等于一份观测）。
    var: f64,
    delta: Delta,
}

impl UslLimit {
    pub fn new(prior: f64, delta: Delta) -> Self {
        Self {
            usl: Usl::default(),
            points: 0,
            limit: prior.max(1.0),
            var: f64::INFINITY,
            delta,
        }
    }

    pub fn limit(&self) -> f64 {
        self.limit
    }

    pub fn permits(&self) -> usize {
        (self.limit.floor() as usize).max(1)
    }

    /// 当前 USL 拟合（诊断用）。
    pub fn fit(&self) -> Option<UslFit> {
        self.usl.fit(self.delta)
    }

    /// 观测点数。
    pub fn points(&self) -> u64 {
        self.points
    }

    /// 记一个观测窗；`room` 是向上探测允许的最大增量（用户上限、内存预算、文件描述符余量）。
    /// 返回新的整数名额。
    pub fn window(&mut self, n_bar: f64, x: f64, saturated: bool, room: f64) -> usize {
        if n_bar.is_finite() && x.is_finite() && n_bar > 0.0 && x > 0.0 {
            self.usl.push(n_bar, x);
            self.points += 1;
        }
        let room = room.max(0.0);
        let fit = self.usl.fit(self.delta).filter(|f| f.peak.is_finite());
        // [推导] 残差自由度 n − 3：线性化 N/X = a + bN + cN² 有三个系数
        let dof = self.points.saturating_sub(3) as f64;
        match fit {
            Some(f) if f.peak_lower > 1.0 && dof > 0.0 => {
                let t = t_quantile(1.0 - self.delta.get(), dof);
                let se = (f.peak - f.peak_lower) / t;
                let upper = f.peak + t * se;
                if upper < self.limit {
                    let r = se * se;
                    let k = if self.var.is_finite() {
                        self.var / (self.var + r)
                    } else {
                        0.5
                    };
                    let p = if self.var.is_finite() { self.var } else { r };
                    self.limit += k * (f.peak_lower - self.limit);
                    self.var = (1.0 - k) * p;
                } else if f.peak_lower > self.limit && saturated {
                    self.probe((f.peak_lower - self.limit).min(room));
                }
            }
            _ if saturated => self.probe(room),
            _ => {}
        }
        self.limit = self.limit.max(1.0);
        self.permits()
    }

    fn probe(&mut self, room: f64) {
        let step = self.limit.sqrt().min(room);
        if step > 0.0 {
            self.limit += step;
            self.var = if self.var.is_finite() {
                self.var + step * step
            } else {
                self.var
            };
        }
    }
}

#[cfg(test)]
#[path = "limit_tests.rs"]
mod tests;
