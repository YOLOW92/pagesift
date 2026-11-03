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

mod buffer;
mod driver;
mod ops;

use std::ffi::{CStr, c_char, c_int, c_void};
use std::ptr::null_mut;

pub use buffer::{DeviceBuffer, Timer};
pub use ops::ConvShape;

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
}

/// 一块 N 卡上的上下文、载好的内核模块和函数表。所有方法取 `&self`，可以跨线程共享。
pub struct Gpu {
    drv: Driver,
    dev: c_int,
    ctx: Handle,
    module: Handle,
    k: Kernels,
    name: String,
    cc: (i32, i32),
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

impl Gpu {
    /// 打开 0 号 N 卡：载入驱动、取主上下文、按算力挑 PTX 并 JIT。
    /// 没有驱动、没有设备、算力低于 6.1 或 JIT 失败时返回错误（不 panic）。
    pub fn open() -> Result<Gpu, String> {
        let drv = Driver::load()?;
        drv.check(unsafe { (drv.init)(0) }, "cuInit")?;
        let mut count = 0;
        drv.check(
            unsafe { (drv.device_get_count)(&mut count) },
            "cuDeviceGetCount",
        )?;
        if count == 0 {
            return Err("没有 NVIDIA GPU".into());
        }
        let mut dev = 0;
        drv.check(unsafe { (drv.device_get)(&mut dev, 0) }, "cuDeviceGet")?;
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
        let mut ctx = null_mut();
        drv.check(
            unsafe { (drv.primary_ctx_retain)(&mut ctx, dev) },
            "cuDevicePrimaryCtxRetain",
        )?;
        // 之后出错要释放主上下文：先拼出不带模块的半成品，交给 Drop 收尾
        let mut gpu = Gpu {
            drv,
            dev,
            ctx,
            module: null_mut(),
            k: Kernels::null(),
            name,
            cc,
        };
        gpu.bind()?;
        gpu.module = load_module(&gpu.drv, ptx)?;
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

/// 按算力挑 PTX：≥ 7.0 用 sm_70 原版；6.1、6.2 用改过文件头的 sm_61 版；更老的不支持
/// （.target sm_61 的 PTX 在 6.0 的卡上 JIT 会失败）。
fn select_ptx(cc: (i32, i32)) -> Result<&'static str, String> {
    match cc {
        // [协议] PTX ISA：sm_70 是 Volta 的算力 7.0；sm_61 覆盖算力 6.1 与 6.2
        (major, _) if major >= 7 => Ok(PTX_SM70),
        (6, minor) if minor >= 1 => Ok(PTX_SM61),
        (major, minor) => Err(format!(
            "GPU 算力 {major}.{minor} 太老，至少要 6.1（Pascal）"
        )),
    }
}

fn device_name(drv: &Driver, dev: c_int) -> String {
    let mut buf = [0 as c_char; 256]; // [策略] 设备名缓冲，驱动给的名字远短于此
    let r = unsafe { (drv.device_get_name)(buf.as_mut_ptr(), buf.len() as c_int, dev) };
    if r != 0 {
        return "未知 NVIDIA GPU".into();
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
        return Err(format!("{e}；JIT 日志：{}", text.trim()));
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
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn ptx_by_compute_capability() {
        assert!(select_ptx((8, 6)).unwrap().contains("\n.target sm_70\n"));
        assert!(select_ptx((7, 0)).unwrap().contains("\n.target sm_70\n"));
        assert!(select_ptx((6, 1)).unwrap().contains("\n.target sm_61\n"));
        assert!(select_ptx((6, 2)).unwrap().contains("\n.target sm_61\n"));
        assert!(select_ptx((6, 0)).is_err());
        assert!(select_ptx((5, 2)).is_err());
        assert!(
            PTX_SM61.is_ascii() && PTX_SM70.is_ascii(),
            "ptxas 只收 ASCII"
        );
        assert!(PTX_SM61.ends_with('\0') && PTX_SM70.ends_with('\0'));
    }
}
