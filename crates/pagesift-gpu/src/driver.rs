//! CUDA 驱动 API 的最小绑定：运行时 `dlopen("libcuda.so.1")`，用 `dlsym` 取函数指针。
//! 不链接 CUDA 工具链，也不需要它；机器上只要有 NVIDIA 驱动。函数签名照 `cuda.h`（驱动 API）。

use std::ffi::{CStr, CString, c_char, c_int, c_uint, c_void};

/// `CUresult`：0 是成功，其余是错误码。
pub(crate) type CuResult = c_int;
/// `CUdevice`、`CUcontext`、`CUmodule`、`CUfunction`、`CUevent`、`CUstream`（后几个是不透明指针）。
pub(crate) type CuDevice = c_int;
pub(crate) type Handle = *mut c_void;
/// `CUdeviceptr`：64 位设备地址。
pub(crate) type DevPtr = u64;

// [协议] CUDA Driver API `CUresult` 枚举：CUDA_SUCCESS
pub(crate) const CUDA_SUCCESS: CuResult = 0;
// [协议] CUDA Driver API `CUdevice_attribute` 枚举：CU_DEVICE_ATTRIBUTE_COMPUTE_CAPABILITY_MAJOR
pub(crate) const ATTR_CC_MAJOR: c_int = 75;
// [协议] CUDA Driver API `CUdevice_attribute` 枚举：CU_DEVICE_ATTRIBUTE_COMPUTE_CAPABILITY_MINOR
pub(crate) const ATTR_CC_MINOR: c_int = 76;
// [协议] CUDA Driver API `CUjit_option` 枚举：CU_JIT_ERROR_LOG_BUFFER
pub(crate) const JIT_ERROR_LOG_BUFFER: c_int = 5;
// [协议] CUDA Driver API `CUjit_option` 枚举：CU_JIT_ERROR_LOG_BUFFER_SIZE_BYTES
pub(crate) const JIT_ERROR_LOG_BUFFER_SIZE_BYTES: c_int = 6;
// [协议] CUDA Driver API `CUevent_flags` 枚举：CU_EVENT_DEFAULT
pub(crate) const EVENT_DEFAULT: c_uint = 0;

#[cfg(unix)]
#[link(name = "dl")]
unsafe extern "C" {
    fn dlopen(file: *const c_char, flag: c_int) -> *mut c_void;
    fn dlsym(handle: *mut c_void, sym: *const c_char) -> *mut c_void;
}

// [协议] <dlfcn.h>：RTLD_NOW（glibc 与 macOS 都是 2），载入时就解析全部符号
#[cfg(unix)]
const RTLD_NOW: c_int = 2;

/// 驱动函数表。字段名对应驱动导出的符号（见 `load` 里的字符串，带 `_v2` 的是现行 ABI 版本）。
pub(crate) struct Driver {
    pub init: unsafe extern "C" fn(c_uint) -> CuResult,
    pub device_get_count: unsafe extern "C" fn(*mut c_int) -> CuResult,
    pub device_get: unsafe extern "C" fn(*mut CuDevice, c_int) -> CuResult,
    pub device_get_attribute: unsafe extern "C" fn(*mut c_int, c_int, CuDevice) -> CuResult,
    pub device_get_name: unsafe extern "C" fn(*mut c_char, c_int, CuDevice) -> CuResult,
    pub primary_ctx_retain: unsafe extern "C" fn(*mut Handle, CuDevice) -> CuResult,
    pub primary_ctx_release: unsafe extern "C" fn(CuDevice) -> CuResult,
    pub ctx_set_current: unsafe extern "C" fn(Handle) -> CuResult,
    pub ctx_synchronize: unsafe extern "C" fn() -> CuResult,
    pub module_load_data_ex: unsafe extern "C" fn(
        *mut Handle,
        *const c_void,
        c_uint,
        *mut c_int,
        *mut *mut c_void,
    ) -> CuResult,
    pub module_unload: unsafe extern "C" fn(Handle) -> CuResult,
    pub module_get_function: unsafe extern "C" fn(*mut Handle, Handle, *const c_char) -> CuResult,
    pub mem_alloc: unsafe extern "C" fn(*mut DevPtr, usize) -> CuResult,
    pub mem_free: unsafe extern "C" fn(DevPtr) -> CuResult,
    pub memcpy_htod: unsafe extern "C" fn(DevPtr, *const c_void, usize) -> CuResult,
    pub memcpy_dtoh: unsafe extern "C" fn(*mut c_void, DevPtr, usize) -> CuResult,
    pub memcpy_dtod: unsafe extern "C" fn(DevPtr, DevPtr, usize) -> CuResult,
    #[allow(clippy::type_complexity)]
    pub launch_kernel: unsafe extern "C" fn(
        Handle,
        c_uint,
        c_uint,
        c_uint,
        c_uint,
        c_uint,
        c_uint,
        c_uint,
        Handle,
        *mut *mut c_void,
        *mut *mut c_void,
    ) -> CuResult,
    pub event_create: unsafe extern "C" fn(*mut Handle, c_uint) -> CuResult,
    pub event_destroy: unsafe extern "C" fn(Handle) -> CuResult,
    pub event_record: unsafe extern "C" fn(Handle, Handle) -> CuResult,
    pub event_synchronize: unsafe extern "C" fn(Handle) -> CuResult,
    pub event_elapsed_time: unsafe extern "C" fn(*mut f32, Handle, Handle) -> CuResult,
    pub get_error_string: unsafe extern "C" fn(CuResult, *mut *const c_char) -> CuResult,
    pub mem_get_info: unsafe extern "C" fn(*mut usize, *mut usize) -> CuResult,
    pub driver_get_version: unsafe extern "C" fn(*mut c_int) -> CuResult,
}

/// 驱动库的名字；`PAGESIFT_GPU_LIB` 可以换成别的路径（驱动装在非标准位置时用，测试也靠它模拟载入失败）。
#[cfg(unix)]
fn library_name() -> CString {
    let custom = std::env::var("PAGESIFT_GPU_LIB")
        .ok()
        .filter(|v| !v.is_empty())
        .and_then(|v| CString::new(v).ok());
    custom.unwrap_or_else(|| c"libcuda.so.1".to_owned())
}

/// 用 NVML 读真实的驱动版本（如 “550.54.14”）。NVML 只是报告用途，缺了就返回 None。
#[cfg(unix)]
pub(crate) fn nvml_driver_version() -> Option<String> {
    let h = unsafe { dlopen(c"libnvidia-ml.so.1".as_ptr(), RTLD_NOW) };
    if h.is_null() {
        return None;
    }
    let sym = |name: &CStr| {
        let p = unsafe { dlsym(h, name.as_ptr()) };
        (!p.is_null()).then_some(p)
    };
    type Init = unsafe extern "C" fn() -> c_int;
    type Version = unsafe extern "C" fn(*mut c_char, c_uint) -> c_int;
    // SAFETY: 符号来自 NVML，签名照 nvml.h；函数指针与数据指针同宽
    let init: Init = unsafe { std::mem::transmute_copy(&sym(c"nvmlInit_v2")?) };
    let version: Version =
        unsafe { std::mem::transmute_copy(&sym(c"nvmlSystemGetDriverVersion")?) };
    let shutdown: Init = unsafe { std::mem::transmute_copy(&sym(c"nvmlShutdown")?) };
    if unsafe { init() } != 0 {
        return None;
    }
    let mut buf = [0 as c_char; 96]; // [协议] nvml.h：NVML_SYSTEM_DRIVER_VERSION_BUFFER_SIZE 是 80，留点余量
    let r = unsafe { version(buf.as_mut_ptr(), buf.len() as c_uint) };
    unsafe { shutdown() };
    (r == 0).then(|| {
        unsafe { CStr::from_ptr(buf.as_ptr()) }
            .to_string_lossy()
            .into_owned()
    })
}

#[cfg(not(unix))]
pub(crate) fn nvml_driver_version() -> Option<String> {
    None
}

impl Driver {
    /// 载入驱动库并取齐全部函数。没有驱动（或不是 Unix）时返回错误，不 panic。
    #[cfg(unix)]
    pub(crate) fn load() -> Result<Driver, String> {
        // 句柄不关：驱动库在进程里只载一次，函数指针要一直有效
        let lib = library_name();
        let h = unsafe { dlopen(lib.as_ptr(), RTLD_NOW) };
        if h.is_null() {
            return Err(format!(
                "failed to load {}: NVIDIA driver not installed",
                lib.to_string_lossy()
            ));
        }
        let sym = |name: &str| -> Result<*mut c_void, String> {
            let c = CString::new(name).map_err(|e| e.to_string())?;
            let p = unsafe { dlsym(h, c.as_ptr()) };
            if p.is_null() {
                Err(format!("driver lacks function {name} (driver too old?)"))
            } else {
                Ok(p)
            }
        };
        macro_rules! f {
            ($name:literal) => {
                // SAFETY: 符号来自驱动库，签名与字段类型一致（照 cuda.h 写）；函数指针与数据指针同宽
                unsafe { std::mem::transmute_copy::<*mut c_void, _>(&sym($name)?) }
            };
        }
        Ok(Driver {
            init: f!("cuInit"),
            device_get_count: f!("cuDeviceGetCount"),
            device_get: f!("cuDeviceGet"),
            device_get_attribute: f!("cuDeviceGetAttribute"),
            device_get_name: f!("cuDeviceGetName"),
            primary_ctx_retain: f!("cuDevicePrimaryCtxRetain"),
            primary_ctx_release: f!("cuDevicePrimaryCtxRelease_v2"),
            ctx_set_current: f!("cuCtxSetCurrent"),
            ctx_synchronize: f!("cuCtxSynchronize"),
            module_load_data_ex: f!("cuModuleLoadDataEx"),
            module_unload: f!("cuModuleUnload"),
            module_get_function: f!("cuModuleGetFunction"),
            mem_alloc: f!("cuMemAlloc_v2"),
            mem_free: f!("cuMemFree_v2"),
            memcpy_htod: f!("cuMemcpyHtoD_v2"),
            memcpy_dtoh: f!("cuMemcpyDtoH_v2"),
            memcpy_dtod: f!("cuMemcpyDtoD_v2"),
            launch_kernel: f!("cuLaunchKernel"),
            event_create: f!("cuEventCreate"),
            event_destroy: f!("cuEventDestroy_v2"),
            event_record: f!("cuEventRecord"),
            event_synchronize: f!("cuEventSynchronize"),
            event_elapsed_time: f!("cuEventElapsedTime"),
            get_error_string: f!("cuGetErrorString"),
            mem_get_info: f!("cuMemGetInfo_v2"),
            driver_get_version: f!("cuDriverGetVersion"),
        })
    }

    #[cfg(not(unix))]
    pub(crate) fn load() -> Result<Driver, String> {
        Err("the GPU backend only supports the NVIDIA driver on Linux".into())
    }

    /// 把驱动返回码转成 `Result`，错误信息带上调用名和驱动给的说明。
    pub(crate) fn check(&self, r: CuResult, what: &str) -> Result<(), String> {
        if r == CUDA_SUCCESS {
            return Ok(());
        }
        let mut s: *const c_char = std::ptr::null();
        unsafe { (self.get_error_string)(r, &mut s) };
        let msg = if s.is_null() {
            "unknown error".into()
        } else {
            unsafe { CStr::from_ptr(s) }.to_string_lossy().into_owned()
        };
        Err(format!("{what} failed: error {r} ({msg})"))
    }
}
