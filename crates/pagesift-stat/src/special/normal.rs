//! 标准正态分布的密度、分布函数与分位数。

use std::f64::consts::{PI, SQRT_2};

use super::EPS;
use super::gamma::erfc;

/// 标准正态分布函数 `Φ(x) = ½ erfc(−x/√2)`（DLMF 7.18.2 的记号）。
pub fn normal_cdf(x: f64) -> f64 {
    0.5 * erfc(-x / SQRT_2)
}

/// 标准正态密度 `φ(x) = e^{−x²/2} / √(2π)`。
pub fn normal_pdf(x: f64) -> f64 {
    (-0.5 * x * x - 0.5 * (2.0 * PI).ln()).exp()
}

/// 标准正态分位数 `Φ⁻¹(p)`，`0 < p < 1`。
///
/// 初值用 Abramowitz–Stegun 26.2.23（绝对误差 < 4.5e-4），再用 Halley 迭代
/// `x ← x − e / (φ(x)·(1 + x·e/(2φ(x))))`，`e = Φ(x) − p`，三次方收敛。
/// 在下尾计算再按对称性取符号，避免 `p` 接近 1 时 `Φ(x) − p` 的相消误差。
pub fn normal_quantile(p: f64) -> f64 {
    if p.is_nan() || p <= 0.0 {
        return if p == 0.0 {
            f64::NEG_INFINITY
        } else {
            f64::NAN
        };
    }
    if p >= 1.0 {
        return if p == 1.0 { f64::INFINITY } else { f64::NAN };
    }
    let (q, sign) = if p < 0.5 { (p, -1.0) } else { (1.0 - p, 1.0) };
    // [推导] A&S 26.2.23 的有理逼近系数
    let t = (-2.0 * q.ln()).sqrt();
    let num = 2.515517 + 0.802853 * t + 0.010328 * t * t; // [推导] 同上
    let den = 1.0 + 1.432788 * t + 0.189269 * t * t + 0.001308 * t * t * t; // [推导] 同上
    let mut x = -(t - num / den); // 下尾，x < 0
    // [推导] Halley 三次收敛：初值误差 4.5e-4 时三步到 1e-30 量级，最多再给一步余量
    for _ in 0..4 {
        let e = normal_cdf(x) - q;
        let phi = normal_pdf(x);
        if phi == 0.0 {
            break;
        }
        let u = e / phi;
        let step = u / (1.0 + 0.5 * x * u);
        x -= step;
        if step.abs() <= EPS * x.abs() {
            break;
        }
    }
    sign * -x
}
