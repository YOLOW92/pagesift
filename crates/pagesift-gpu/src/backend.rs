//! 透明分流：实现 `pagesift_infer::backend::Backend`，让直接调 `ops`、`nn` 的模型（公式、版面）
//! 不改代码就能把大算子放到 GPU 上。
//!
//! 划不划算当场算：CPU 时间 = 乘加次数 / 实测 CPU 算力；GPU 时间 = 实测往返延迟 + 传输字节 / 实测带宽 +
//! 乘加次数 / 实测 GPU 算力。门槛不是写死的数，换一张卡或一台机器自动变。权重传过一次就留在显存里，
//! 按内容哈希认（地址可能被别的张量复用），所以长期不变的权重不算传输开销。

use std::sync::Mutex;
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::Instant;

use pagesift_infer::backend::Backend;
use pagesift_infer::{Activation, Tensor, ops};

use crate::exec::Ctx;
use crate::ops::ConvShape;
use crate::pool::{bytes, capacity_class};
use crate::worker::SharedWorker;

/// [策略] 校准用的矩阵边长：512³ ≈ 1.3 亿次乘加，两边都是毫秒级，足够盖过计时噪声又不拖慢启动
const CAL_GEMM: usize = 512;
/// [策略] 校准传输用的元素数：2²¹ 个 f32 = 8 MiB，和卷积特征图同一量级
const CAL_TRANSFER: usize = 1 << 21;
/// [策略] 校准卷积的通道数和边长：和检测骨干的中段特征同一量级
const CAL_CONV_C: usize = 64;
/// [策略] 见 `CAL_CONV_C`
const CAL_CONV_HW: usize = 256;
/// [推导] 校准卷积的核边长：骨干里最常见的 3×3
const CAL_CONV_K: usize = 3;
/// [协议] MurmurHash3 fmix64 的乘数，用作内容哈希的混合常数
const HASH_MUL: u64 = 0xff51_afd7_ed55_8ccd;

/// 实测的开销，单位都是秒。
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Costs {
    /// 一次空的往返（交给工人、启动、等完成）。
    pub latency: f64,
    /// 每字节上传、下载。
    pub up: f64,
    pub down: f64,
    /// GPU 上每次乘加：SGEMM、深度卷积的直接内核；im2col 每个元素。
    pub gpu_gemm: f64,
    pub gpu_direct: f64,
    pub gpu_col: f64,
    /// CPU 上每次乘加：矩阵乘、卷积。
    pub cpu_gemm: f64,
    pub cpu_conv: f64,
    /// 激活函数每个元素：CPU、GPU（以 GELU 为代表，它最贵）。
    pub cpu_act: f64,
    pub gpu_act: f64,
}

impl Costs {
    /// 一次矩阵乘走 GPU 是否更快。`cached_b` 表示 `b` 已在显存。
    pub fn gemm_on_gpu(&self, (m, n, k): (usize, usize, usize), cached_b: bool) -> bool {
        let mads = (m * n * k) as f64;
        let up = bytes(m * k + if cached_b { 0 } else { k * n }) as f64;
        let gpu = self.latency + up * self.up + bytes(m * n) as f64 * self.down;
        gpu + mads * self.gpu_gemm < mads * self.cpu_gemm
    }

    /// 一次卷积走 GPU 是否更快。`cached_w` 表示权重和偏置已在显存。
    pub fn conv_on_gpu(&self, s: &ConvShape, cached_w: bool) -> bool {
        let (oh, ow) = s.out_hw();
        let cg = s.c / s.groups;
        let mads = (s.o * oh * ow * cg * s.kh * s.kw) as f64;
        let wlen = if cached_w { 0 } else { s.o * cg * s.kh * s.kw };
        let up = bytes(s.c * s.h * s.w + wlen) as f64;
        let run = if cg == 1 {
            mads * self.gpu_direct
        } else if s.needs_col() {
            mads * self.gpu_gemm + s.col_len() as f64 * self.gpu_col
        } else {
            mads * self.gpu_gemm
        };
        let gpu = self.latency + up * self.up + bytes(s.o * oh * ow) as f64 * self.down;
        gpu + run < mads * self.cpu_conv
    }

    /// 对 `n` 个元素做激活走 GPU 是否更快。
    pub fn act_on_gpu(&self, n: usize) -> bool {
        let n_bytes = bytes(n) as f64;
        let gpu = self.latency + n_bytes * (self.up + self.down) + n as f64 * self.gpu_act;
        gpu < n as f64 * self.cpu_act
    }
}

/// 内容哈希，只用于认出“同一份权重”。
fn content_hash(data: &[f32]) -> u64 {
    let mut h = data.len() as u64;
    for v in data {
        h = (h ^ u64::from(v.to_bits())).wrapping_mul(HASH_MUL);
        h ^= h >> 29; // [协议] fmix64 的移位量（高位折回低位，避免只有高位变化）
    }
    h
}

/// 透明分流的后端。
pub struct GpuBackend {
    worker: SharedWorker,
    costs: Costs,
    offloaded: AtomicU64,
    last_error: Mutex<Option<String>>,
}

impl GpuBackend {
    /// 校准开销（几十毫秒）并建好后端。
    pub fn new(worker: SharedWorker) -> Result<GpuBackend, String> {
        let costs = calibrate(&worker)?;
        Ok(GpuBackend {
            worker,
            costs,
            offloaded: AtomicU64::new(0),
            last_error: Mutex::new(None),
        })
    }

    /// 校准出来的开销。
    pub fn costs(&self) -> Costs {
        self.costs
    }

    /// 走了 GPU 的算子个数。
    pub fn offloaded(&self) -> u64 {
        self.offloaded.load(Ordering::Relaxed)
    }

    /// 最近一次 GPU 出错的原因（出错的算子已自动在 CPU 上重算）。
    pub fn last_error(&self) -> Option<String> {
        self.last_error.lock().ok().and_then(|g| g.clone())
    }

    /// 执行一个算子；出错时记下原因并返回 None，调用方退回 CPU。
    fn run<R: Send>(
        &self,
        f: impl for<'c, 'g> FnOnce(&'c Ctx<'g>) -> Result<R, String> + Send,
    ) -> Option<R> {
        match self.worker.call(f).and_then(|r| r) {
            Ok(r) => {
                self.offloaded.fetch_add(1, Ordering::Relaxed);
                Some(r)
            }
            Err(e) => {
                if let Ok(mut g) = self.last_error.lock() {
                    *g = Some(e);
                }
                None
            }
        }
    }
}

impl<'g> Ctx<'g> {
    /// 按内容哈希取权重的设备地址，不在显存就传上去。
    fn hashed_weight(&self, data: &[f32], hash: u64) -> Result<u64, String> {
        if let Some(b) = self.hashed.borrow().get(&hash) {
            return Ok(b.ptr());
        }
        let b = self.upload_or_trim(data)?;
        let p = b.ptr();
        self.hashed.borrow_mut().insert(hash, b);
        Ok(p)
    }

    /// 借一块临时缓冲交给 `f`，用完还回池。
    fn with_temp<R>(
        &self,
        len: usize,
        f: impl FnOnce(&crate::DeviceBuffer<'g>) -> Result<R, String>,
    ) -> Result<R, String> {
        let cap = capacity_class(len);
        let b = self.pool.borrow_mut().take(self.gpu, cap)?;
        let r = f(&b);
        self.pool.borrow_mut().give(cap, b);
        r
    }
}

impl Backend for GpuBackend {
    fn matmul(&self, a: &Tensor, b: &Tensor, b_is_weight: bool) -> Option<Vec<f32>> {
        let (m, k) = a.tc();
        let (_, n) = b.tc();
        if !self.costs.gemm_on_gpu((m, n, k), b_is_weight) {
            return None;
        }
        let hash = b_is_weight.then(|| content_hash(&b.data));
        self.run(move |ctx: &Ctx<'_>| {
            let run = |bp: u64| {
                ctx.with_temp(m * k, |da| {
                    da.write_prefix(&a.data)?;
                    ctx.with_temp(m * n, |dc| {
                        ctx.gpu
                            .sgemm_raw(da.ptr(), bp, dc.ptr(), (m, n, k), 1, (0, 0, 0))?;
                        let mut out = vec![0f32; m * n];
                        dc.read_prefix(&mut out)?;
                        Ok(out)
                    })
                })
            };
            match hash {
                Some(h) => run(ctx.hashed_weight(&b.data, h)?),
                None => ctx.with_temp(k * n, |db| {
                    db.write_prefix(&b.data)?;
                    run(db.ptr())
                }),
            }
        })
    }

    fn conv2d(
        &self,
        x: &Tensor,
        w: &Tensor,
        bias: Option<&[f32]>,
        stride: (usize, usize),
        pad: (usize, usize),
        dilation: (usize, usize),
        groups: usize,
    ) -> Option<Vec<f32>> {
        let (c, h, wd) = x.chw();
        let s = ConvShape {
            c,
            h,
            w: wd,
            o: w.shape[0],
            kh: w.shape[2],
            kw: w.shape[3],
            stride,
            pad,
            dilation,
            groups,
        };
        if !self.costs.conv_on_gpu(&s, true) {
            return None;
        }
        let (oh, ow) = s.out_hw();
        let (hw, hb) = (content_hash(&w.data), bias.map(content_hash));
        self.run(move |ctx: &Ctx<'_>| {
            let wp = ctx.hashed_weight(&w.data, hw)?;
            let bp = match (bias, hb) {
                (Some(b), Some(h)) => Some(ctx.hashed_weight(b, h)?),
                _ => None,
            };
            let col_len = if s.needs_col() { s.col_len() } else { 0 };
            ctx.with_temp(x.data.len(), |dx| {
                dx.write_prefix(&x.data)?;
                ctx.with_temp(s.o * oh * ow, |dout| {
                    ctx.with_temp(col_len, |dcol| {
                        let col = s.needs_col().then(|| dcol.ptr());
                        ctx.gpu.conv_raw(dx.ptr(), wp, bp, &s, 1, dout.ptr(), col)?;
                        let mut out = vec![0f32; s.o * oh * ow];
                        dout.read_prefix(&mut out)?;
                        Ok(out)
                    })
                })
            })
        })
    }

    fn activate(&self, x: &mut [f32], a: Activation) -> bool {
        if !self.costs.act_on_gpu(x.len()) {
            return false;
        }
        self.run(move |ctx: &Ctx<'_>| ctx.gpu.activate(x, a))
            .is_some()
    }
}

/// 秒表：跑两次取第二次，第一次含 JIT 后的首次启动和缓存预热。
fn timed(mut f: impl FnMut() -> Result<(), String>) -> Result<f64, String> {
    f()?;
    let t = Instant::now();
    f()?;
    Ok(t.elapsed().as_secs_f64())
}

/// 量各项开销。时间再小也不让它变成 0，后面要做除数和比较。
fn calibrate(worker: &SharedWorker) -> Result<Costs, String> {
    // 一次最小的工作：交给工人、启动内核、等完成
    let latency = worker.call(|ctx: &Ctx<'_>| -> Result<f64, String> {
        let mut one = ctx.gpu.upload(&[0.5])?;
        timed(|| {
            ctx.gpu.activate_device(&mut one, Activation::Relu)?;
            ctx.gpu.synchronize()
        })
    })??;

    // 传输
    let host = vec![1f32; CAL_TRANSFER];
    let n_bytes = bytes(CAL_TRANSFER) as f64;
    let (t_up, t_down) = worker.call(|ctx: &Ctx<'_>| -> Result<(f64, f64), String> {
        let d = ctx.gpu.alloc(CAL_TRANSFER)?;
        d.write_prefix(&host)?;
        let t = Instant::now();
        d.write_prefix(&host)?;
        let up = t.elapsed().as_secs_f64();
        let mut back = vec![0f32; CAL_TRANSFER];
        let t = Instant::now();
        d.read_prefix(&mut back)?;
        Ok((up, t.elapsed().as_secs_f64()))
    })??;

    // 矩阵乘
    let n = CAL_GEMM;
    let a = Tensor::new(vec![n, n], vec![0.5; n * n]);
    let mads = (n * n * n) as f64;
    let cpu_gemm = timed(|| {
        std::hint::black_box(ops::matmul(&a, &a));
        Ok(())
    })? / mads;
    let gpu_gemm = worker.call(|ctx: &Ctx<'_>| -> Result<f64, String> {
        let (da, mut dc) = (ctx.gpu.upload(&a.data)?, ctx.gpu.alloc(n * n)?);
        timed(|| {
            ctx.gpu.gemm_device(&da, &da, &mut dc, n, n, n)?;
            ctx.gpu.synchronize()
        })
    })?? / mads;

    // 卷积：深度卷积的直接内核，和普通卷积（im2col + SGEMM）里 im2col 的部分
    let (c, hw, k) = (CAL_CONV_C, CAL_CONV_HW, CAL_CONV_K);
    let x = Tensor::new(vec![c, hw, hw], vec![0.5; c * hw * hw]);
    // [策略] 权重取什么值不影响耗时，取个不会溢出的小数
    let weight = 0.1;
    let wd = Tensor::new(vec![c, 1, k, k], vec![weight; c * k * k]);
    let wf = Tensor::new(vec![c, c, k, k], vec![weight; c * c * k * k]);
    let shape = |groups| ConvShape {
        c,
        h: hw,
        w: hw,
        o: c,
        kh: k,
        kw: k,
        stride: (1, 1),
        pad: (k / 2, k / 2),
        dilation: (1, 1),
        groups,
    };
    let mads_full = (c * hw * hw * c * k * k) as f64;
    let mads_direct = (c * hw * hw * k * k) as f64;
    let cpu_conv = timed(|| {
        std::hint::black_box(ops::conv2d(
            &x,
            &wf,
            None,
            (1, 1),
            (k / 2, k / 2),
            (1, 1),
            1,
        ));
        Ok(())
    })? / mads_full;
    let (gpu_direct, t_full, col_len) = worker.call(|ctx: &Ctx<'_>| {
        let run = |w: &Tensor, s: ConvShape| -> Result<f64, String> {
            let (dx, dw) = (ctx.gpu.upload(&x.data)?, ctx.gpu.upload(&w.data)?);
            timed(|| {
                ctx.gpu.conv2d_device(&dx, &dw, None, &s).map(drop)?;
                ctx.gpu.synchronize()
            })
        };
        let (s1, s2) = (shape(c), shape(1));
        Ok::<_, String>((run(&wd, s1)? / mads_direct, run(&wf, s2)?, s2.col_len()))
    })??;
    let gpu_col = ((t_full - mads_full * gpu_gemm) / col_len as f64).max(0.0);

    // 激活
    let act_n = CAL_TRANSFER;
    let mut buf = Tensor::new(vec![act_n], vec![0.5; act_n]);
    let cpu_act = timed(|| {
        Activation::Gelu.apply_all(&mut buf);
        Ok(())
    })? / act_n as f64;
    let gpu_act = worker.call(|ctx: &Ctx<'_>| -> Result<f64, String> {
        let mut d = ctx.gpu.upload(&buf.data)?;
        timed(|| {
            ctx.gpu.activate_device(&mut d, Activation::Gelu)?;
            ctx.gpu.synchronize()
        })
    })?? / act_n as f64;

    Ok(Costs {
        latency,
        up: t_up / n_bytes,
        down: t_down / n_bytes,
        gpu_gemm,
        gpu_direct,
        gpu_col,
        cpu_gemm,
        cpu_conv,
        cpu_act,
        gpu_act,
    })
}
