//! 租约时长：`ttl = q(1−δ)(该类任务耗时) + q(1−δ)(往返时间)`。
//!
//! 耗时的分布按任务类别各记一份 DDSketch。`q(1−δ)` 取它的无分布置信上界（W. J. Conover,
//! Practical Nonparametric Statistics, 3rd ed., 1999, §3.2）：秩 `n·p + z(1−δ)·√(n·p(1−p))`
//! 处的次序统计量。秩越出样本时没有上界——样本还不够回答这个分位数（信息不足），
//! 此时用任务自带的总预算 `timeout` 作冷启动值：一个任务不应该比它自己的预算跑得更久。
//!
//! 分片各自累计样本，攒够 `n* = ⌈ln δ / ln(1−δ)⌉` 个再并入全局表（低频汇总，避免每次提交都去拿
//! 全局锁）：`n*` 是"样本最大值以 `1−δ` 置信度不低于 `q(1−δ)`"所需的最少样本数，
//! 即 `P(max < q(1−δ)) = (1−δ)^n ≤ δ`。
//!
//! 与原文的差别：被重新分配的任务（租约到期）是右截尾观测，这里没有计入，所以估计偏向已完成的任务；
//! 到期的任务由续约兜住（持有者在 [`renew_after`] 时续约）。

use std::collections::HashMap;
use std::sync::RwLock;
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::Duration;

use pagesift_stat::hazard::quantile_bounds;
use pagesift_stat::sketch::Sketch;
use pagesift_stat::{Delta, Stat};

/// `q(1−δ)` 的单侧 `1−δ` 置信上界（`pagesift_stat::hazard::quantile_bounds`）；样本不够时为 `None`。
pub(crate) fn upper(s: &Sketch, delta: Delta) -> Option<f64> {
    quantile_bounds(s, 1.0 - delta.get(), delta).2
}

/// 往返时间等"越大越危险"的量的 `q(1−δ)` 上界（秒）；样本不够给出置信上界时用见过的最大值
/// （冷启动，最大值是手里唯一不低估的估计）。
pub(crate) fn upper_or_max(s: &Sketch, delta: Delta) -> f64 {
    upper(s, delta).unwrap_or_else(|| s.max().max(0.0))
}

/// 每次并入全局表前分片至少攒的样本数 `⌈ln δ / ln(1−δ)⌉`。
pub(crate) fn fold_after(delta: Delta) -> u64 {
    let d = delta.get();
    (d.ln() / (1.0 - d).ln()).ceil() as u64
}

/// 拿到租约后多久续约：续约请求要在到期前送达，而租约发出和续约送达各要一个单程，
/// 合计不超过一个往返；往返用 `q(1−δ)` 上界 `rtt_hi`，所以在 `ttl − 2·rtt_hi` 时续约
/// （工人一侧的截止时刻已扣掉一个 `rtt_hi`，见 `remote`）。
/// 进程内（`rtt_hi = 0`）续约只是一次内存操作，但异步调度的延迟没有上界，取租约的一半，
/// 两边留同样的余量。
pub fn renew_after(ttl: Duration, rtt_hi: Duration) -> Duration {
    if rtt_hi.is_zero() {
        ttl / 2
    } else {
        ttl.saturating_sub(rtt_hi * 2)
    }
}

/// 全局的耗时表：类别 → 草图（秒）。写锁只在分片并入时拿。
#[derive(Debug, Default)]
pub(crate) struct Durations {
    global: RwLock<HashMap<u32, Sketch>>,
    /// 每次并入加一；分片据此判断缓存是否过期（只读的原子量，写入频率等于并入频率）。
    generation: AtomicU64,
}

impl Durations {
    /// 并入一个分片攒下的样本，返回这些类别新的上界。
    pub(crate) fn fold(
        &self,
        local: &mut HashMap<u32, Sketch>,
        delta: Delta,
    ) -> Vec<(u32, Option<f64>)> {
        let mut g = self.global.write().unwrap_or_else(|e| e.into_inner());
        self.generation.fetch_add(1, Ordering::Release);
        local
            .drain()
            .map(|(class, s)| {
                let e = g.entry(class).or_default();
                e.merge(&s);
                (class, upper(e, delta))
            })
            .collect()
    }

    /// 读全局上界（分片缓存里没有这个类别时才调用）。
    pub(crate) fn upper(&self, class: u32, delta: Delta) -> Option<f64> {
        let g = self.global.read().unwrap_or_else(|e| e.into_inner());
        g.get(&class).and_then(|s| upper(s, delta))
    }
}

/// 分片里的耗时累计与上界缓存。
#[derive(Debug, Default)]
pub(crate) struct LocalDurations {
    pending: HashMap<u32, Sketch>,
    pending_n: u64,
    cache: HashMap<u32, Option<f64>>,
    seen_generation: u64,
}

impl LocalDurations {
    pub(crate) fn observe(&mut self, class: u32, secs: f64, global: &Durations, delta: Delta) {
        self.pending.entry(class).or_default().push(secs);
        self.pending_n += 1;
        if self.pending_n >= fold_after(delta) {
            self.pending_n = 0;
            for (c, q) in global.fold(&mut self.pending, delta) {
                self.cache.insert(c, q);
            }
        }
    }

    /// 本类任务耗时的 `q(1−δ)` 上界；没有足够样本时用 `timeout`（冷启动）。
    pub(crate) fn task_hi(
        &mut self,
        class: u32,
        timeout: Duration,
        global: &Durations,
        delta: Delta,
    ) -> Duration {
        let generation = global.generation.load(Ordering::Acquire);
        if generation != self.seen_generation {
            self.seen_generation = generation;
            self.cache.clear();
        }
        let q = *self
            .cache
            .entry(class)
            .or_insert_with(|| global.upper(class, delta));
        q.map_or(timeout, Duration::from_secs_f64)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn upper_bound_needs_enough_samples_and_covers_quantile() {
        let delta = Delta::DEFAULT;
        assert_eq!(fold_after(delta), 59, "ln 0.05 / ln 0.95 = 58.4");
        let mut s = Sketch::default();
        for i in 1..=50 {
            s.push(i as f64);
        }
        assert!(upper(&s, delta).is_none(), "50 samples: no bound");
        let mut s = Sketch::default();
        for i in 1..=1000 {
            s.push(i as f64);
        }
        let hi = upper(&s, delta).unwrap();
        assert!(hi >= 950.0 * (1.0 - s.alpha()) && hi < 1000.0, "{hi}");
    }

    #[test]
    fn cold_start_uses_timeout_then_learns() {
        let delta = Delta::DEFAULT;
        let g = Durations::default();
        let mut l = LocalDurations::default();
        let t = Duration::from_secs(60);
        assert_eq!(l.task_hi(1, t, &g, delta), t);
        for _ in 0..200 {
            l.observe(1, 2.0, &g, delta);
        }
        let hi = l.task_hi(1, t, &g, delta);
        assert!((hi.as_secs_f64() - 2.0).abs() < 0.1, "{hi:?}");
        assert_eq!(
            renew_after(Duration::from_secs(10), Duration::from_secs(1)),
            Duration::from_secs(8)
        );
        assert_eq!(
            renew_after(Duration::from_secs(10), Duration::ZERO),
            Duration::from_secs(5)
        );
    }
}
