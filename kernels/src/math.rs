//! 设备端数学函数。`core` 没有 exp（要 libm），这里自己写：Cody–Waite 区间约化 + 多项式。

/// exp(x)，f32。x = n·ln2 + r，|r| ≤ ln2/2，e^r 用 7 阶泰勒多项式，结果乘 2^n。
/// 截断误差 |r|⁸/8! ≈ 5 × 10⁻⁹ 相对，加上舍入约 1–2 ulp。输入夹到和 CPU 版
/// （`pagesift_infer` 的 fastmath）相同的区间，两边在区间外的结果也一致。
#[inline(always)]
pub fn expf(x: f32) -> f32 {
    // [推导] 同 fastmath.rs 的 EXP_LO、EXP_HI：2ⁿ 保持规格化、不上溢
    let x = x.clamp(-87.336_55, 88.37);
    // [推导] log2(e) 与 ln2 的高低两段（Cody–Waite：高段尾数只有 9 位，n·LN2_HI 精确）
    const LOG2E: f32 = 1.442_695_04;
    const LN2_HI: f32 = 0.693_359_375;
    const LN2_LO: f32 = -2.121_944_4e-4;
    let t = x * LOG2E;
    let n = if t >= 0.0 {
        (t + 0.5) as i32
    } else {
        (t - 0.5) as i32
    };
    let nf = n as f32;
    let r = (x - nf * LN2_HI) - nf * LN2_LO;
    // [推导] e^r 的泰勒系数 1/k!，k = 2..7
    let p = 1.0
        + r * (1.0
            + r * (0.5
                + r * (1.666_666_7e-1
                    + r * (4.166_666_8e-2
                        + r * (8.333_334e-3 + r * (1.388_888_9e-3 + r * 1.984_127e-4))))));
    // [协议] binary32 指数偏置 127、尾数 23 位；夹过之后 n ∈ [−126, 127]
    p * f32::from_bits(((n + 127) as u32) << 23)
}

/// erfc(z)，z ≥ 0。Abramowitz & Stegun 式 7.1.26：erfc(z) ≈ poly(t)·e^−z²，绝对误差 ≤ 1.5e-7。
#[inline(always)]
pub fn erfc_positive(z: f32) -> f32 {
    // [推导] 式 7.1.26 的系数 p、a1–a5
    const P: f32 = 0.327_591_1;
    const A: [f32; 5] = [
        0.254_829_6,
        -0.284_496_74,
        1.421_413_7,
        -1.453_152_1,
        1.061_405_4,
    ];
    let t = 1.0 / (1.0 + P * z);
    let poly = t * (A[0] + t * (A[1] + t * (A[2] + t * (A[3] + t * A[4]))));
    poly * expf(-z * z)
}
