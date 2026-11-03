//! 快速实现与 `reference` 里按定义写的原版逐一对照：随机输入，覆盖各分派路径、步长、补边、
//! dilation、groups，以及够大到多线程的形状。

use super::reference;
use crate::{Activation, Tensor};

/// 确定性的伪随机数（线性同余），取值在 [-1, 1)。
struct Rng(u32);

impl Rng {
    fn next(&mut self) -> f32 {
        // [协议] Numerical Recipes 的线性同余参数
        self.0 = self.0.wrapping_mul(1_664_525).wrapping_add(1_013_904_223);
        // [推导] 取高 24 位（f32 尾数宽度）映到 [0, 1)，再映到 [-1, 1)
        (self.0 >> 8) as f32 / (1u32 << 24) as f32 * 2.0 - 1.0
    }

    fn tensor(&mut self, shape: Vec<usize>) -> Tensor {
        let n = shape.iter().product();
        Tensor::new(shape, (0..n).map(|_| self.next()).collect())
    }

    fn vec(&mut self, n: usize) -> Vec<f32> {
        (0..n).map(|_| self.next()).collect()
    }
}

/// 逐元素比较：相对误差不超过 1e-4；参照值量级不足 1 时按 1 算（输入在 [-1, 1)，
/// 接近 0 的和是多项相消的结果，只能按输入的量级衡量舍入误差）。
fn assert_close(got: &Tensor, want: &Tensor, what: &str) {
    assert_eq!(got.shape, want.shape, "{what}：形状不同");
    for (i, (g, w)) in got.data.iter().zip(&want.data).enumerate() {
        // [策略] 题目要求的相对误差 1e-4
        let tol = 1e-4 * w.abs().max(1.0);
        assert!((g - w).abs() <= tol, "{what}：第 {i} 个元素 {g} ≠ {w}");
    }
}

/// 一组卷积参数：(输入 C、H、W)、输出通道、groups、核 (kh, kw)、步长、补边、dilation、是否带偏置。
type ConvCase = (
    (usize, usize, usize),
    usize,
    usize,
    (usize, usize),
    (usize, usize),
    (usize, usize),
    (usize, usize),
    bool,
);

#[test]
fn conv2d_matches_reference() {
    // [策略] 各分派路径的典型形状和边角，末尾几个足够大，会多线程
    let cases: &[ConvCase] = &[
        // 1×1 当矩阵乘
        ((8, 5, 7), 12, 1, (1, 1), (1, 1), (0, 0), (1, 1), true),
        ((8, 5, 7), 6, 2, (1, 1), (1, 1), (0, 0), (1, 1), false),
        ((300, 4, 9), 13, 1, (1, 1), (1, 1), (0, 0), (1, 1), true),
        ((24, 1, 1), 96, 1, (1, 1), (1, 1), (0, 0), (1, 1), true),
        ((96, 24, 40), 192, 1, (1, 1), (1, 1), (0, 0), (1, 1), true),
        // 1×1 但有步长或补边：走 im2col
        ((8, 9, 9), 5, 1, (1, 1), (2, 2), (0, 0), (1, 1), true),
        ((8, 4, 5), 5, 1, (1, 1), (1, 1), (1, 1), (1, 1), true),
        // 深度卷积
        ((16, 12, 80), 16, 16, (3, 3), (1, 1), (1, 1), (1, 1), true),
        ((16, 13, 81), 16, 16, (3, 3), (2, 2), (1, 1), (1, 1), true),
        ((16, 12, 80), 16, 16, (3, 3), (2, 1), (1, 1), (1, 1), false),
        ((16, 12, 80), 16, 16, (3, 3), (1, 2), (1, 1), (1, 1), true),
        ((8, 15, 17), 8, 8, (7, 7), (1, 1), (3, 3), (1, 1), true),
        ((8, 15, 17), 8, 8, (5, 5), (1, 1), (4, 4), (2, 2), true),
        ((8, 15, 17), 8, 8, (3, 3), (2, 2), (2, 3), (2, 3), true),
        ((8, 11, 13), 8, 8, (1, 5), (1, 1), (0, 2), (1, 1), true),
        ((4, 9, 10), 8, 4, (3, 3), (1, 1), (1, 1), (1, 1), true),
        ((1, 9, 10), 4, 1, (3, 3), (1, 1), (1, 1), (1, 1), true),
        ((8, 2, 2), 8, 8, (3, 3), (1, 1), (1, 1), (1, 1), true),
        (
            (256, 64, 64),
            256,
            256,
            (3, 3),
            (1, 1),
            (1, 1),
            (1, 1),
            true,
        ),
        // 普通卷积：im2col + 矩阵乘
        ((5, 9, 11), 7, 1, (3, 3), (1, 1), (1, 1), (1, 1), true),
        ((5, 9, 11), 7, 1, (3, 3), (2, 2), (1, 1), (1, 1), true),
        ((5, 12, 20), 7, 1, (3, 3), (2, 1), (1, 1), (1, 1), false),
        ((5, 12, 20), 7, 1, (3, 3), (1, 1), (2, 2), (2, 2), true),
        ((6, 13, 14), 4, 1, (3, 3), (1, 1), (3, 1), (3, 1), true),
        ((24, 17, 19), 12, 1, (2, 2), (1, 1), (0, 0), (1, 1), true),
        ((8, 9, 10), 6, 2, (3, 3), (1, 1), (1, 1), (1, 1), true),
        ((12, 9, 10), 12, 4, (3, 3), (2, 2), (1, 1), (1, 1), true),
        ((3, 2, 2), 4, 1, (3, 3), (1, 1), (1, 1), (1, 1), true),
        ((3, 7, 5), 4, 1, (5, 3), (1, 1), (2, 1), (1, 1), true),
        ((40, 6, 80), 9, 1, (3, 3), (1, 1), (1, 1), (1, 1), true),
        ((48, 40, 60), 24, 1, (3, 3), (2, 2), (1, 1), (1, 1), true),
        ((32, 48, 64), 40, 1, (3, 3), (1, 1), (1, 1), (1, 1), true),
        ((3, 96, 120), 24, 1, (3, 3), (2, 2), (1, 1), (1, 1), true),
    ];
    let mut r = Rng(7);
    for &((c, h, w), o, groups, (kh, kw), stride, pad, dilation, with_bias) in cases {
        let x = r.tensor(vec![c, h, w]);
        let wt = r.tensor(vec![o, c / groups, kh, kw]);
        let b = with_bias.then(|| r.vec(o));
        let got = super::conv2d(&x, &wt, b.as_deref(), stride, pad, dilation, groups);
        let want = reference::conv2d(&x, &wt, b.as_deref(), stride, pad, dilation, groups);
        let what = format!(
            "conv2d C{c} {h}×{w} O{o} g{groups} k{kh}×{kw} s{stride:?} p{pad:?} d{dilation:?}"
        );
        assert_close(&got, &want, &what);
    }
}

#[test]
fn conv_transpose2d_matches_reference() {
    // (输入 C、H、W)、输出通道、核、步长、补边
    type Case = (
        (usize, usize, usize),
        usize,
        (usize, usize),
        (usize, usize),
        (usize, usize),
    );
    // [策略] 常见的 2×2/4×4 上采样、步长 1、非方形，最后一个够大会多线程
    let cases: &[Case] = &[
        ((6, 5, 7), 4, (2, 2), (2, 2), (0, 0)),
        ((6, 5, 7), 4, (4, 4), (2, 2), (1, 1)),
        ((5, 6, 6), 3, (3, 3), (1, 1), (1, 1)),
        ((4, 3, 8), 5, (3, 2), (2, 1), (1, 0)),
        ((64, 46, 46), 32, (2, 2), (2, 2), (0, 0)),
    ];
    let mut r = Rng(11);
    for (i, &((c, h, w), o, (kh, kw), stride, pad)) in cases.iter().enumerate() {
        let x = r.tensor(vec![c, h, w]);
        let wt = r.tensor(vec![c, o, kh, kw]);
        let b = (i % 2 == 0).then(|| r.vec(o));
        let got = super::conv_transpose2d(&x, &wt, b.as_deref(), stride, pad);
        let want = reference::conv_transpose2d(&x, &wt, b.as_deref(), stride, pad);
        assert_close(&got, &want, &format!("conv_transpose2d 第 {i} 组"));
    }
}

#[test]
fn matmul_and_transpose_match_reference() {
    // [策略] 1、跨 K 块、Linear 的大输出（120 → 18710 的缩小版）、足以多线程的方阵
    let dims = [
        (1, 1, 1),
        (7, 300, 19),
        (40, 120, 4000),
        (130, 513, 70),
        (256, 256, 256),
    ];
    let mut r = Rng(13);
    for &(m, k, n) in &dims {
        let a = r.tensor(vec![m, k]);
        let b = r.tensor(vec![k, n]);
        let what = format!("matmul {m}×{k}·{k}×{n}");
        assert_close(&super::matmul(&a, &b), &reference::matmul(&a, &b), &what);
        assert_eq!(
            super::transpose(&b),
            reference::transpose(&b),
            "transpose {k}×{n}"
        );
    }
}

#[test]
fn pools_and_interpolate_match_reference() {
    let mut r = Rng(17);
    let x = r.tensor(vec![3, 13, 17]);
    // [策略] stem 用的 2×2 步长 1 向上取整，以及常见的 2×2、3×3 窗口
    for &(k, s, ceil) in &[
        ((2, 2), (1, 1), true),
        ((2, 2), (2, 2), false),
        ((2, 2), (2, 2), true),
        ((3, 3), (2, 2), true),
        ((3, 2), (2, 3), false),
    ] {
        assert_eq!(
            super::max_pool2d(&x, k, s, ceil),
            reference::max_pool2d(&x, k, s, ceil),
            "max_pool2d k{k:?} s{s:?} ceil={ceil}"
        );
    }
    for &k in &[(2, 2), (3, 2), (1, 4)] {
        assert_close(
            &super::avg_pool2d(&x, k),
            &reference::avg_pool2d(&x, k),
            &format!("avg_pool2d k{k:?}"),
        );
    }
    for &(oh, ow) in &[(26, 34), (5, 4), (40, 17), (1, 1)] {
        assert_eq!(
            super::interpolate_nearest(&x, oh, ow),
            reference::interpolate_nearest(&x, oh, ow),
            "interpolate_nearest {oh}×{ow}"
        );
    }
}

#[test]
fn activation_all_matches_scalar() {
    // [策略] 2²⁰ 个元素，足以切段多线程
    let n = 1 << 20;
    let mut r = Rng(19);
    // [策略] 放大到 [-8, 8)，覆盖各激活的饱和区
    let x = Tensor::new(vec![n / 64, 64], r.vec(n).iter().map(|v| v * 8.0).collect());
    for act in [
        Activation::Relu,
        Activation::Silu,
        Activation::Gelu,
        Activation::HardSigmoid,
        Activation::Sigmoid,
    ] {
        let mut got = x.clone();
        act.apply_all(&mut got);
        let want: Vec<f32> = x.data.iter().map(|&v| act.apply(v)).collect();
        assert_eq!(got.data, want, "{act:?}");
    }
}
