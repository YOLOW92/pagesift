//! GPU 结果与 `pagesift_infer` 的 CPU 参考实现对照。没有 N 卡（或没有驱动）时每个测试打印原因后跳过。
//!
//! 误差口径：矩阵乘与卷积用 max|GPU − CPU| / max|CPU|（相对于输出的最大幅值，累加顺序不同、
//! GPU 用 FMA，单个接近 0 的输出不适合逐元素算相对误差）；激活函数逐元素算
//! |GPU − CPU| / max(|CPU|, 1e-30)。上限都是 1e-4。

use std::sync::OnceLock;

use pagesift_gpu::Gpu;
use pagesift_infer::{Activation, Tensor, ops};

const TOL: f64 = 1e-4;

/// 共用的 GPU。没有驱动或没有 N 卡时返回 None（测试跳过）；有卡却打不开（PTX 载不进、驱动报错）
/// 是要报出来的故障，直接失败，不能当成“没有 GPU”跳过。
fn gpu() -> Option<&'static Gpu> {
    static GPU: OnceLock<Result<Gpu, String>> = OnceLock::new();
    match GPU.get_or_init(Gpu::open) {
        Ok(g) => Some(g),
        Err(e) if pagesift_gpu::device_count() == 0 => {
            eprintln!("跳过 GPU 测试：{e}");
            None
        }
        Err(e) => panic!("有 N 卡但打开失败：{e}"),
    }
}

/// xorshift64，取值 [−1, 1)。
struct Rng(u64);

impl Rng {
    fn next(&mut self) -> f32 {
        self.0 ^= self.0 << 13;
        self.0 ^= self.0 >> 7;
        self.0 ^= self.0 << 17;
        ((self.0 >> 40) as f32 / (1u64 << 24) as f32) * 2.0 - 1.0
    }

    fn tensor(&mut self, shape: Vec<usize>) -> Tensor {
        let n = shape.iter().product();
        Tensor::new(shape, (0..n).map(|_| self.next()).collect())
    }
}

/// max|g − c| / max|c|。
fn norm_rel_err(g: &[f32], c: &[f32]) -> f64 {
    assert_eq!(g.len(), c.len(), "长度不同");
    let scale = c
        .iter()
        .fold(0f64, |m, &v| m.max(v.abs() as f64))
        .max(1e-30);
    let diff = g
        .iter()
        .zip(c)
        .fold(0f64, |m, (&a, &b)| m.max((a as f64 - b as f64).abs()));
    if g.iter().zip(c).any(|(a, b)| a.is_nan() != b.is_nan()) {
        return f64::INFINITY;
    }
    diff / scale
}

#[test]
fn open_does_not_panic() {
    match Gpu::open() {
        Ok(g) => eprintln!("GPU：{}，算力 {:?}", g.name(), g.compute_capability()),
        Err(e) => eprintln!("没有可用 GPU：{e}"),
    }
}

#[test]
fn buffer_round_trip() {
    let Some(g) = gpu() else { return };
    let data: Vec<f32> = (0..1000).map(|i| i as f32 * 0.5).collect();
    let a = g.upload(&data).unwrap();
    let mut b = g.alloc(data.len()).unwrap();
    b.copy_from_device(&a).unwrap();
    assert_eq!(b.to_vec().unwrap(), data);
    let empty = g.upload(&[]).unwrap();
    assert!(empty.is_empty() && empty.to_vec().unwrap().is_empty());
}

#[test]
fn matmul_matches_cpu() {
    let Some(g) = gpu() else { return };
    let mut rng = Rng(0x9e37_79b9_7f4a_7c15);
    for (m, k, n) in [
        (1, 1, 1),
        (3, 5, 7),
        (64, 64, 64),
        (65, 33, 129),
        (100, 77, 300),
        (256, 1024, 512),
    ] {
        let a = rng.tensor(vec![m, k]);
        let b = rng.tensor(vec![k, n]);
        let cpu = ops::matmul(&a, &b);
        let gpu = g.matmul(&a, &b).unwrap();
        let e = norm_rel_err(&gpu, &cpu.data);
        eprintln!("matmul {m}×{k}·{k}×{n}：相对误差 {e:.2e}");
        assert!(e <= TOL, "matmul {m}×{k}·{k}×{n} 相对误差 {e:.2e}");
    }
}

struct Case {
    name: &'static str,
    chw: (usize, usize, usize),
    okk: (usize, usize, usize),
    stride: (usize, usize),
    pad: (usize, usize),
    dilation: (usize, usize),
    groups: usize,
    bias: bool,
}

#[rustfmt::skip]
const CASES: &[Case] = &[
    Case { name: "1×1", chw: (16, 20, 20), okk: (32, 1, 1), stride: (1, 1), pad: (0, 0), dilation: (1, 1), groups: 1, bias: true },
    Case { name: "1×1 步长 2", chw: (16, 20, 21), okk: (8, 1, 1), stride: (2, 2), pad: (0, 0), dilation: (1, 1), groups: 1, bias: false },
    Case { name: "1×1 groups 2", chw: (8, 10, 10), okk: (12, 1, 1), stride: (1, 1), pad: (0, 0), dilation: (1, 1), groups: 2, bias: true },
    Case { name: "3×3", chw: (8, 13, 17), okk: (16, 3, 3), stride: (1, 1), pad: (1, 1), dilation: (1, 1), groups: 1, bias: true },
    Case { name: "3×3 步长 2", chw: (3, 33, 31), okk: (32, 3, 3), stride: (2, 2), pad: (1, 1), dilation: (1, 1), groups: 1, bias: true },
    Case { name: "3×3 步长 (2,1)", chw: (16, 12, 40), okk: (24, 3, 3), stride: (2, 1), pad: (1, 1), dilation: (1, 1), groups: 1, bias: true },
    Case { name: "3×3 大 K、O 非 64 倍数", chw: (64, 24, 24), okk: (96, 3, 3), stride: (1, 1), pad: (1, 1), dilation: (1, 1), groups: 1, bias: true },
    Case { name: "3×3 dilation 2", chw: (8, 20, 20), okk: (8, 3, 3), stride: (1, 1), pad: (2, 2), dilation: (2, 2), groups: 1, bias: false },
    Case { name: "3×3 dilation (1,3)", chw: (6, 17, 25), okk: (10, 3, 3), stride: (1, 1), pad: (1, 3), dilation: (1, 3), groups: 1, bias: true },
    Case { name: "3×3 groups 4", chw: (16, 14, 14), okk: (32, 3, 3), stride: (1, 1), pad: (1, 1), dilation: (1, 1), groups: 4, bias: true },
    Case { name: "2×2 不补边", chw: (4, 9, 9), okk: (6, 2, 2), stride: (1, 1), pad: (0, 0), dilation: (1, 1), groups: 1, bias: true },
    Case { name: "3×3 深度", chw: (32, 16, 16), okk: (32, 3, 3), stride: (1, 1), pad: (1, 1), dilation: (1, 1), groups: 32, bias: true },
    Case { name: "3×3 深度 通道倍增", chw: (8, 15, 15), okk: (16, 3, 3), stride: (1, 1), pad: (1, 1), dilation: (1, 1), groups: 8, bias: true },
    Case { name: "7×7 深度", chw: (24, 19, 23), okk: (24, 7, 7), stride: (1, 1), pad: (3, 3), dilation: (1, 1), groups: 24, bias: true },
    Case { name: "7×7 深度 步长 2", chw: (32, 28, 28), okk: (32, 7, 7), stride: (2, 2), pad: (3, 3), dilation: (1, 1), groups: 32, bias: false },
    Case { name: "5×5 深度 dilation 2", chw: (12, 30, 30), okk: (12, 5, 5), stride: (1, 1), pad: (4, 4), dilation: (2, 2), groups: 12, bias: true },
    Case { name: "3×3 深度 步长 (2,1)", chw: (16, 12, 40), okk: (16, 3, 3), stride: (2, 1), pad: (1, 1), dilation: (1, 1), groups: 16, bias: true },
    Case { name: "单通道输入", chw: (1, 32, 48), okk: (16, 3, 3), stride: (2, 2), pad: (1, 1), dilation: (1, 1), groups: 1, bias: true },
];

#[test]
fn conv2d_matches_cpu() {
    let Some(g) = gpu() else { return };
    let mut rng = Rng(0x2545_f491_4f6c_dd1d);
    for c in CASES {
        let (ch, h, w) = c.chw;
        let (o, kh, kw) = c.okk;
        let x = rng.tensor(vec![ch, h, w]);
        let wt = rng.tensor(vec![o, ch / c.groups, kh, kw]);
        let bias: Option<Vec<f32>> = c.bias.then(|| (0..o).map(|_| rng.next()).collect());
        let cpu = ops::conv2d(
            &x,
            &wt,
            bias.as_deref(),
            c.stride,
            c.pad,
            c.dilation,
            c.groups,
        );
        let gpu = g
            .conv2d(
                &x,
                &wt,
                bias.as_deref(),
                c.stride,
                c.pad,
                c.dilation,
                c.groups,
            )
            .unwrap();
        let e = norm_rel_err(&gpu, &cpu.data);
        eprintln!("conv {}：输出 {:?}，相对误差 {e:.2e}", c.name, cpu.shape);
        assert!(e <= TOL, "conv {} 相对误差 {e:.2e}", c.name);
    }
}

#[test]
fn activations_match_cpu() {
    let Some(g) = gpu() else { return };
    let mut xs: Vec<f32> = (0..=4800).map(|i| (i as f32 - 2400.0) / 200.0).collect(); // −12 … 12，步长 0.005
    xs.extend([
        0.0,
        -0.0,
        1e-30,
        -1e-30,
        3.0,
        -3.0,
        50.0,
        -50.0,
        87.0,
        -87.0,
        100.0,
        -100.0,
        f32::MAX,
        f32::MIN,
    ]);
    for act in [
        Activation::Relu,
        Activation::Silu,
        Activation::Gelu,
        Activation::HardSigmoid,
        Activation::Sigmoid,
    ] {
        let cpu: Vec<f32> = xs.iter().map(|&v| act.apply(v)).collect();
        let mut gpu = xs.clone();
        g.activate(&mut gpu, act).unwrap();
        let mut worst = (0f64, 0f32);
        for ((&gv, &cv), &x) in gpu.iter().zip(&cpu).zip(&xs) {
            let e = if gv == cv {
                0.0
            } else {
                (gv as f64 - cv as f64).abs() / (cv.abs() as f64).max(1e-30)
            };
            if e > worst.0 {
                worst = (e, x);
            }
        }
        eprintln!(
            "{act:?}：逐元素最大相对误差 {:.2e}（x = {}）",
            worst.0, worst.1
        );
        assert!(
            worst.0 <= TOL,
            "{act:?} 在 x = {} 相对误差 {:.2e}",
            worst.1,
            worst.0
        );
    }
}

#[test]
fn bias_then_activation_on_device() {
    let Some(g) = gpu() else { return };
    let mut rng = Rng(7);
    let x = rng.tensor(vec![5, 3, 7]);
    let bias: Vec<f32> = (0..5).map(|_| rng.next()).collect();
    let mut want = x.clone();
    for (i, v) in want.data.iter_mut().enumerate() {
        *v += bias[i / 21];
    }
    Activation::Silu.apply_all(&mut want);
    let mut d = g.upload(&x.data).unwrap();
    g.add_bias_device(&mut d, &g.upload(&bias).unwrap(), 21)
        .unwrap();
    g.activate_device(&mut d, Activation::Silu).unwrap();
    let e = norm_rel_err(&d.to_vec().unwrap(), &want.data);
    assert!(e <= TOL, "加偏置再激活 相对误差 {e:.2e}");
}

/// `info()` 和不载内核的 `probe()` 看到的是同一块卡；不存在的序号要带原因报错。
#[test]
fn info_and_probe_agree() {
    let Some(g) = gpu() else { return };
    let (a, b) = (g.info().unwrap(), pagesift_gpu::probe(0).unwrap());
    assert_eq!(a.name, b.name);
    assert_eq!(a.compute_capability, b.compute_capability);
    assert_eq!(a.memory_total, b.memory_total);
    assert_eq!(a.ptx, b.ptx);
    assert!(a.memory_total >= a.memory_free && a.memory_free > 0);
    assert!(a.cuda_version.0 > 0);
    let e = pagesift_gpu::probe(9999).unwrap_err();
    assert!(e.contains("9999"), "{e}");
    assert!(Gpu::open_index(9999).is_err());
}

/// 透明分流：模型直接调 `ops`，大算子按实测开销走 GPU，小算子留在 CPU，结果都和 CPU 一致。
#[test]
fn backend_offloads_by_measured_cost() {
    if gpu().is_none() {
        return;
    }
    let worker = std::sync::Arc::new(pagesift_gpu::Worker::spawn(0).unwrap());
    let b = pagesift_gpu::GpuBackend::new(worker).unwrap();
    let c = b.costs();
    eprintln!("实测开销：{c:?}");
    assert!(c.latency > 0.0 && c.up > 0.0 && c.down > 0.0, "{c:?}");
    let mut rng = Rng(0x1234_5678_9abc_def1);

    // 公式编码器全局注意力的形状（48² 个 token、线性层 768 维）
    let (big_a, big_b) = (rng.tensor(vec![2304, 768]), rng.tensor(vec![768, 2304]));
    let (small_a, small_b) = (rng.tensor(vec![4, 8]), rng.tensor(vec![8, 4]));
    let x = rng.tensor(vec![128, 256, 256]);
    let w = rng.tensor(vec![128, 128, 3, 3]);
    let bias: Vec<f32> = (0..128).map(|_| rng.next()).collect();

    let cpu_big = ops::matmul(&big_a, &big_b);
    let cpu_small = ops::matmul(&small_a, &small_b);
    let cpu_conv = ops::conv2d(&x, &w, Some(&bias), (1, 1), (1, 1), (1, 1), 1);
    let (got_big, got_small, got_conv) = pagesift_infer::backend::scoped(&b, || {
        let before = b.offloaded();
        let s = ops::matmul(&small_a, &small_b);
        assert_eq!(b.offloaded(), before, "这么小的矩阵乘不该走 GPU");
        let big = ops::matmul(&big_a, &big_b);
        // 权重走两次：第二次命中内容哈希缓存，结果也要一样
        let first = ops::conv2d(&x, &w, Some(&bias), (1, 1), (1, 1), (1, 1), 1);
        let second = ops::conv2d(&x, &w, Some(&bias), (1, 1), (1, 1), (1, 1), 1);
        assert_eq!(first.data, second.data);
        (big, s, first)
    });
    assert_eq!(b.last_error(), None);
    assert_eq!(got_small.data, cpu_small.data);
    assert!(norm_rel_err(&got_big.data, &cpu_big.data) <= TOL);
    assert!(norm_rel_err(&got_conv.data, &cpu_conv.data) <= TOL);
    eprintln!("走 GPU 的算子：{}", b.offloaded());
    if c.gemm_on_gpu((2304, 2304, 768), false) {
        assert!(b.offloaded() > 0, "开销模型说该走 GPU，实际没走");
    }
}
