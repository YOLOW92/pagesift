//! 正则化不完全贝塔函数、贝塔分布与 Student t 分布的分位数。

use super::gamma::ln_beta;
use super::normal::normal_quantile;
use super::{EPS, MAX_ITER, TINY};

/// 正则化不完全贝塔函数 `I_x(a, b)`，`0 ≤ x ≤ 1`。
///
/// 连分式 DLMF 8.17.22，用修正 Lentz 算法；`x > (a+1)/(a+b+2)` 时用
/// `I_x(a,b) = 1 − I_{1−x}(b,a)`（DLMF 8.17.4）换到收敛快的一侧。
pub fn beta_inc(a: f64, b: f64, x: f64) -> f64 {
    if x <= 0.0 {
        return 0.0;
    }
    if x >= 1.0 {
        return 1.0;
    }
    // [推导] DLMF 8.17.22 的收敛区间判据 x < (a+1)/(a+b+2)
    if x > (a + 1.0) / (a + b + 2.0) {
        return 1.0 - beta_inc(b, a, 1.0 - x);
    }
    let front = (a * x.ln() + b * (1.0 - x).ln() - ln_beta(a, b)).exp() / a;
    // d_{2m+1} = −(a+m)(a+b+m)x / ((a+2m)(a+2m+1))，d_{2m} = m(b−m)x / ((a+2m−1)(a+2m))
    let mut c = 1.0;
    let mut d = 1.0 - (a + b) * x / (a + 1.0);
    if d.abs() < TINY {
        d = TINY;
    }
    d = 1.0 / d;
    let mut h = d;
    for m in 1..MAX_ITER {
        let m = m as f64;
        let m2 = 2.0 * m;
        for num in [
            m * (b - m) * x / ((a + m2 - 1.0) * (a + m2)),
            -(a + m) * (a + b + m) * x / ((a + m2) * (a + m2 + 1.0)),
        ] {
            d = 1.0 + num * d;
            if d.abs() < TINY {
                d = TINY;
            }
            c = 1.0 + num / c;
            if c.abs() < TINY {
                c = TINY;
            }
            d = 1.0 / d;
            h *= d * c;
        }
        if (d * c - 1.0).abs() < EPS {
            break;
        }
    }
    front * h
}

/// 用二分法解单调递增函数 `f(x) = target`，`x ∈ [lo, hi]`，直到区间相对宽度到机器精度。
/// 分位数不在热路径上，二分最稳，不需要导数。
fn bisect(mut lo: f64, mut hi: f64, target: f64, f: impl Fn(f64) -> f64) -> f64 {
    for _ in 0..MAX_ITER {
        let mid = 0.5 * (lo + hi);
        if mid <= lo || mid >= hi || hi - lo <= EPS * mid.abs().max(TINY) {
            return mid;
        }
        if f(mid) < target {
            lo = mid;
        } else {
            hi = mid;
        }
    }
    0.5 * (lo + hi)
}

/// 贝塔分布 `Beta(a, b)` 的 `p` 分位数。
pub fn beta_quantile(a: f64, b: f64, p: f64) -> f64 {
    if p <= 0.0 {
        return 0.0;
    }
    if p >= 1.0 {
        return 1.0;
    }
    bisect(0.0, 1.0, p, |x| beta_inc(a, b, x))
}

/// 自由度 `nu` 的 Student t 分布函数（DLMF 8.17.1 与 t 分布的关系：
/// `t ≥ 0` 时 `F(t) = 1 − ½ I_{ν/(ν+t²)}(ν/2, ½)`）。
pub fn t_cdf(t: f64, nu: f64) -> f64 {
    let tail = 0.5 * beta_inc(0.5 * nu, 0.5, nu / (nu + t * t));
    if t >= 0.0 { 1.0 - tail } else { tail }
}

/// Student t 分布的 `p` 分位数。上界从正态分位数起倍增，直到把 `p` 包住，再二分。
pub fn t_quantile(p: f64, nu: f64) -> f64 {
    if p == 0.5 {
        return 0.0;
    }
    if p < 0.5 {
        return -t_quantile(1.0 - p, nu);
    }
    let mut hi = normal_quantile(p).max(1.0);
    while t_cdf(hi, nu) < p && hi.is_finite() {
        hi *= 2.0;
    }
    bisect(0.0, hi, p, |t| t_cdf(t, nu))
}
