//! GPU 对 CPU 朴素实现（`pagesift_infer::ops`）的测速：SGEMM 与几种 OCR 模型里常见的卷积形状。
//!
//!   cargo run --release -p pagesift-gpu --example gpu_bench
//!
//! 每项给三个时间：CPU（单线程朴素实现，预热 1 次后取 3 次平均）；GPU 内核（输入、权重、输出已在显存，
//! CUDA 事件计时，预热 1 次后取 20 次平均；卷积含 im2col 临时显存的分配与释放）；GPU 端到端（含上传输入与权重、下载结果，墙钟计时，
//! 预热 1 次后取 5 次平均）。倍数 = CPU / GPU。

use std::time::Instant;

use pagesift_gpu::{ConvShape, Gpu};
use pagesift_infer::{Tensor, ops};

struct Rng(u64);

impl Rng {
    fn tensor(&mut self, shape: Vec<usize>) -> Tensor {
        let n = shape.iter().product();
        let data = (0..n)
            .map(|_| {
                self.0 ^= self.0 << 13;
                self.0 ^= self.0 >> 7;
                self.0 ^= self.0 << 17;
                (self.0 >> 40) as f32 / (1u64 << 24) as f32 - 0.5
            })
            .collect();
        Tensor::new(shape, data)
    }
}

/// 预热一次后跑 `reps` 次，返回平均毫秒。
fn wall_ms(reps: usize, mut f: impl FnMut()) -> f64 {
    f();
    let t = Instant::now();
    for _ in 0..reps {
        f();
    }
    t.elapsed().as_secs_f64() * 1e3 / reps as f64
}

/// 预热一次后跑 `reps` 次，用 CUDA 事件量 GPU 上的平均毫秒。
fn gpu_ms(g: &Gpu, reps: usize, mut f: impl FnMut()) -> f64 {
    f();
    g.synchronize().unwrap();
    let t = g.timer().unwrap();
    for _ in 0..reps {
        f();
    }
    t.stop_ms().unwrap() as f64 / reps as f64
}

fn row(name: &str, gflop: f64, cpu: f64, kern: f64, e2e: f64) {
    println!(
        "| {name} | {cpu:.2} | {kern:.3} | {e2e:.2} | {:.0} | {:.0} | {:.1} | {:.0} |",
        cpu / kern,
        cpu / e2e,
        gflop / (cpu / 1e3),
        gflop / (kern / 1e3),
    );
}

fn main() {
    let g = match Gpu::open() {
        Ok(g) => g,
        Err(e) => {
            println!("没有可用的 GPU：{e}");
            return;
        }
    };
    println!("GPU：{}（算力 {:?}）\n", g.name(), g.compute_capability());
    println!(
        "| 项目 | CPU ms | GPU 内核 ms | GPU 端到端 ms | 倍数（内核） | 倍数（端到端） | CPU GFLOPS | GPU GFLOPS |"
    );
    println!("|---|---:|---:|---:|---:|---:|---:|---:|");
    let mut rng = Rng(0x9e37_79b9_7f4a_7c15);

    for s in [512, 1024, 2048] {
        let a = rng.tensor(vec![s, s]);
        let b = rng.tensor(vec![s, s]);
        let gflop = 2.0 * (s as f64).powi(3) / 1e9;
        let cpu = wall_ms(3, || drop(ops::matmul(&a, &b)));
        let (da, db) = (g.upload(&a.data).unwrap(), g.upload(&b.data).unwrap());
        let mut dc = g.alloc(s * s).unwrap();
        let kern = gpu_ms(&g, 20, || {
            g.gemm_device(&da, &db, &mut dc, s, s, s).unwrap()
        });
        let e2e = wall_ms(5, || drop(g.matmul(&a, &b).unwrap()));
        row(&format!("SGEMM {s}³"), gflop, cpu, kern, e2e);
    }

    // (说明, [C, H, W], [O, kh, kw], stride, pad, groups)
    type Conv = (
        &'static str,
        [usize; 3],
        [usize; 3],
        (usize, usize),
        (usize, usize),
        usize,
    );
    let convs: [Conv; 6] = [
        (
            "3×3 s2 首层 3→32 @640²",
            [3, 640, 640],
            [32, 3, 3],
            (2, 2),
            (1, 1),
            1,
        ),
        (
            "1×1 64→128 @160²",
            [64, 160, 160],
            [128, 1, 1],
            (1, 1),
            (0, 0),
            1,
        ),
        (
            "3×3 64→64 @80²",
            [64, 80, 80],
            [64, 3, 3],
            (1, 1),
            (1, 1),
            1,
        ),
        (
            "3×3 s(2,1) 64→128 @48×320",
            [64, 48, 320],
            [128, 3, 3],
            (2, 1),
            (1, 1),
            1,
        ),
        (
            "深度 3×3 128 @160²",
            [128, 160, 160],
            [128, 3, 3],
            (1, 1),
            (1, 1),
            128,
        ),
        (
            "深度 7×7 96 @80²",
            [96, 80, 80],
            [96, 7, 7],
            (1, 1),
            (3, 3),
            96,
        ),
    ];
    for (name, [c, h, w], [o, kh, kw], stride, pad, groups) in convs {
        let x = rng.tensor(vec![c, h, w]);
        let wt = rng.tensor(vec![o, c / groups, kh, kw]);
        let bias: Vec<f32> = rng.tensor(vec![o]).data;
        let s = ConvShape {
            c,
            h,
            w,
            o,
            kh,
            kw,
            stride,
            pad,
            dilation: (1, 1),
            groups,
        };
        let (oh, ow) = s.out_hw();
        let gflop = 2.0 * (o * oh * ow * (c / groups) * kh * kw) as f64 / 1e9;
        let cpu = wall_ms(3, || {
            drop(ops::conv2d(
                &x,
                &wt,
                Some(&bias),
                stride,
                pad,
                (1, 1),
                groups,
            ))
        });
        let (dx, dw, db) = (
            g.upload(&x.data).unwrap(),
            g.upload(&wt.data).unwrap(),
            g.upload(&bias).unwrap(),
        );
        let mut out = g.alloc(o * oh * ow).unwrap();
        let kern = gpu_ms(&g, 20, || {
            g.conv2d_device_into(&dx, &dw, Some(&db), &s, &mut out)
                .unwrap()
        });
        let e2e = wall_ms(5, || {
            drop(
                g.conv2d(&x, &wt, Some(&bias), stride, pad, (1, 1), groups)
                    .unwrap(),
            )
        });
        row(name, gflop, cpu, kern, e2e);
    }
}
