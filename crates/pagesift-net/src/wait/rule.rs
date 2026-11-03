//! 等待决策的纯函数。不碰锁和时钟，单独可测。

use pagesift_stat::Delta;
use pagesift_stat::beta::wilson;
use pagesift_stat::sketch::Sketch;
use pagesift_stat::survival::RiskRow;

/// `Ŝ(x)`：KM 表里时间 ≤ x 的最后一行之后的生存值。
fn survival_at(rows: &[RiskRow], x: f64) -> f64 {
    rows.iter()
        .take_while(|r| r.time <= x)
        .last()
        .map_or(1.0, |r| r.survival)
}

/// 时间 > x 的观测数（KM 在 x 之后的风险集）。
fn beyond(rows: &[RiskRow], x: f64) -> f64 {
    rows.iter().find(|r| r.time > x).map_or(0.0, |r| r.at_risk)
}

/// 放弃时刻（DESIGN §5.4-W）：已等 `t`，继续等 ⇔ `h·价值 > λ·Δ·占用`。`price` 是 `λ·占用/价值`
/// （每秒），没有负载时为 0。
///
/// 只有统计上显著地"再等也不会来"才放弃，没有证据的区间一律按上限等（冷启动、数据不足时与 0.81 相同）：
/// - **展望到观测的尽头**：`h = 1 − S(T_end)/S(t)` 是等过 `t` 的请求在全部观测时长内**终究完成**的概率，
///   `Δ = T_end − t`。只看下一小段（例如 `Δ = t`）会在多峰分布的峰间空档（大多 5 ms、偶尔 200 ms）
///   误以为"不会来了"：0.86 第一版就因此在负载高时截断了偶尔慢一次的首字节。
/// - **证据来自截尾样本**：`h` 取 Wilson 上界（水平 δ，样本量是 `t` 之后的风险集），要求它同时
///   `< δ`（等过 `t` 的请求几乎都没等到，即一直挂到上限）和 `≤ λ·Δ`（再等的期望价值抵不上占用）。
///   `t` 之后的请求终究都完成了时上界接近 1，不放弃；样本少时上界大，也不放弃。
///
/// 返回 `None` 表示继续等到上限。
pub(crate) fn abandon_time(rows: &[RiskRow], price: f64, delta: Delta) -> Option<f64> {
    if price.is_nan() || price <= 0.0 {
        return None;
    }
    let last = rows.last()?;
    let (end, s_end) = (last.time, last.survival);
    for r in rows {
        let t = r.time;
        let n = beyond(rows, t);
        let st = survival_at(rows, t);
        if n <= 0.0 || st <= 0.0 {
            break;
        }
        let h = (1.0 - s_end / st).max(0.0);
        let (_, hi) = wilson(h * n, n, delta);
        if hi < delta.get() && hi <= price * (end - t) {
            return Some(t);
        }
    }
    None
}

/// 分位数 `q(p)` 的分布无关置信区间（次序统计量法，David & Nagaraja, *Order Statistics*, 3rd ed.,
/// 2003, §7.1）：秩 `np ± z(1−δ)·√(np(1−p))` 处的样本值。区间越出 `[0, 1]` 时样本不够，返回 `None`。
pub(crate) fn quantile_interval(s: &Sketch, p: f64, delta: Delta) -> Option<(f64, f64)> {
    let n = s.count() as f64;
    if n <= 0.0 {
        return None;
    }
    let half = delta.z() * (p * (1.0 - p) / n).sqrt();
    let (lo, hi) = (p - half, p + half);
    if lo < 0.0 || hi > 1.0 {
        return None;
    }
    Some((s.quantile(lo), s.quantile(hi)))
}

/// 窗口收口（`pagesift_stat::window` 的分位数版本）：`q(p)` 的置信区间半宽相对点估计小于 `δ`
/// 时，这个窗口的估计够准，返回 `(点估计, 区间)`；否则继续攒样本。
pub(crate) fn precise_quantile(s: &Sketch, p: f64, delta: Delta) -> Option<(f64, (f64, f64))> {
    let (lo, hi) = quantile_interval(s, p, delta)?;
    let q = s.quantile(p);
    (q > 0.0 && (hi - lo) / 2.0 < delta.get() * q).then_some((q, (lo, hi)))
}

/// 只在显著时更新：当前值落在新窗口估计的置信区间外才换成估计值。
/// 当前值是只有一个伪样本强度的先验，精度加权步长 `K = n/(n+1)` 在窗口收口时（n 为数百以上）
/// 与 1 的差小于草图误差，这里直接取估计值。
pub(crate) fn settle(current: f64, estimate: f64, interval: (f64, f64)) -> f64 {
    if current < interval.0 || current > interval.1 {
        estimate
    } else {
        current
    }
}

/// 下一次重算的样本量：新增约 `z(1−δ)·√n` 个样本后，均值类估计的变化才可能超过 `z·SE`
/// （SE ∝ 1/√n），更早重算只会得到不显著的差。
pub(crate) fn next_refit(n: u64, delta: Delta) -> u64 {
    n + (delta.z() * (n as f64).sqrt()).ceil().max(1.0) as u64
}

/// 变化检测：伯努利 CUSUM（Page, Biometrika 41, 1954）。放弃时刻生效后，按设计被放弃的
/// 比例不超过 `p₀ = δ`（放弃时刻不早于 `q(1−δ)`）；备择是"大半请求被放弃" `p₁ = ½`
/// ——主机整体变慢时的情形，也是继续用旧上限会让成功率减半的分界。单步对数似然比
/// `x·ln(p₁/p₀) + (1−x)·ln((1−p₁)/(1−p₀))`，累计和（下截 0）超过 `ln(1/δ)` 报警
/// （Wald SPRT 门槛；Lorden 1971：平均误报间隔不少于 `1/δ`）。返回新的累计和与是否报警。
pub(crate) fn cusum_step(sum: f64, abandoned: bool, delta: Delta) -> (f64, bool) {
    let p0 = delta.get();
    let p1 = 0.5;
    let llr = if abandoned {
        (p1 / p0).ln()
    } else {
        ((1.0 - p1) / (1.0 - p0)).ln()
    };
    let next = (sum + llr).max(0.0);
    if next > (1.0 / p0).ln() {
        (0.0, true)
    } else {
        (next, false)
    }
}

/// 有界表的清理门槛（摊还分析，Cormen 等 *Introduction to Algorithms* 3rd ed. §17.4 动态表）：
/// 清理后剩 `live` 项，下次在 `2·live` 时再清；不低于先验门槛 `floor`（0.81 的固定值）。
/// 每次清理的 O(n) 摊到其间的插入上是 O(1)，表的大小不超过存活项的两倍；
/// 固定门槛在存活项多于门槛时会每次插入都全表扫描。
pub(crate) fn next_sweep(live: usize, floor: usize) -> usize {
    live.saturating_mul(2).max(floor)
}

#[cfg(test)]
mod tests {
    use super::*;
    use pagesift_stat::rng::Rng;
    use pagesift_stat::survival::Survival;

    fn sample(f: impl Fn(&mut Rng) -> f64, n: usize, cap: f64) -> Survival {
        let mut rng = Rng::seed(7);
        let mut s = Survival::with_alpha(0.02);
        for _ in 0..n {
            let t = f(&mut rng);
            if t < cap {
                s.event(t);
            } else {
                s.censor(cap);
            }
        }
        s
    }

    /// 没有负载（价格 0）或样本太少时不放弃：冷启动等于现值（只按上限）。
    #[test]
    fn idle_or_cold_never_abandons() {
        let s = sample(|r| -r.uniform().max(1e-12).ln(), 5_000, 30.0);
        let d = Delta::DEFAULT;
        assert_eq!(abandon_time(&s.risk_table(), 0.0, d), None);
        let few = sample(|r| -r.uniform().max(1e-12).ln(), 10, 30.0);
        assert_eq!(abandon_time(&few.risk_table(), 0.01, d), None);
    }

    /// 回归（0.84 对比里 24 页 408）：大多 5 ms、偶尔 200 ms 的首字节，负载再高也不能放弃那次 200 ms。
    #[test]
    fn occasional_slow_first_byte_is_never_abandoned() {
        let mut s = Survival::with_alpha(0.02);
        for i in 0..20_000 {
            s.event(if i % 50 == 0 { 0.2 } else { 0.005 });
        }
        let rows = s.risk_table();
        for price in [0.01, 1.0, 100.0, 1e6] {
            assert_eq!(abandon_time(&rows, price, Delta::DEFAULT), None, "{price}");
        }
    }

    /// 一部分请求永远挂到上限（截尾在 30 s）：过了正常请求的完成时刻、风险集里只剩挂住的，
    /// 有显著证据"再等也不会来"，有负载时在那里放弃；没有负载时仍等到上限。
    #[test]
    fn hanging_requests_are_abandoned_only_with_evidence_and_load() {
        let mut rng = Rng::seed(7);
        let mut s = Survival::with_alpha(0.02);
        for i in 0..5_000 {
            if i % 10 == 0 {
                s.censor(30.0);
            } else {
                s.event(0.005 + 0.05 * rng.uniform());
            }
        }
        let rows = s.risk_table();
        let d = Delta::DEFAULT;
        let t = abandon_time(&rows, 0.1, d).unwrap();
        assert!((0.05..0.06).contains(&t), "{t}");
        assert_eq!(abandon_time(&rows, 0.0, d), None);
        // 挂住的只有 20 个：上界约 z²/(n+z²) ≈ 0.12 > δ，证据不够，不放弃
        let mut few = Survival::with_alpha(0.02);
        for i in 0..2_000 {
            if i % 100 == 0 {
                few.censor(30.0);
            } else {
                few.event(0.005);
            }
        }
        assert_eq!(abandon_time(&few.risk_table(), 0.1, d), None);
    }

    #[test]
    fn quantile_windows_and_settling() {
        let d = Delta::DEFAULT;
        let mut s = Sketch::new(0.01);
        let mut rng = Rng::seed(3);
        assert!(precise_quantile(&s, 0.5, d).is_none());
        for _ in 0..20 {
            s.push(-rng.uniform().max(1e-12).ln());
        }
        assert!(precise_quantile(&s, 0.5, d).is_none(), "20 个样本不够准");
        for _ in 0..20_000 {
            s.push(-rng.uniform().max(1e-12).ln());
        }
        let (q, ci) = precise_quantile(&s, 0.5, d).unwrap();
        assert!((q - std::f64::consts::LN_2).abs() < 0.03, "{q}");
        assert!(ci.0 <= q && q <= ci.1);
        // 当前值在区间内不动，区间外换成估计值
        assert_eq!(settle(q, q * 1.001, ci), q);
        assert_eq!(settle(5.0, q, ci), q);
        // 尾分位需要更多样本：p = 0.95 时 n ≥ z²·p/(1−p) ≈ 52 才有区间
        let mut t = Sketch::new(0.01);
        (0..40).for_each(|i| t.push(i as f64 + 1.0));
        assert!(quantile_interval(&t, 0.95, d).is_none());
    }

    #[test]
    fn refit_change_and_sweep_schedules() {
        let d = Delta::DEFAULT;
        assert_eq!(next_refit(0, d), 1);
        assert_eq!(next_refit(100, d), 117);
        // 平稳时（每 20 个放弃 1 个）长期不报警；连续被放弃很快报警
        let mut s = 0.0;
        for i in 0..2_000 {
            let (n, alarm) = cusum_step(s, i % 20 == 0, d);
            assert!(!alarm, "第 {i} 个误报");
            s = n;
        }
        let (s, a) = cusum_step(0.0, true, d);
        assert!(!a);
        assert!(cusum_step(s, true, d).1);
        assert_eq!(next_sweep(10, 4096), 4096);
        assert_eq!(next_sweep(5000, 4096), 10_000);
    }
}
