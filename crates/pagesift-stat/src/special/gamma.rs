//! 对数伽马、不完全伽马与互补误差函数。

use std::f64::consts::PI;

use super::{EPS, MAX_ITER, TINY};

/// `ln Γ(x)`，`x > 0`。
///
/// Stirling 级数（DLMF 5.11.1）：
/// `ln Γ(x) = (x−½)ln x − x + ½ ln(2π) + Σ B₂ₖ / (2k(2k−1) x^{2k−1})`。
/// 先用 `Γ(x+1) = xΓ(x)`（DLMF 5.5.1）把 `x` 推到 ≥ [`STIRLING_FROM`]，
/// 级数取到 B₁₂ 项，截断误差小于下一项 `|B₁₄|/(14·13·x¹³)` < 1e-17。
pub fn ln_gamma(x: f64) -> f64 {
    debug_assert!(x > 0.0, "ln_gamma needs x > 0, got {x}");
    // [推导] 推到 x ≥ 15 时，第一个舍去项 B₁₄/(182 x¹³) ≈ 6e-17，低于 f64 精度
    const STIRLING_FROM: f64 = 15.0;
    let mut shift = 0.0;
    let mut z = x;
    while z < STIRLING_FROM {
        shift += z.ln();
        z += 1.0;
    }
    // [推导] B₂ₖ/(2k(2k−1))，k = 1..6：1/12、−1/360、1/1260、−1/1680、1/1188、−691/360360（DLMF 24.2.1 表）
    const C: [f64; 6] = [
        1.0 / 12.0,
        -1.0 / 360.0,
        1.0 / 1260.0,
        -1.0 / 1680.0,
        1.0 / 1188.0,
        -691.0 / 360_360.0,
    ];
    let inv = 1.0 / z;
    let inv2 = inv * inv;
    let mut series = 0.0;
    let mut p = inv;
    for c in C {
        series += c * p;
        p *= inv2;
    }
    // [推导] ½ ln(2π)
    (z - 0.5) * z.ln() - z + 0.5 * (2.0 * PI).ln() + series - shift
}

/// `ln B(a, b) = ln Γ(a) + ln Γ(b) − ln Γ(a+b)`（DLMF 5.12.1）。
pub fn ln_beta(a: f64, b: f64) -> f64 {
    ln_gamma(a) + ln_gamma(b) - ln_gamma(a + b)
}

/// 正则化下不完全伽马函数 `P(a, x) = γ(a, x) / Γ(a)`。
pub fn gamma_p(a: f64, x: f64) -> f64 {
    if x <= 0.0 {
        return 0.0;
    }
    if x < a + 1.0 {
        gamma_series(a, x)
    } else {
        1.0 - gamma_cf(a, x)
    }
}

/// 正则化上不完全伽马函数 `Q(a, x) = 1 − P(a, x)`。
pub fn gamma_q(a: f64, x: f64) -> f64 {
    if x <= 0.0 {
        return 1.0;
    }
    if x < a + 1.0 {
        1.0 - gamma_series(a, x)
    } else {
        gamma_cf(a, x)
    }
}

/// `P(a, x)` 的级数（DLMF 8.7.1 改写为 `γ*`）：`x^a e^{−x} / Γ(a+1) · Σ xⁿ / ((a+1)…(a+n))`。
/// 在 `x < a + 1` 时各项单调减小，收敛快。
fn gamma_series(a: f64, x: f64) -> f64 {
    let mut term = 1.0 / a;
    let mut sum = term;
    let mut n = a;
    for _ in 0..MAX_ITER {
        n += 1.0;
        term *= x / n;
        sum += term;
        if term.abs() < sum.abs() * EPS {
            break;
        }
    }
    sum * (a * x.ln() - x - ln_gamma(a)).exp()
}

/// `Q(a, x)` 的连分式（DLMF 8.9.2），用修正 Lentz 算法求值；`x ≥ a + 1` 时收敛快。
fn gamma_cf(a: f64, x: f64) -> f64 {
    // 偶部化后的形式：Γ(a,x) = e^{−x} x^a / (x+1−a− 1·(1−a)/(x+3−a− 2·(2−a)/(x+5−a− …)))
    let mut b = x + 1.0 - a;
    let mut c = 1.0 / TINY;
    let mut d = 1.0 / b;
    let mut h = d;
    for i in 1..MAX_ITER {
        let i = i as f64;
        let an = -i * (i - a);
        // [推导] 偶部化连分式的分母每步加 2
        b += 2.0;
        d = an * d + b;
        if d.abs() < TINY {
            d = TINY;
        }
        c = b + an / c;
        if c.abs() < TINY {
            c = TINY;
        }
        d = 1.0 / d;
        let delta = d * c;
        h *= delta;
        if (delta - 1.0).abs() < EPS {
            break;
        }
    }
    (a * x.ln() - x - ln_gamma(a)).exp() * h
}

/// 互补误差函数：`erfc(x) = Q(½, x²)`，`x ≥ 0`（DLMF 7.11.2）；负半轴用 `erfc(−x) = 2 − erfc(x)`。
pub fn erfc(x: f64) -> f64 {
    if x >= 0.0 {
        gamma_q(0.5, x * x)
    } else {
        2.0 - gamma_q(0.5, x * x)
    }
}
