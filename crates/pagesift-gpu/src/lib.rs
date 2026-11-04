//! NVIDIA GPU 推理后端：给 `pagesift-infer` 的重算子（矩阵乘、卷积、激活）提供同语义的 GPU 版本。
//!
//! - 不依赖 CUDA 工具链：运行时 `dlopen("libcuda.so.1")` 调驱动 API（见 `driver.rs`）；
//! - 内核是仓库里 `kernels/` 用 nightly 的 nvptx 目标编出的 PTX，`include_str!` 嵌进来，由驱动 JIT。
//!   有两份：sm_70 原版给算力 ≥ 7.0 的卡，`.target` 改成 sm_61 的一份给 Pascal（6.1、6.2）；
//! - 没有驱动、没有 N 卡、算力不够时 [`Gpu::open`] 返回错误，调用方退回 CPU。
//!
//! 结果与 `pagesift_infer::ops` 的 CPU 实现只差浮点舍入（累加顺序不同、GPU 用 FMA）。
//!
//! ```no_run
//! let gpu = pagesift_gpu::Gpu::open()?;
//! let a = pagesift_infer::Tensor::new(vec![2, 3], vec![1.0; 6]);
//! let b = pagesift_infer::Tensor::new(vec![3, 4], vec![1.0; 12]);
//! let c = gpu.matmul(&a, &b)?; // [2, 4]，行优先
//! assert_eq!(c, vec![3.0; 8]);
//! # Ok::<(), String>(())
//! ```

mod backend;
mod buffer;
mod driver;
mod exec;
mod exec_ops;
mod ops;
mod pool;
mod worker;

use std::ffi::{CStr, c_char, c_int, c_void};
use std::ptr::null_mut;

pub use backend::{Costs, GpuBackend};
pub use buffer::{DeviceBuffer, Timer};
pub use exec::{BatchExec, Ctx, DevTensor, Owner};
pub use ops::ConvShape;
pub use pool::{OOM_TAG, is_oom};
pub use worker::{SharedWorker, WaitError, Worker};

use driver::{
    ATTR_CC_MAJOR, ATTR_CC_MINOR, Driver, Handle, JIT_ERROR_LOG_BUFFER,
    JIT_ERROR_LOG_BUFFER_SIZE_BYTES,
};

/// sm_70 原版 PTX（算力 ≥ 7.0）。末尾补 NUL：`cuModuleLoadData` 要 C 字符串。
const PTX_SM70: &str = concat!(include_str!("../ptx/kernels_sm70.ptx"), "\0");
/// `.target` 改成 sm_61 的 PTX（算力 6.1、6.2）。
const PTX_SM61: &str = concat!(include_str!("../ptx/kernels_sm61.ptx"), "\0");

/// 内核函数句柄，名字与 `kernels/src` 里的 `#[no_mangle]` 函数一一对应。
pub(crate) struct Kernels {
    pub sgemm: Handle,
    pub im2col: Handle,
    pub depthwise: Handle,
    pub add_bias: Handle,
    pub relu: Handle,
    pub silu: Handle,
    pub gelu: Handle,
    pub hardsigmoid: Handle,
    pub sigmoid: Handle,
    pub se_gate: Handle,
    pub zero_cols: Handle,
    pub pad_rb: Handle,
    pub pool_max: Handle,
    pub pool_avg: Handle,
    pub global_avg: Handle,
    pub scale_ch: Handle,
    pub add2: Handle,
    pub copy_part: Handle,
    pub interp_nearest: Handle,
    pub chw_to_seq: Handle,
    pub conv_transpose: Handle,
    pub layer_norm: Handle,
    pub attn_split: Handle,
    pub attn_softmax: Handle,
    pub attn_merge: Handle,
    pub ctc_digest: Handle,
    pub pad_all: Handle,
    pub crop_cols: Handle,
}

/// 一块 N 卡上的上下文、载好的内核模块和函数表。所有方法取 `&self`，可以跨线程共享。
pub struct Gpu {
    drv: Driver,
    dev: c_int,
    index: usize,
    ctx: Handle,
    module: Handle,
    k: Kernels,
    name: String,
    cc: (i32, i32),
    ptx: &'static Ptx,
}

// SAFETY: 驱动 API 是线程安全的；上下文是设备的主上下文，每个入口先在当前线程设为当前上下文
// （`bind`），句柄本身只是驱动内部对象的不透明指针，不在 Rust 侧读写。
unsafe impl Send for Gpu {}
unsafe impl Sync for Gpu {}

/// 本机可见的 N 卡数量；没有驱动或驱动初始化失败时为 0。用来区分“没有 GPU”（该退回 CPU）和
/// “有 GPU 但 [`Gpu::open`] 失败”（PTX、驱动出了问题，应当报出来）。
pub fn device_count() -> usize {
    let Ok(drv) = Driver::load() else { return 0 };
    let mut count = 0;
    if unsafe { (drv.init)(0) } != 0 || unsafe { (drv.device_get_count)(&mut count) } != 0 {
        return 0;
    }
    count.max(0) as usize
}

/// 一块 N 卡的情况（给 doctor 和模型选择用）。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct GpuInfo {
    /// 设备序号（`cuda:N` 的 N）。
    pub index: usize,
    /// 设备名，例如 “NVIDIA GeForce GTX 1070”。
    pub name: String,
    /// 算力 (主, 次)，例如 (6, 1)。
    pub compute_capability: (i32, i32),
    /// 显存总量（字节）。
    pub memory_total: u64,
    /// 查询时的空闲显存（字节）。
    pub memory_free: u64,
    /// 真实驱动版本（来自 NVML，例如 “550.54.14”）；没有 NVML 时为 None。
    pub driver_version: Option<String>,
    /// 驱动支持的 CUDA API 版本 (主, 次)，例如 (12, 4)。
    pub cuda_version: (i32, i32),
    /// 选中的 PTX 版本：“sm_61” 或 “sm_70”。
    pub ptx: &'static str,
}

/// `PAGESIFT_GPU=off` 时返回原因字符串：这时 [`Gpu::open`]、[`Gpu::open_index`]、[`probe`] 都报错，
/// 调用方照常退回 CPU。
pub fn disabled_by_env() -> Option<String> {
    let v = std::env::var("PAGESIFT_GPU").ok()?;
    v.eq_ignore_ascii_case("off")
        .then(|| "GPU disabled by PAGESIFT_GPU=off".to_owned())
}

/// 驱动里挑出的一个设备：已 `cuInit`，序号也核对过。
struct Picked {
    drv: Driver,
    dev: c_int,
    cc: (i32, i32),
    ptx: &'static Ptx,
    name: String,
}

/// 载入驱动并取出第 `index` 块卡；不碰上下文。
fn pick(index: usize) -> Result<Picked, String> {
    if let Some(why) = disabled_by_env() {
        return Err(why);
    }
    let drv = Driver::load()?;
    drv.check(unsafe { (drv.init)(0) }, "cuInit")?;
    let mut count = 0;
    drv.check(
        unsafe { (drv.device_get_count)(&mut count) },
        "cuDeviceGetCount",
    )?;
    if count == 0 {
        return Err("no NVIDIA GPU found".into());
    }
    if index >= count as usize {
        return Err(format!("no GPU {index}: this machine has {count}"));
    }
    let mut dev = 0;
    drv.check(
        unsafe { (drv.device_get)(&mut dev, index as c_int) },
        "cuDeviceGet",
    )?;
    let attr = |a| -> Result<i32, String> {
        let mut v = 0;
        drv.check(
            unsafe { (drv.device_get_attribute)(&mut v, a, dev) },
            "cuDeviceGetAttribute",
        )?;
        Ok(v)
    };
    let cc = (attr(ATTR_CC_MAJOR)?, attr(ATTR_CC_MINOR)?);
    let ptx = select_ptx(cc)?;
    let name = device_name(&drv, dev);
    Ok(Picked {
        drv,
        dev,
        cc,
        ptx,
        name,
    })
}

/// 取当前上下文所在设备的显存 (空闲, 总量)，字节。
fn memory_info(drv: &Driver) -> Result<(u64, u64), String> {
    let (mut free, mut total) = (0usize, 0usize);
    drv.check(
        unsafe { (drv.mem_get_info)(&mut free, &mut total) },
        "cuMemGetInfo",
    )?;
    Ok((free as u64, total as u64))
}

/// 汇总当前上下文所在设备的情况。
fn build_info(
    drv: &Driver,
    index: usize,
    name: &str,
    cc: (i32, i32),
    ptx: &Ptx,
) -> Result<GpuInfo, String> {
    let (memory_free, memory_total) = memory_info(drv)?;
    let mut v = 0;
    drv.check(
        unsafe { (drv.driver_get_version)(&mut v) },
        "cuDriverGetVersion",
    )?;
    // [协议] cuDriverGetVersion 的编码：主版本 × 1000 + 次版本 × 10
    let cuda_version = (v / 1000, (v % 1000) / 10);
    Ok(GpuInfo {
        index,
        name: name.to_owned(),
        compute_capability: cc,
        memory_total,
        memory_free,
        driver_version: driver::nvml_driver_version(),
        cuda_version,
        ptx: ptx.name,
    })
}

/// 查第 `index` 块 N 卡的情况，不载内核（不 JIT，比 [`Gpu::open_index`] 轻）。
/// 没有驱动、没有这块卡、算力不够或 `PAGESIFT_GPU=off` 时，错误就是可以给用户看的原因。
pub fn probe(index: usize) -> Result<GpuInfo, String> {
    let p = pick(index)?;
    let mut ctx = null_mut();
    p.drv.check(
        unsafe { (p.drv.primary_ctx_retain)(&mut ctx, p.dev) },
        "cuDevicePrimaryCtxRetain",
    )?;
    let r = p
        .drv
        .check(unsafe { (p.drv.ctx_set_current)(ctx) }, "cuCtxSetCurrent")
        .and_then(|()| build_info(&p.drv, index, &p.name, p.cc, p.ptx));
    unsafe { (p.drv.primary_ctx_release)(p.dev) };
    r
}

impl Gpu {
    /// 打开 0 号 N 卡，同 [`Gpu::open_index`]`(0)`。
    pub fn open() -> Result<Gpu, String> {
        Gpu::open_index(0)
    }

    /// 打开第 `index` 块 N 卡：载入驱动、取主上下文、按算力挑 PTX 并 JIT。
    /// 没有驱动、没有这块卡、算力低于 6.1、JIT 失败或 `PAGESIFT_GPU=off` 时返回错误（不 panic）。
    pub fn open_index(index: usize) -> Result<Gpu, String> {
        let Picked {
            drv,
            dev,
            cc,
            ptx,
            name,
        } = pick(index)?;
        let mut ctx = null_mut();
        drv.check(
            unsafe { (drv.primary_ctx_retain)(&mut ctx, dev) },
            "cuDevicePrimaryCtxRetain",
        )?;
        // 之后出错要释放主上下文：先拼出不带模块的半成品，交给 Drop 收尾
        let mut gpu = Gpu {
            drv,
            dev,
            index,
            ctx,
            module: null_mut(),
            k: Kernels::null(),
            name,
            cc,
            ptx,
        };
        gpu.bind()?;
        gpu.module = load_module(&gpu.drv, ptx.source)?;
        gpu.k = Kernels::get(&gpu.drv, gpu.module)?;
        Ok(gpu)
    }

    /// 设备名，例如 “NVIDIA GeForce GTX 1070”。
    pub fn name(&self) -> &str {
        &self.name
    }

    /// 算力 (主, 次)，例如 (6, 1)。
    pub fn compute_capability(&self) -> (i32, i32) {
        self.cc
    }

    /// 现在的空闲显存（字节）。
    pub fn memory_free(&self) -> Result<u64, String> {
        self.bind()?;
        Ok(memory_info(&self.drv)?.0)
    }

    /// 型号、算力、显存总量和空闲量、驱动版本、选中的 PTX 版本。
    pub fn info(&self) -> Result<GpuInfo, String> {
        self.bind()?;
        build_info(&self.drv, self.index, &self.name, self.cc, self.ptx)
    }

    /// 等设备上已提交的工作全部完成。
    pub fn synchronize(&self) -> Result<(), String> {
        self.bind()?;
        self.drv
            .check(unsafe { (self.drv.ctx_synchronize)() }, "cuCtxSynchronize")
    }

    /// 把本设备的主上下文设为当前线程的当前上下文。每个公开入口先调它。
    pub(crate) fn bind(&self) -> Result<(), String> {
        self.drv.check(
            unsafe { (self.drv.ctx_set_current)(self.ctx) },
            "cuCtxSetCurrent",
        )
    }
}

impl Drop for Gpu {
    fn drop(&mut self) {
        // 出错也只能忽略：Drop 里没有地方报
        let _ = self.bind();
        if !self.module.is_null() {
            unsafe { (self.drv.module_unload)(self.module) };
        }
        unsafe { (self.drv.primary_ctx_release)(self.dev) };
    }
}

/// 一份嵌入的 PTX。
struct Ptx {
    name: &'static str,
    source: &'static str,
}

static SM70: Ptx = Ptx {
    name: "sm_70",
    source: PTX_SM70,
};
static SM61: Ptx = Ptx {
    name: "sm_61",
    source: PTX_SM61,
};

/// 按算力挑 PTX：≥ 7.0 用 sm_70 原版；6.1、6.2 用改过文件头的 sm_61 版；更老的不支持
/// （.target sm_61 的 PTX 在 6.0 的卡上 JIT 会失败）。
fn select_ptx(cc: (i32, i32)) -> Result<&'static Ptx, String> {
    match cc {
        // [协议] PTX ISA：sm_70 是 Volta 的算力 7.0；sm_61 覆盖算力 6.1 与 6.2
        (major, _) if major >= 7 => Ok(&SM70),
        (6, minor) if minor >= 1 => Ok(&SM61),
        (major, minor) => Err(format!(
            "GPU compute capability {major}.{minor} is too old, need at least 6.1 (Pascal)"
        )),
    }
}

fn device_name(drv: &Driver, dev: c_int) -> String {
    let mut buf = [0 as c_char; 256]; // [策略] 设备名缓冲，驱动给的名字远短于此
    let r = unsafe { (drv.device_get_name)(buf.as_mut_ptr(), buf.len() as c_int, dev) };
    if r != 0 {
        return "unknown NVIDIA GPU".into();
    }
    unsafe { CStr::from_ptr(buf.as_ptr()) }
        .to_string_lossy()
        .into_owned()
}

/// JIT 载入 PTX；失败时把驱动的 JIT 错误日志带进错误信息。
fn load_module(drv: &Driver, ptx: &str) -> Result<Handle, String> {
    let mut log = vec![0u8; 16 * 1024]; // [策略] JIT 错误日志缓冲，16 KiB 足够容下几十行报错
    let mut opts = [JIT_ERROR_LOG_BUFFER, JIT_ERROR_LOG_BUFFER_SIZE_BYTES];
    // 驱动约定：选项值按指针宽度传，缓冲大小也塞在指针里
    let mut vals = [log.as_mut_ptr() as *mut c_void, log.len() as *mut c_void];
    let mut module = null_mut();
    let r = unsafe {
        (drv.module_load_data_ex)(
            &mut module,
            ptx.as_ptr() as *const c_void,
            opts.len() as u32,
            opts.as_mut_ptr(),
            vals.as_mut_ptr(),
        )
    };
    if let Err(e) = drv.check(r, "cuModuleLoadDataEx") {
        let text = CStr::from_bytes_until_nul(&log)
            .map(|c| c.to_string_lossy().into_owned())
            .unwrap_or_default();
        return Err(format!("{e}; JIT log: {}", text.trim()));
    }
    Ok(module)
}

impl Kernels {
    fn null() -> Kernels {
        let n = null_mut();
        Kernels {
            sgemm: n,
            im2col: n,
            depthwise: n,
            add_bias: n,
            relu: n,
            silu: n,
            gelu: n,
            hardsigmoid: n,
            sigmoid: n,
            se_gate: n,
            zero_cols: n,
            pad_rb: n,
            pool_max: n,
            pool_avg: n,
            global_avg: n,
            scale_ch: n,
            add2: n,
            copy_part: n,
            interp_nearest: n,
            chw_to_seq: n,
            conv_transpose: n,
            layer_norm: n,
            attn_split: n,
            attn_softmax: n,
            attn_merge: n,
            ctc_digest: n,
            pad_all: n,
            crop_cols: n,
        }
    }

    fn get(drv: &Driver, module: Handle) -> Result<Kernels, String> {
        let f = |name: &CStr| -> Result<Handle, String> {
            let mut h = null_mut();
            let what = format!("cuModuleGetFunction({})", name.to_string_lossy());
            drv.check(
                unsafe { (drv.module_get_function)(&mut h, module, name.as_ptr()) },
                &what,
            )?;
            Ok(h)
        };
        Ok(Kernels {
            sgemm: f(c"sgemm")?,
            im2col: f(c"im2col")?,
            depthwise: f(c"depthwise")?,
            add_bias: f(c"add_bias")?,
            relu: f(c"relu")?,
            silu: f(c"silu")?,
            gelu: f(c"gelu")?,
            hardsigmoid: f(c"hardsigmoid")?,
            sigmoid: f(c"sigmoid")?,
            se_gate: f(c"se_gate")?,
            zero_cols: f(c"zero_cols")?,
            pad_rb: f(c"pad_rb")?,
            pool_max: f(c"pool_max")?,
            pool_avg: f(c"pool_avg")?,
            global_avg: f(c"global_avg")?,
            scale_ch: f(c"scale_ch")?,
            add2: f(c"add2")?,
            copy_part: f(c"copy_part")?,
            interp_nearest: f(c"interp_nearest")?,
            chw_to_seq: f(c"chw_to_seq")?,
            conv_transpose: f(c"conv_transpose")?,
            layer_norm: f(c"layer_norm")?,
            attn_split: f(c"attn_split")?,
            attn_softmax: f(c"attn_softmax")?,
            attn_merge: f(c"attn_merge")?,
            ctc_digest: f(c"ctc_digest")?,
            pad_all: f(c"pad_all")?,
            crop_cols: f(c"crop_cols")?,
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// PTX 编译失败要返回错误并带 JIT 日志（调用方据此退回 CPU），不能 panic。没有驱动时跳过。
    #[test]
    fn bad_ptx_is_an_error() {
        let Ok(g) = Gpu::open() else { return };
        let e = load_module(&g.drv, "this is not ptx\0").err().unwrap();
        assert!(e.contains("cuModuleLoadDataEx"), "{e}");
    }

    #[test]
    fn ptx_by_compute_capability() {
        let src = |cc| select_ptx(cc).map(|p| p.source);
        assert!(src((8, 6)).unwrap().contains("\n.target sm_70\n"));
        assert!(src((7, 0)).unwrap().contains("\n.target sm_70\n"));
        assert!(src((6, 1)).unwrap().contains("\n.target sm_61\n"));
        assert!(src((6, 2)).unwrap().contains("\n.target sm_61\n"));
        assert_eq!(select_ptx((6, 1)).unwrap().name, "sm_61");
        assert_eq!(select_ptx((8, 6)).unwrap().name, "sm_70");
        assert!(src((6, 0)).is_err());
        assert!(src((5, 2)).is_err());
        assert!(
            PTX_SM61.is_ascii() && PTX_SM70.is_ascii(),
            "ptxas 只收 ASCII"
        );
        assert!(PTX_SM61.ends_with('\0') && PTX_SM70.ends_with('\0'));
    }
}
