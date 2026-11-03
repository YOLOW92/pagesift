//! 统计汇聚：工人把学到的统计增量报给网关，网关合并（`pagesift-cluster` 的 `StatsHub`，合并与顺序无关、
//! 按序号去重）后存进账本、随日志复制到温备；工人第一次碰到一个路径族时向网关要先验，站点换了工人，
//! 新工人不用从零学。
//!
//! 接上的统计：
//! - 渲染决策的路径族表（`engine/planner.rs` 的 Beta 计数，渲染与换内核两类族）：键是
//!   `Scope::Family(族的键)`，指标 [`PLANNER_METRIC`]；
//! - 起始内核表（`learn.rs`，每站点每个内核"结果是否足够"的计数）：键是 `Scope::Site(站点的键)`，
//!   指标是内核序号 + [`ENGINE_METRIC_BASE`]。站点的键用 BLAKE3（各工人一致），不用本地表的随机哈希。
//!
//! 什么时候报（DESIGN 里"信息量足以让某个决策翻转"）：
//! - 网关还没有这个族的数据时，第一份观测就报：它让网关上的决策从冷启动的启发式变成按后验决策；
//! - 之后攒着，攒下的观测让后验均值相对已报部分的变化超过 `z(1−δ)·SE` 时再报
//!   （`pagesift_stat::shrink::significant`，SE 取含新观测的后验标准差），不显著的变化不值一次发送。

use std::collections::{HashMap, HashSet};
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex, RwLock};

use pagesift_cluster::{Coordinator, Ledger, RemoteCoordinator, Scope, StatKind, StatsDelta};
use pagesift_stat::Delta;
use pagesift_stat::beta::{BetaCounts, Prior};
use pagesift_stat::table::{Key, MetricId, Mode, StatTable};

use crate::Scraper;
use crate::engine::planner::{fallback_family, family_key, render_family};

/// [协议] 统计种类编号：渲染决策的路径族表
pub const PLANNER: StatKind = StatKind(1);
/// [协议] 表里的指标编号：渲染（或换内核）有没有增益的 Beta 计数
pub const PLANNER_METRIC: MetricId = MetricId(1);

type Remote = RemoteCoordinator<Vec<u8>>;

/// [协议] 统计种类编号：起始内核表
pub const ENGINE: StatKind = StatKind(2);
/// [协议] 起始内核表的指标编号起点：内核 i 的指标是 `ENGINE_METRIC_BASE + i`
pub const ENGINE_METRIC_BASE: u32 = 1;

/// 网关（和温备，跟随之前）登记要合并的统计种类。
pub fn register_stats(ledger: &Ledger) {
    ledger.register_stat::<BetaCounts>(PLANNER);
    ledger.register_stat::<BetaCounts>(ENGINE);
}

fn site_key(host: &str) -> u64 {
    family_key(&format!("site\u{0}{host}"))
}

#[derive(Default)]
struct Family {
    pending: BetaCounts,
    sent: BetaCounts,
    /// 网关上已经有这个族的数据（报过，或拿到过先验）。
    at_gateway: bool,
}

/// 工人一侧：观测攒成增量上报，路径族的先验按需取回。
pub(crate) struct Stats {
    families: Mutex<HashMap<(StatKind, Key), Family>>,
    fetched: Mutex<HashSet<(StatKind, u64)>>,
    link: RwLock<Option<Arc<Remote>>>,
    /// 增量的序号（按种类递增；来源是工人 id + 启动编号，重启后从头编号不会与旧的混淆）。
    seq: AtomicU64,
    delta: Delta,
}

fn lock<T>(m: &Mutex<T>) -> std::sync::MutexGuard<'_, T> {
    m.lock().unwrap_or_else(|e| e.into_inner())
}

impl Stats {
    /// 接到 `scraper` 的渲染决策表上（只接一次）。
    pub fn attach(scraper: &Scraper) -> Arc<Self> {
        let s = Arc::new(Self {
            families: Mutex::default(),
            fetched: Mutex::default(),
            link: RwLock::new(None),
            seq: AtomicU64::new(0),
            delta: Delta::DEFAULT,
        });
        let me = s.clone();
        scraper.planner().observe_with(Arc::new(move |key, gain| {
            let k = Key {
                scope: Scope::Family(key),
                metric: PLANNER_METRIC,
            };
            me.observe(PLANNER, k, gain)
        }));
        let me = s.clone();
        scraper
            .learner()
            .observe_with(Arc::new(move |host, engine, sufficient| {
                let k = Key {
                    scope: Scope::Site(site_key(host)),
                    metric: MetricId(ENGINE_METRIC_BASE + engine as u32),
                };
                me.observe(ENGINE, k, sufficient)
            }));
        s
    }

    /// 当前连着的网关（断线重连后换新的）。
    pub fn connect(&self, rc: Option<Arc<Remote>>) {
        *self.link.write().unwrap_or_else(|e| e.into_inner()) = rc;
    }

    fn observe(&self, kind: StatKind, key: Key, gain: bool) {
        let send = {
            let mut m = lock(&self.families);
            let f = m.entry((kind, key)).or_default();
            f.pending.observe(gain);
            let due = !f.at_gateway || {
                let mut all = f.sent.clone();
                pagesift_stat::Stat::merge(&mut all, &f.pending);
                let (before, after) = (
                    f.sent.posterior(Prior::UNIFORM),
                    all.posterior(Prior::UNIFORM),
                );
                pagesift_stat::shrink::significant(
                    before.mean(),
                    after.mean(),
                    after.variance().sqrt(),
                    self.delta,
                )
            };
            due.then(|| std::mem::take(&mut f.pending))
        };
        let Some(counts) = send else { return };
        let rc = self.link.read().unwrap_or_else(|e| e.into_inner()).clone();
        let Some(rc) = rc.filter(|r| !r.is_closed()) else {
            // 没有连接：放回去，下一次观测时再报
            pagesift_stat::Stat::merge(
                &mut lock(&self.families).entry((kind, key)).or_default().pending,
                &counts,
            );
            return;
        };
        let mut t = StatTable::<BetaCounts>::new(Mode::Live);
        t.update(key, |slot| *slot = counts.clone());
        rc.publish_stats(StatsDelta {
            kind,
            seq: self.seq.fetch_add(1, Ordering::Relaxed),
            table: t.encode(),
        });
        let mut m = lock(&self.families);
        let f = m.entry((kind, key)).or_default();
        pagesift_stat::Stat::merge(&mut f.sent, &counts);
        f.at_gateway = true;
    }

    /// 执行 `url` 之前：这个路径族（渲染的两类族、换内核的各个族）还没向网关要过先验的，现在要，
    /// 导入渲染决策表。每个族每个工人只要一次。
    pub async fn prime(&self, scraper: &Scraper, rc: &Remote, url: &str) {
        let mut keys: Vec<u64> = [true, false]
            .into_iter()
            .map(|js| family_key(&render_family(url, js)))
            .collect();
        keys.extend((0..scraper.engine_count()).map(|i| family_key(&fallback_family(url, i))));
        keys.retain(|k| lock(&self.fetched).insert((PLANNER, *k)));
        for k in keys {
            let Ok(p) = rc.refresh_priors(Scope::Family(k)).await else {
                // 没拿到：下次再要
                lock(&self.fetched).remove(&(PLANNER, k));
                continue;
            };
            for (kind, table) in &p.tables {
                if *kind != PLANNER {
                    continue;
                }
                let Ok(t) = StatTable::<BetaCounts>::decode(table, Mode::Live) else {
                    continue;
                };
                let key = Key {
                    scope: Scope::Family(k),
                    metric: PLANNER_METRIC,
                };
                if let Some(c) = t.get(&key).filter(|c| c.trials() > 0.0) {
                    scraper.planner().import(k, c.clone());
                    lock(&self.families)
                        .entry((PLANNER, key))
                        .or_default()
                        .at_gateway = true;
                }
            }
        }
        // 起始内核表：每个站点要一次
        let Some(host) = url::Url::parse(url)
            .ok()
            .and_then(|u| u.host_str().map(str::to_owned))
        else {
            return;
        };
        let site = site_key(&host);
        if !lock(&self.fetched).insert((ENGINE, site)) {
            return;
        }
        let Ok(p) = rc.refresh_priors(Scope::Site(site)).await else {
            lock(&self.fetched).remove(&(ENGINE, site));
            return;
        };
        for (kind, table) in &p.tables {
            if *kind != ENGINE {
                continue;
            }
            let Ok(t) = StatTable::<BetaCounts>::decode(table, Mode::Live) else {
                continue;
            };
            for engine in 0..scraper.engine_count() {
                let key = Key {
                    scope: Scope::Site(site),
                    metric: MetricId(ENGINE_METRIC_BASE + engine as u32),
                };
                if let Some(c) = t.get(&key).filter(|c| c.trials() > 0.0) {
                    scraper
                        .learner()
                        .import(&host, engine, c.success, c.failure);
                    lock(&self.families)
                        .entry((ENGINE, key))
                        .or_default()
                        .at_gateway = true;
                }
            }
        }
    }
}
