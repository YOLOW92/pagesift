//! 不分主机的网络统计：Happy Eyeballs 间隔、DNS 重试退避底数与重试次数。
//!
//! 样本按分片记（分片和主机表一样多，按主机哈希），某个分片攒够一批后才汇总全部分片
//! （低频）：窗口内估计够准（置信区间半宽 < δ·估计值）就发布到 128 字节对齐的原子量，
//! 并清空窗口重新攒（窗口均值 + 显著性，不用累计统计，192 vCPU 实测经验）。

use std::hash::BuildHasher;
use std::sync::Mutex;
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::Duration;

use pagesift_stat::sketch::Sketch;
use pagesift_stat::survival::Survival;
use pagesift_stat::{Delta, Stat};

use super::{Padded, rule};

/// [协议] RFC 8305 §5：Connection Attempt Delay 建议下限 100 ms
const HE_MIN: Duration = Duration::from_millis(100);
/// [协议] RFC 8305 §5：Connection Attempt Delay 建议上限 2 s
const HE_MAX: Duration = Duration::from_secs(2);
/// [策略] 先验包 v0：0.81 的现值，DNS 瞬时失败后的退避底数 25 ms
const DNS_BASE_PRIOR: Duration = Duration::from_millis(25);
/// [策略] 先验包 v0：0.81 的尾部阈值 2 s，作为"源站没有样本时多久算慢"的先验
const SLOW_PRIOR: Duration = Duration::from_secs(2);

#[derive(Default)]
struct Shard {
    /// 成功的 IPv6 TCP 建连耗时（秒）。
    ipv6: Sketch,
    ipv6_next: u64,
    /// 成功的 DNS 查询耗时（秒，不含缓存命中）。
    dns: Sketch,
    dns_next: u64,
    /// DNS 瞬时失败到恢复的时长（秒）；重试用完仍失败为截尾。
    recovery: Survival,
    recovery_next: u64,
    /// 所有源站的首字节耗时（秒）。
    ttfb: Sketch,
    ttfb_next: u64,
}

pub(crate) struct NetStats {
    shards: Box<[Padded<Mutex<Shard>>]>,
    hasher: std::collections::hash_map::RandomState,
    delta: Delta,
    he_ns: Padded<AtomicU64>,
    slow_ns: Padded<AtomicU64>,
    dns_base_ns: Padded<AtomicU64>,
    /// 学到的重试次数；`u64::MAX` 表示还没有，用配置值（先验）。
    dns_retries: Padded<AtomicU64>,
}

impl NetStats {
    pub(super) fn new(shards: usize, delta: Delta, he_prior: Duration) -> Self {
        Self {
            shards: (0..shards.max(1))
                .map(|_| Padded(Mutex::new(Shard::default())))
                .collect(),
            hasher: Default::default(),
            delta,
            he_ns: Padded(AtomicU64::new(he_prior.as_nanos() as u64)),
            slow_ns: Padded(AtomicU64::new(SLOW_PRIOR.as_nanos() as u64)),
            dns_base_ns: Padded(AtomicU64::new(DNS_BASE_PRIOR.as_nanos() as u64)),
            dns_retries: Padded(AtomicU64::new(u64::MAX)),
        }
    }

    fn shard(&self, host: &str) -> &Mutex<Shard> {
        &self.shards[(self.hasher.hash_one(host) as usize) % self.shards.len()].0
    }

    /// Happy Eyeballs 的尝试间隔（RFC 8305 §5）：先验是配置值（默认 250 ms，RFC 建议值）；
    /// 有足够样本后取 IPv6 建连耗时的 `q(1−δ)`——到这时 `1−δ` 的 IPv6 连接已经建好，
    /// 再等下去多半是这条路不通，该试下一个地址了。夹在 RFC 的 [100 ms, 2 s] 内。
    pub(crate) fn happy_eyeballs_delay(&self) -> Duration {
        Duration::from_nanos(self.he_ns.0.load(Ordering::Relaxed))
    }

    /// 还没有自己样本的源站上，请求多久算"慢"（每源站控制器据此停放慢请求、让出热连接）：
    /// 全部源站首字节耗时的 `q(1−δ)`——向上层收缩，先验 2 s（0.81 的尾部阈值）。
    pub(crate) fn slow_cap(&self) -> Duration {
        Duration::from_nanos(self.slow_ns.0.load(Ordering::Relaxed))
    }

    pub(crate) fn first_byte(&self, host: &str, elapsed: Duration) {
        let due = {
            let Ok(mut s) = self.shard(host).lock() else {
                return;
            };
            s.ttfb.push(elapsed.as_secs_f64());
            let n = s.ttfb.count();
            let due = n >= s.ttfb_next;
            if due {
                s.ttfb_next = rule::next_refit(n, self.delta);
            }
            due
        };
        if due {
            self.publish_quantile(
                |s| &mut s.ttfb,
                1.0 - self.delta.get(),
                &self.slow_ns.0,
                |q| q,
            );
        }
    }

    /// DNS 瞬时失败后的退避底数：解析器 RTT 的中位数（先验 25 ms）。
    pub(crate) fn dns_backoff_base(&self) -> Duration {
        Duration::from_nanos(self.dns_base_ns.0.load(Ordering::Relaxed))
    }

    /// DNS 瞬时失败的重试次数：恢复时长分布上有把握覆盖 `1−δ` 所需的次数；
    /// 还没有足够的恢复样本时用配置值 `prior`（0.81 的现值 1）。
    pub(crate) fn dns_retries(&self, prior: usize) -> usize {
        match self.dns_retries.0.load(Ordering::Relaxed) {
            u64::MAX => prior,
            n => n as usize,
        }
    }

    pub(crate) fn ipv6_connected(&self, host: &str, elapsed: Duration) {
        let due = {
            let Ok(mut s) = self.shard(host).lock() else {
                return;
            };
            s.ipv6.push(elapsed.as_secs_f64());
            let n = s.ipv6.count();
            let due = n >= s.ipv6_next;
            if due {
                s.ipv6_next = rule::next_refit(n, self.delta);
            }
            due
        };
        if due {
            self.publish_quantile(
                |s| &mut s.ipv6,
                1.0 - self.delta.get(),
                &self.he_ns.0,
                |q| q.clamp(HE_MIN, HE_MAX),
            );
        }
    }

    pub(crate) fn dns_answered(&self, host: &str, elapsed: Duration) {
        let due = {
            let Ok(mut s) = self.shard(host).lock() else {
                return;
            };
            s.dns.push(elapsed.as_secs_f64());
            let n = s.dns.count();
            let due = n >= s.dns_next;
            if due {
                s.dns_next = rule::next_refit(n, self.delta);
            }
            due
        };
        if due {
            self.publish_quantile(|s| &mut s.dns, 0.5, &self.dns_base_ns.0, |q| q);
        }
    }

    /// 一次 DNS 瞬时失败的结局：`recovered` 为真表示重试后在 `elapsed` 时成功。
    pub(crate) fn dns_recovery(&self, host: &str, elapsed: Duration, recovered: bool) {
        let due = {
            let Ok(mut s) = self.shard(host).lock() else {
                return;
            };
            let t = elapsed.as_secs_f64();
            if recovered {
                s.recovery.event(t);
            } else {
                s.recovery.censor(t);
            }
            let n = s.recovery.count();
            let due = n >= s.recovery_next;
            if due {
                s.recovery_next = rule::next_refit(n, self.delta);
            }
            due
        };
        if !due {
            return;
        }
        let mut all = Survival::default();
        for s in self.shards.iter() {
            if let Ok(s) = s.0.lock() {
                // 各分片都是默认 α 的草图，合并就是桶计数相加
                all.merge(&s.recovery);
            }
        }
        let Some(q) = all.confident_time_to(1.0 - self.delta.get(), 0.0, self.delta) else {
            return;
        };
        // 第 k 次重试前等 base·2^k（二进制指数退避）；取累计等待覆盖 q 的最小 k
        let base = self.dns_backoff_base().as_secs_f64();
        let mut waited = 0.0;
        let mut k = 0u64;
        while waited < q && base > 0.0 {
            k += 1;
            waited += base * 2f64.powi(k as i32);
        }
        self.dns_retries.0.store(k.max(1), Ordering::Relaxed);
        for s in self.shards.iter() {
            if let Ok(mut s) = s.0.lock() {
                s.recovery = Survival::default();
                s.recovery_next = 0;
            }
        }
    }

    /// 汇总各分片的窗口；够准就按显著性规则发布并清空窗口。
    fn publish_quantile(
        &self,
        field: impl Fn(&mut Shard) -> &mut Sketch,
        p: f64,
        out: &AtomicU64,
        bound: impl Fn(Duration) -> Duration,
    ) {
        let mut all = Sketch::default();
        for s in self.shards.iter() {
            if let Ok(mut s) = s.0.lock() {
                all.merge(field(&mut s));
            }
        }
        let Some((q, ci)) = rule::precise_quantile(&all, p, self.delta) else {
            return;
        };
        let current = Duration::from_nanos(out.load(Ordering::Relaxed)).as_secs_f64();
        let next = bound(Duration::from_secs_f64(rule::settle(current, q, ci)));
        out.store(next.as_nanos() as u64, Ordering::Relaxed);
        for s in self.shards.iter() {
            if let Ok(mut s) = s.0.lock() {
                *field(&mut s) = Sketch::default();
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn cold_values_equal_the_priors() {
        let s = NetStats::new(4, Delta::DEFAULT, Duration::from_millis(250));
        assert_eq!(s.happy_eyeballs_delay(), Duration::from_millis(250));
        assert_eq!(s.dns_backoff_base(), Duration::from_millis(25));
        assert_eq!(s.dns_retries(1), 1);
        assert_eq!(s.slow_cap(), SLOW_PRIOR);
    }

    #[test]
    fn happy_eyeballs_follows_ipv6_connect_tail_within_rfc_bounds() {
        let s = NetStats::new(4, Delta::DEFAULT, Duration::from_millis(250));
        // 少量样本：不动
        for i in 0..30 {
            s.ipv6_connected(&format!("h{i}"), Duration::from_millis(40));
        }
        assert_eq!(s.happy_eyeballs_delay(), Duration::from_millis(250));
        // 大量 20–60 ms 的建连：q(0.95) ≈ 58 ms，低于 RFC 下限 → 100 ms
        let mut rng = pagesift_stat::rng::Rng::seed(9);
        for i in 0..50_000 {
            let ms = 20.0 + 40.0 * rng.uniform();
            s.ipv6_connected(&format!("h{}", i % 97), Duration::from_secs_f64(ms / 1e3));
        }
        assert_eq!(s.happy_eyeballs_delay(), HE_MIN);
        // 慢的 IPv6（0.5–1.5 s）：q(0.95) ≈ 1.45 s
        for i in 0..50_000 {
            let ms = 500.0 + 1000.0 * rng.uniform();
            s.ipv6_connected(&format!("h{}", i % 97), Duration::from_secs_f64(ms / 1e3));
        }
        let d = s.happy_eyeballs_delay();
        assert!(
            d > Duration::from_millis(1300) && d < Duration::from_millis(1600),
            "{d:?}"
        );
    }

    #[test]
    fn dns_base_tracks_the_resolver_median() {
        let s = NetStats::new(4, Delta::DEFAULT, Duration::from_millis(250));
        let mut rng = pagesift_stat::rng::Rng::seed(4);
        for i in 0..50_000 {
            let ms = 2.0 + 6.0 * rng.uniform();
            s.dns_answered(&format!("h{}", i % 31), Duration::from_secs_f64(ms / 1e3));
        }
        let b = s.dns_backoff_base();
        assert!(
            b > Duration::from_micros(4500) && b < Duration::from_micros(5500),
            "{b:?}"
        );
    }

    #[test]
    fn dns_retries_cover_the_recovery_time() {
        let s = NetStats::new(2, Delta::DEFAULT, Duration::from_millis(250));
        // 解析器抖动 0–40 ms 内恢复；底数 25 ms：第一次重试等 50 ms 就覆盖 → 1 次
        for i in 0..5_000 {
            s.dns_recovery("x", Duration::from_millis(i % 40), true);
        }
        assert_eq!(s.dns_retries(1), 1);
        // 恢复要 100–300 ms：需要 50 + 100 + 200 ms → 3 次
        let s = NetStats::new(2, Delta::DEFAULT, Duration::from_millis(250));
        for i in 0..5_000 {
            s.dns_recovery("x", Duration::from_millis(100 + i % 200), true);
        }
        assert_eq!(s.dns_retries(1), 3);
    }
}
