//! 显存里的批量执行器：实现 `pagesift_infer::exec::Exec`，模型的一次前向全程留在显存，只在两头传输。
//!
//! **拼批和结果一致。** 一批 N 个样本存成 `[N, C, H, W]`，W 是这批最宽的；每个句柄记着各样本的
//! 有效宽度。CPU 单张推理时，卷积窗口越过右边界读到的是 0，所以这里每层卷积、池化之后把有效宽度以外
//! 的列清零（`zero_cols`），下一层读到的就和单张时的补边完全一样；全局平均只算有效列；注意力的
//! softmax 只在有效长度上做。这样拼批的结果和单张只差浮点舍入，而不是 PaddleOCR 式的“补到一样宽”
//! （那种做法结果随同批的其他样本变化）。
//!
//! **出错不 panic。** 分配或启动失败后记下第一个错误，之后的算子只维护形状、不再动显存，
//! 模型代码不用到处处理 `Result`，整批在 [`BatchExec::finish`] 时报告。
//!
//! **试算。** [`Ctx::dry`] 走同样的代码但不碰显存，只记峰值，批大小就是这样按空闲显存推出来的。

use std::any::Any;
use std::cell::{Cell, RefCell};
use std::collections::HashMap;
use std::rc::Rc;
use std::sync::Arc;

use pagesift_infer::{Conv2d, Tensor};

use crate::Gpu;
use crate::buffer::{Args, DeviceBuffer};
use crate::driver::{DevPtr, Handle};
use crate::ops::{BLOCK_1D, blocks_1d};
use crate::pool::{OOM_TAG, Pool, bytes, capacity_class};

/// 拥有权重的对象（模型）。权重缓存按地址查，缓存项持有它，地址在缓存存续期间就一直有效。
pub type Owner = Arc<dyn Any + Send + Sync>;

/// 一个权重缓存项。
struct Cached<'g> {
    buf: DeviceBuffer<'g>,
    /// 只为保活。
    _owner: Owner,
}

/// 工人线程上的长期状态：显存池和权重缓存。
pub struct Ctx<'g> {
    pub(crate) gpu: &'g Gpu,
    pub(crate) pool: RefCell<Pool<'g>>,
    weights: RefCell<HashMap<(usize, usize), Cached<'g>>>,
    pub(crate) hashed: RefCell<HashMap<u64, DeviceBuffer<'g>>>,
    weight_bytes: Cell<u64>,
}

impl<'g> Ctx<'g> {
    pub fn new(gpu: &'g Gpu) -> Ctx<'g> {
        Ctx {
            gpu,
            pool: RefCell::new(Pool::new()),
            weights: RefCell::new(HashMap::new()),
            hashed: RefCell::new(HashMap::new()),
            weight_bytes: Cell::new(0),
        }
    }

    pub fn gpu(&self) -> &'g Gpu {
        self.gpu
    }

    /// 真正执行的批量执行器。`owner` 是拥有权重的模型。
    pub fn exec<'c>(&'c self, n: usize, owner: Owner) -> BatchExec<'c, 'g> {
        BatchExec::new(self, n, owner, false)
    }

    /// 试算：不碰显存，只记显存峰值。
    pub fn dry<'c>(&'c self, n: usize, owner: Owner) -> BatchExec<'c, 'g> {
        BatchExec::new(self, n, owner, true)
    }

    /// 自上次 [`Ctx::reset_peak`] 以来占用显存的峰值（缓冲池 + 权重，字节）。
    pub fn peak_bytes(&self) -> u64 {
        self.pool.borrow().peak() + self.weight_bytes.get()
    }

    /// 真正需要的显存峰值：在用的张量 + 权重（不含池里闲着的，字节）。
    pub fn needed_bytes(&self) -> u64 {
        self.pool.borrow().in_use_peak() + self.weight_bytes.get()
    }

    pub fn reset_peak(&self) {
        self.pool.borrow_mut().reset_peak();
    }

    /// 现在能用于工作内存的显存：驱动报告的空闲量加上池里闲着的。
    pub fn available_bytes(&self) -> Result<u64, String> {
        Ok(self.gpu.memory_free()? + self.pool.borrow().idle())
    }

    /// 把权重传上显存（已在就直接返回），`Err` 时已经试过腾出缓冲池。
    fn weight(
        &self,
        key: (usize, usize),
        make: impl FnOnce() -> Vec<f32>,
        owner: &Owner,
    ) -> Result<DevPtr, String> {
        if let Some(c) = self.weights.borrow().get(&key) {
            return Ok(c.buf.ptr());
        }
        let data = make();
        let buf = self.upload_or_trim(&data)?;
        let ptr = buf.ptr();
        self.weight_bytes
            .set(self.weight_bytes.get() + bytes(data.len()));
        self.weights.borrow_mut().insert(
            key,
            Cached {
                buf,
                _owner: owner.clone(),
            },
        );
        Ok(ptr)
    }

    /// 上传；显存不够时腾掉缓冲池的空闲部分再试一次。
    pub(crate) fn upload_or_trim(&self, data: &[f32]) -> Result<DeviceBuffer<'g>, String> {
        self.gpu.upload(data).or_else(|_| {
            self.pool.borrow_mut().trim();
            self.gpu.upload(data).map_err(|e| {
                format!(
                    "{OOM_TAG}: uploading {} bytes of weights failed ({e})",
                    bytes(data.len())
                )
            })
        })
    }
}

/// 试算时的计数。
#[derive(Default)]
struct DryStats {
    live: Cell<u64>,
    peak: Cell<u64>,
}

/// 显存里的一个批量张量。
pub struct DevTensor<'c, 'g> {
    /// 试算或出错后为 None。
    buf: Option<DeviceBuffer<'g>>,
    cap: usize,
    pool: &'c RefCell<Pool<'g>>,
    dry: Option<Rc<DryStats>>,
    pub(crate) shape: Vec<usize>,
    /// 每个样本的有效宽度（图像特征是 W，序列是 T）。
    pub(crate) widths: Rc<Vec<u32>>,
}

impl DevTensor<'_, '_> {
    /// 设备地址；试算或出错后是 0（这时不会启动内核）。
    pub(crate) fn ptr(&self) -> DevPtr {
        self.buf.as_ref().map_or(0, |b| b.ptr())
    }
}

impl Drop for DevTensor<'_, '_> {
    fn drop(&mut self) {
        if let Some(b) = self.buf.take() {
            self.pool.borrow_mut().give(self.cap, b);
        }
        if let Some(d) = &self.dry {
            d.live.set(d.live.get() - bytes(self.cap));
        }
    }
}

/// 一次批量前向的执行器。一批一个，用完检查 [`BatchExec::finish`]。
pub struct BatchExec<'c, 'g> {
    pub(crate) ctx: &'c Ctx<'g>,
    pub(crate) n: usize,
    owner: Owner,
    dry: Option<Rc<DryStats>>,
    /// 试算时还不在显存里的权重字节数。
    dry_weights: RefCell<HashMap<usize, u64>>,
    err: RefCell<Option<String>>,
    /// 各样本有效宽度上传后的显存，同样的宽度只传一次。
    masks: RefCell<HashMap<Vec<u32>, DeviceBuffer<'g>>>,
}

impl Drop for BatchExec<'_, '_> {
    fn drop(&mut self) {
        // 这一批没用到的容量档不留：下一批宽度相近时还能复用，差得远的不再占着显存
        if self.dry.is_none() {
            self.ctx.pool.borrow_mut().end_batch();
        }
    }
}

impl<'c, 'g> BatchExec<'c, 'g> {
    fn new(ctx: &'c Ctx<'g>, n: usize, owner: Owner, dry: bool) -> BatchExec<'c, 'g> {
        BatchExec {
            ctx,
            n,
            owner,
            dry: dry.then(Rc::default),
            dry_weights: RefCell::new(HashMap::new()),
            err: RefCell::new(None),
            masks: RefCell::new(HashMap::new()),
        }
    }

    /// 还能不能真的动显存。
    pub(crate) fn live(&self) -> bool {
        self.dry.is_none() && self.err.borrow().is_none()
    }

    /// 记下第一个错误。
    pub(crate) fn fail(&self, e: String) {
        self.err.borrow_mut().get_or_insert(e);
    }

    pub(crate) fn check(&self, r: Result<(), String>) {
        if let Err(e) = r {
            self.fail(e);
        }
    }

    /// 等已提交的工作跑完，报告这一批有没有出错。
    pub fn finish(&self) -> Result<(), String> {
        if self.live() {
            self.check(self.ctx.gpu.synchronize());
        }
        match self.err.borrow().as_ref() {
            Some(e) => Err(e.clone()),
            None => Ok(()),
        }
    }

    /// 试算的结果：(工作内存峰值, 还没上显存的权重)，字节。真正执行时恒为 0。
    pub fn dry_usage(&self) -> (u64, u64) {
        let peak = self.dry.as_ref().map_or(0, |d| d.peak.get());
        (peak, self.dry_weights.borrow().values().sum())
    }

    /// 分配一个张量。`len` 超过 u32 范围也当显存不足：内核的下标是 u32，减小批次就能跑。
    pub(crate) fn alloc(
        &self,
        len: usize,
        shape: Vec<usize>,
        widths: Rc<Vec<u32>>,
    ) -> DevTensor<'c, 'g> {
        let cap = capacity_class(len);
        if len > u32::MAX as usize {
            self.fail(format!(
                "{OOM_TAG}: tensor of {len} elements exceeds the kernels' u32 indices"
            ));
        }
        let mut t = DevTensor {
            buf: None,
            cap,
            pool: &self.ctx.pool,
            dry: None,
            shape,
            widths,
        };
        if let Some(d) = &self.dry {
            d.live.set(d.live.get() + bytes(cap));
            d.peak.set(d.peak.get().max(d.live.get()));
            t.dry = Some(d.clone());
        } else if self.live() {
            match self.ctx.pool.borrow_mut().take(self.ctx.gpu, cap) {
                Ok(b) => t.buf = Some(b),
                Err(e) => self.fail(e),
            }
        }
        t
    }

    /// 权重的设备地址；试算时只累计大小。
    pub(crate) fn weight(&self, data: &[f32]) -> DevPtr {
        self.weight_keyed(data.as_ptr() as usize, data.len(), || data.to_vec())
    }

    /// 卷积权重重排成 `[kh, kw, o, c]`（隐式卷积用），缓存键是原权重地址加 1（f32 地址 4 字节对齐，不会撞）。
    pub(crate) fn weight_kernel_first(&self, l: &Conv2d) -> DevPtr {
        let w = &l.weight;
        let (o, c, kh, kw) = (w.shape[0], w.shape[1], w.shape[2], w.shape[3]);
        self.weight_keyed(w.data.as_ptr() as usize + 1, w.data.len(), || {
            let mut out = vec![0f32; w.data.len()];
            for ko in 0..kh * kw {
                for oc in 0..o {
                    for ic in 0..c {
                        out[(ko * o + oc) * c + ic] = w.data[(oc * c + ic) * kh * kw + ko];
                    }
                }
            }
            out
        })
    }

    fn weight_keyed(&self, addr: usize, len: usize, make: impl FnOnce() -> Vec<f32>) -> DevPtr {
        let key = (addr, len);
        if self.dry.is_some() {
            if !self.ctx.weights.borrow().contains_key(&key) {
                self.dry_weights.borrow_mut().insert(addr, bytes(len));
            }
            return 0;
        }
        if !self.live() {
            return 0;
        }
        self.ctx.weight(key, make, &self.owner).unwrap_or_else(|e| {
            self.fail(e);
            0
        })
    }

    /// 上传一个样本（批大小必须是 1），有效宽度就是实际宽度。
    pub fn upload(&self, x: &Tensor) -> DevTensor<'c, 'g> {
        assert_eq!(self.n, 1, "upload 只用于批大小 1，拼批用 upload_batch");
        let width = *x.shape.last().expect("张量至少一维") as u32;
        // 序列 [T, C] 的有效长度是 T
        let width = if x.shape.len() == 2 {
            x.shape[0] as u32
        } else {
            width
        };
        let t = self.alloc(x.data.len(), x.shape.clone(), Rc::new(vec![width]));
        self.write(&t, &x.data);
        t
    }

    /// 不带数据的张量（试算时当输入用）；有效宽度取最后一维。
    pub fn placeholder(&self, shape: Vec<usize>) -> DevTensor<'c, 'g> {
        let width = *shape.last().expect("张量至少一维") as u32;
        self.alloc(
            self.n * shape.iter().product::<usize>(),
            shape,
            Rc::new(vec![width; self.n]),
        )
    }

    /// 上传一批 `[C, H, w_i]`（C、H 相同），右边补 0 到最宽的宽度，并记下各样本的有效宽度。
    pub fn upload_batch(&self, xs: &[&Tensor]) -> DevTensor<'c, 'g> {
        assert_eq!(xs.len(), self.n, "样本数和批大小不同");
        let (c, h, _) = xs[0].chw();
        let widths: Vec<u32> = xs.iter().map(|x| x.chw().2 as u32).collect();
        let wmax = *widths.iter().max().expect("空批") as usize;
        let t = self.alloc(
            self.n * c * h * wmax,
            vec![c, h, wmax],
            Rc::new(widths.clone()),
        );
        if self.live() {
            let mut host = vec![0f32; self.n * c * h * wmax];
            for (z, (x, &w)) in xs.iter().zip(&widths).enumerate() {
                assert_eq!(x.chw().0, c, "同一批的通道数不同");
                assert_eq!(x.chw().1, h, "同一批的高度不同");
                let (w, base) = (w as usize, z * c * h * wmax);
                for r in 0..c * h {
                    let src = &x.data[r * w..(r + 1) * w];
                    host[base + r * wmax..base + r * wmax + w].copy_from_slice(src);
                }
            }
            self.write(&t, &host);
        }
        t
    }

    fn write(&self, t: &DevTensor<'_, 'g>, data: &[f32]) {
        if let (true, Some(b)) = (self.live(), t.buf.as_ref()) {
            self.check(b.write_prefix(data));
        }
    }

    /// 下载整个批量张量（行优先，含补边）；出错或试算时返回错误或空。
    pub fn download(&self, t: &DevTensor<'_, 'g>) -> Result<Vec<f32>, String> {
        self.finish()?;
        let mut v = vec![0f32; self.n * t.shape.iter().product::<usize>()];
        if let Some(b) = t.buf.as_ref() {
            b.read_prefix(&mut v)?;
        }
        Ok(v)
    }

    /// 一维内核：`total` 个工作项，每块 256 线程。
    pub(crate) fn d1(&self, f: Handle, total: usize, args: Args) {
        if self.live() {
            self.check(
                self.ctx
                    .gpu
                    .launch(f, (blocks_1d(total), 1, 1), (BLOCK_1D, 1, 1), args),
            );
        }
    }

    /// 各样本有效宽度的设备地址。
    pub(crate) fn widths_ptr(&self, w: &[u32]) -> DevPtr {
        if !self.live() {
            return 0;
        }
        if let Some(b) = self.masks.borrow().get(w) {
            return b.ptr();
        }
        let bits: Vec<f32> = w.iter().map(|&v| f32::from_bits(v)).collect();
        match self.ctx.upload_or_trim(&bits) {
            Ok(b) => {
                let p = b.ptr();
                self.masks.borrow_mut().insert(w.to_vec(), b);
                p
            }
            Err(e) => {
                self.fail(e);
                0
            }
        }
    }

    /// 把有效宽度以外的列清零（`x` 是 `[C, H, W]`）。各样本一样宽时什么也不做。
    pub(crate) fn mask(&self, x: &DevTensor<'_, 'g>) {
        let (c, h, w) = (x.shape[0], x.shape[1], x.shape[2]);
        if x.widths.iter().all(|&v| v as usize == w) {
            return;
        }
        let per = c * h * w;
        let args = Args::default()
            .raw_ptr(x.ptr())
            .raw_ptr(self.widths_ptr(&x.widths))
            .u32(per)
            .u32(w)
            .u32(self.n * per);
        self.d1(self.ctx.gpu.k.zero_cols, self.n * per, args);
    }
}
