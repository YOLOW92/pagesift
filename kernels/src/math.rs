//! 设备端数学函数。`core` 没有 exp（要 libm），这里自己写：Cody–Waite 区间约化 + 多项式。

/// exp(x)，f32。x = n·ln2 + r，|r| ≤ ln2/2，e^r 用 7 阶泰勒多项式，结果乘 2^n。
/// 截断误差 |r|⁸/8! ≈ 5 × 10⁻⁹ 相对，加上舍入约 1–2 ulp。上溢给 +∞，下溢给 0（不产生次正规数）。
#[inline(always)]
pub fn expf(x: f32) -> f32 {
    // [协议] IEEE 754 binary32：e^88.73 已超过最大有限值 3.4e38；e^-87.34 低于最小正规数 1.18e-38
    if x > 88.722_84 {
        return f32::INFINITY;
    }
    if x < -87.336_55 {
        return 0.0;
    }
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
    // [协议] binary32 指数偏置 127、尾数 23 位；n ∈ [−126, 128]，128 时分两步乘避免溢出
    if n > 127 {
        return p * 2.0 * f32::from_bits(((n - 1 + 127) as u32) << 23);
    }
    p * f32::from_bits(((n + 127) as u32) << 23)
}

/// exp(x)，f64，只给 erf 用（参数 ≤ 0）。同样的区间约化，13 阶泰勒多项式（截断约 10⁻¹⁷ 相对）。
#[inline(always)]
pub fn exp64(x: f64) -> f64 {
    // [协议] IEEE 754 binary64：e^-708.4 低于最小正规数 2.2e-308；e^709.78 超过最大有限值
    if x < -708.39 {
        return 0.0;
    }
    if x > 709.78 {
        return f64::INFINITY;
    }
    // [推导] log2(e)；ln2 拆成高段（尾数低位为 0，n·LN2_HI 精确）与低段，取自 fdlibm e_exp.c
    const LOG2E: f64 = 1.442_695_040_888_963_4;
    const LN2_HI: f64 = 6.931_471_803_691_238_2e-1;
    const LN2_LO: f64 = 1.908_214_929_270_587_7e-10;
    let t = x * LOG2E;
    let n = if t >= 0.0 {
        (t + 0.5) as i64
    } else {
        (t - 0.5) as i64
    };
    let nf = n as f64;
    let r = (x - nf * LN2_HI) - nf * LN2_LO;
    // [推导] 霍纳法从高阶往回：e^r = Σ r^k / k!，k = 0..13
    let mut p = 1.0;
    let mut k = 13;
    while k > 0 {
        p = 1.0 + r * p / k as f64;
        k -= 1;
    }
    // [协议] binary64 指数偏置 1023、尾数 52 位；n ∈ [−1022, 1024]，1024 时分两步乘
    if n > 1023 {
        return p * 2.0 * f64::from_bits(((n - 1 + 1023) as u64) << 52);
    }
    p * f64::from_bits(((n + 1023) as u64) << 52)
}

/// 误差函数，与 `pagesift_infer::erf` 同一个公式、同样在 f64 里算：
/// Abramowitz & Stegun《Handbook of Mathematical Functions》式 7.1.26，绝对误差 ≤ 1.5 × 10⁻⁷。
#[inline(always)]
pub fn erf(x: f32) -> f32 {
    // [推导] 式 7.1.26 的系数 p、a1–a5
    const P: f64 = 0.327_591_1;
    const A1: f64 = 0.254_829_592;
    const A2: f64 = -0.284_496_736;
    const A3: f64 = 1.421_413_741;
    const A4: f64 = -1.453_152_027;
    const A5: f64 = 1.061_405_429;
    let s = if x < 0.0 { -1.0 } else { 1.0 };
    let x = x as f64;
    let x = if x < 0.0 { -x } else { x };
    let t = 1.0 / (1.0 + P * x);
    let poly = t * (A1 + t * (A2 + t * (A3 + t * (A4 + t * A5))));
    (s * (1.0 - poly * exp64(-x * x))) as f32
}
