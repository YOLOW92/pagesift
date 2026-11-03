//! f32 的 e^x 与由它组成的激活函数。不调库函数、不分支（比较只用于选值），
//! 放在对切片的循环里编译器能自动向量化。精度在 f32 舍入的量级（相对误差约 1e-7）。

/// [推导] 输入夹到这个区间：上限使 n = round(x·log₂e) ≤ 127，2ⁿ 不溢出（127.5·ln2 ≈ 88.376，
/// 留一点余量免得恰好舍入到 128）；
/// 下限使 n ≥ −126，2ⁿ 仍是规格化数（−126·ln2 ≈ −87.34）。区间外 e^x 在 f32 里已是 0 或近于上溢，
/// 夹住后只差一个可忽略的绝对量（激活函数里它们都被加 1 或作分母）。
const EXP_LO: f32 = -87.336_55;
const EXP_HI: f32 = 88.37;
/// [推导] 1.5·2²³：加上再减去，把 |v| < 2²² 的数舍入到最近的整数（f32 尾数 23 位）
const ROUND_MAGIC: f32 = 12_582_912.0;
/// [推导] ln2 拆成高低两段（Cody–Waite），高段尾数低位为 0，n·LN2_HI 精确
const LN2_HI: f32 = 0.693_359_4;
const LN2_LO: f32 = -2.121_944_4e-4;
/// [协议] Cephes `expf` 在 |r| ≤ ln2/2 上的极小化多项式系数（从 r⁵ 到 r⁰ 的高次部分）；
/// 末项原文 5.0000001201e-1，在 f32 里就是 0.5
const EXP_P: [f32; 6] = [
    1.987_569_1e-4,
    1.398_199_9e-3,
    8.333_452e-3,
    4.166_579_6e-2,
    1.666_666_5e-1,
    0.5,
];
/// [协议] IEEE 754 单精度：指数偏置 127，尾数 23 位
const F32_BIAS: i32 = 127;
const F32_MANT: u32 = 23;

/// e^x。
#[inline(always)]
pub(crate) fn exp(x: f32) -> f32 {
    let x = x.clamp(EXP_LO, EXP_HI);
    // 加上魔数后整数 n 就在尾数的低位里：位模式之差即 n（补码），免去会被拆成标量的浮点转整数
    let t = x * std::f32::consts::LOG2_E + ROUND_MAGIC;
    let n = t - ROUND_MAGIC;
    let ni = t.to_bits().wrapping_sub(ROUND_MAGIC.to_bits()) as i32;
    let r = x - n * LN2_HI - n * LN2_LO;
    let mut p = EXP_P[0];
    for &c in &EXP_P[1..] {
        p = p * r + c;
    }
    let er = p * r * r + r + 1.0;
    let scale = f32::from_bits(((ni + F32_BIAS) as u32) << F32_MANT);
    er * scale
}

/// σ(x) = 1 / (1 + e^−x)。
#[inline(always)]
pub(crate) fn sigmoid(x: f32) -> f32 {
    1.0 / (1.0 + exp(-x))
}

/// x · σ(x)。
#[inline(always)]
pub(crate) fn silu(x: f32) -> f32 {
    x / (1.0 + exp(-x))
}

/// 精确 GELU：x · Φ(x)，Φ(x) = (1 + erf(x/√2)) / 2。erf 用 Abramowitz & Stegun 式 7.1.26
/// （与 [`crate::erf`] 同一公式，绝对误差 ≤ 1.5e-7）：erfc(z) ≈ poly(t)·e^−z²（z ≥ 0）。
/// x < 0 时直接用 Φ(x) = erfc(|x|/√2) / 2，免去 1 − erf 的相消。
#[inline(always)]
pub(crate) fn gelu(x: f32) -> f32 {
    // [推导] 式 7.1.26 的系数 p、a1–a5（同 nn.rs 的 erf）
    const P: f32 = 0.327_591_1;
    const A: [f32; 5] = [
        0.254_829_6,
        -0.284_496_74,
        1.421_413_7,
        -1.453_152_1,
        1.061_405_4,
    ];
    let z = x.abs() * std::f32::consts::FRAC_1_SQRT_2;
    let t = 1.0 / (1.0 + P * z);
    let poly = t * (A[0] + t * (A[1] + t * (A[2] + t * (A[3] + t * A[4]))));
    let erfc = poly * exp(-z * z);
    let phi = if x >= 0.0 {
        1.0 - 0.5 * erfc
    } else {
        0.5 * erfc
    };
    x * phi
}

#[cfg(test)]
mod tests {
    /// 与 f64 的标准库函数对照，扫过 [-100, 100]（含饱和区）。
    #[test]
    fn matches_f64() {
        // [策略] 步长 1/64，共约 1.3 万个点
        let xs: Vec<f32> = (-6400..=6400).map(|i| i as f32 / 64.0).collect();
        for &x in &xs {
            let xd = x as f64;
            let e = xd.exp();
            if xd < super::EXP_HI as f64 && xd > super::EXP_LO as f64 {
                let g = super::exp(x) as f64;
                // [策略] 相对误差远小于 1e-4，这里要求 f32 舍入量级的 1e-6
                assert!((g - e).abs() <= 1e-6 * e, "exp({x}) = {g}，应为 {e}");
            }
            let sig = 1.0 / (1.0 + (-xd).exp());
            let silu = xd * sig;
            // 参照用 f64 的同一个 erf 公式
            let gelu = 0.5 * xd * (1.0 + crate::erf((xd / std::f64::consts::SQRT_2) as f32) as f64);
            for (name, got, want) in [
                ("sigmoid", super::sigmoid(x), sig),
                ("silu", super::silu(x), silu),
                ("gelu", super::gelu(x), gelu),
            ] {
                let got = got as f64;
                // [策略] 相对误差 1e-4，量级不足 1 时按 1 算
                assert!(
                    (got - want).abs() <= 1e-4 * want.abs().max(1.0),
                    "{name}({x}) = {got}，应为 {want}"
                );
            }
        }
    }
}
