//! 显存与内核启动的安全封装：`DeviceBuffer`（f32 数组，离开作用域时释放）、内核参数打包、
//! 用 CUDA 事件计时的 `Timer`。

use std::ffi::c_void;
use std::ptr::null_mut;

use crate::Gpu;
use crate::driver::{DevPtr, EVENT_DEFAULT, Handle};

/// 显存里的一段 f32。借用创建它的 [`Gpu`]，保证在上下文释放前先释放。
pub struct DeviceBuffer<'g> {
    gpu: &'g Gpu,
    ptr: DevPtr,
    len: usize,
}

impl Gpu {
    /// 分配 `len` 个 f32 的显存（内容未初始化）。`len` 为 0 时不调驱动。
    pub fn alloc(&self, len: usize) -> Result<DeviceBuffer<'_>, String> {
        self.bind()?;
        let mut ptr = 0;
        if len > 0 {
            let bytes = len
                .checked_mul(size_of::<f32>())
                .ok_or("GPU buffer size overflow")?;
            self.drv.check(
                unsafe { (self.drv.mem_alloc)(&mut ptr, bytes) },
                "cuMemAlloc",
            )?;
        }
        Ok(DeviceBuffer {
            gpu: self,
            ptr,
            len,
        })
    }

    /// 分配显存并拷入 `data`。
    pub fn upload(&self, data: &[f32]) -> Result<DeviceBuffer<'_>, String> {
        let mut buf = self.alloc(data.len())?;
        buf.copy_from_host(data)?;
        Ok(buf)
    }
}

impl<'g> DeviceBuffer<'g> {
    /// 元素个数。
    pub fn len(&self) -> usize {
        self.len
    }

    pub fn is_empty(&self) -> bool {
        self.len == 0
    }

    /// 设备地址（传给内核用）。
    pub(crate) fn ptr(&self) -> DevPtr {
        self.ptr
    }

    /// 从内存拷入，长度必须相等。
    pub fn copy_from_host(&mut self, data: &[f32]) -> Result<(), String> {
        assert_eq!(data.len(), self.len, "拷入长度与显存长度不同");
        if self.len == 0 {
            return Ok(());
        }
        let g = self.gpu;
        g.bind()?;
        let r = unsafe {
            (g.drv.memcpy_htod)(
                self.ptr,
                data.as_ptr() as *const c_void,
                self.len * size_of::<f32>(),
            )
        };
        g.drv.check(r, "cuMemcpyHtoD")
    }

    /// 拷回内存，长度必须相等。默认流上同步执行：之前提交的内核先跑完。
    pub fn copy_to_host(&self, out: &mut [f32]) -> Result<(), String> {
        assert_eq!(out.len(), self.len, "拷回长度与显存长度不同");
        if self.len == 0 {
            return Ok(());
        }
        let g = self.gpu;
        g.bind()?;
        let r = unsafe {
            (g.drv.memcpy_dtoh)(
                out.as_mut_ptr() as *mut c_void,
                self.ptr,
                self.len * size_of::<f32>(),
            )
        };
        g.drv.check(r, "cuMemcpyDtoH")
    }

    /// 把 `data` 拷到开头（`data` 可以比缓冲短，缓冲池给的容量常常多一点）。
    pub(crate) fn write_prefix(&self, data: &[f32]) -> Result<(), String> {
        assert!(data.len() <= self.len, "拷入长度超过显存长度");
        if data.is_empty() {
            return Ok(());
        }
        let g = self.gpu;
        g.bind()?;
        let r = unsafe {
            (g.drv.memcpy_htod)(
                self.ptr,
                data.as_ptr() as *const c_void,
                std::mem::size_of_val(data),
            )
        };
        g.drv.check(r, "cuMemcpyHtoD")
    }

    /// 从开头拷回 `out.len()` 个元素。默认流上同步执行：之前提交的内核先跑完。
    pub(crate) fn read_prefix(&self, out: &mut [f32]) -> Result<(), String> {
        assert!(out.len() <= self.len, "拷回长度超过显存长度");
        if out.is_empty() {
            return Ok(());
        }
        let g = self.gpu;
        g.bind()?;
        let r = unsafe {
            (g.drv.memcpy_dtoh)(
                out.as_mut_ptr() as *mut c_void,
                self.ptr,
                std::mem::size_of_val(out),
            )
        };
        g.drv.check(r, "cuMemcpyDtoH")
    }

    /// 拷回成新的 `Vec`。
    pub fn to_vec(&self) -> Result<Vec<f32>, String> {
        let mut v = vec![0f32; self.len];
        self.copy_to_host(&mut v)?;
        Ok(v)
    }

    /// 从另一段显存拷入，长度必须相等。
    pub fn copy_from_device(&mut self, src: &DeviceBuffer<'_>) -> Result<(), String> {
        assert_eq!(src.len, self.len, "显存拷贝长度不同");
        if self.len == 0 {
            return Ok(());
        }
        let g = self.gpu;
        g.bind()?;
        g.drv.check(
            unsafe { (g.drv.memcpy_dtod)(self.ptr, src.ptr, self.len * size_of::<f32>()) },
            "cuMemcpyDtoD",
        )
    }
}

impl Drop for DeviceBuffer<'_> {
    fn drop(&mut self) {
        if self.len > 0 && self.gpu.bind().is_ok() {
            unsafe { (self.gpu.drv.mem_free)(self.ptr) };
        }
    }
}

/// 内核参数：每个参数放在自己的 8 字节槽里，`cuLaunchKernel` 按内核签名的宽度从槽首读取
/// （小端机器上 u32 就在槽的低 4 字节）。
#[derive(Default)]
pub(crate) struct Args {
    slots: Vec<u64>,
}

impl Args {
    pub fn ptr(mut self, b: &DeviceBuffer<'_>) -> Self {
        self.slots.push(b.ptr());
        self
    }

    pub fn raw_ptr(mut self, p: DevPtr) -> Self {
        self.slots.push(p);
        self
    }

    /// u32 参数；超出 u32 的尺寸是调用方的错（显存放不下那么大的张量），这里直接断言。
    pub fn u32(mut self, v: usize) -> Self {
        self.slots
            .push(u32::try_from(v).expect("内核参数超出 u32") as u64);
        self
    }

    pub fn u64(mut self, v: usize) -> Self {
        self.slots.push(v as u64);
        self
    }

    pub fn f32(mut self, v: f32) -> Self {
        self.slots.push(v.to_bits() as u64);
        self
    }
}

const _: () = assert!(cfg!(target_endian = "little"), "Args 的槽布局假定小端");

impl Gpu {
    /// 在默认流上启动内核（异步）。`grid`、`block` 是 (x, y, z)。
    pub(crate) fn launch(
        &self,
        f: Handle,
        grid: (u32, u32, u32),
        block: (u32, u32, u32),
        args: Args,
    ) -> Result<(), String> {
        if grid.0 == 0 || grid.1 == 0 || grid.2 == 0 {
            return Ok(()); // 空张量：没有工作
        }
        self.bind()?;
        let mut slots = args.slots;
        let mut ptrs: Vec<*mut c_void> = slots
            .iter_mut()
            .map(|s| s as *mut u64 as *mut c_void)
            .collect();
        let r = unsafe {
            (self.drv.launch_kernel)(
                f,
                grid.0,
                grid.1,
                grid.2,
                block.0,
                block.1,
                block.2,
                0,
                null_mut(),
                ptrs.as_mut_ptr(),
                null_mut(),
            )
        };
        self.drv.check(r, "cuLaunchKernel")
    }
}

/// 用一对 CUDA 事件量设备上一段工作的耗时（只算 GPU 执行，不含主机侧开销）。
pub struct Timer<'g> {
    gpu: &'g Gpu,
    start: Handle,
    end: Handle,
}

impl Gpu {
    /// 建计时器并记下起点。
    pub fn timer(&self) -> Result<Timer<'_>, String> {
        self.bind()?;
        let mut t = Timer {
            gpu: self,
            start: null_mut(),
            end: null_mut(),
        };
        let d = &self.drv;
        d.check(
            unsafe { (d.event_create)(&mut t.start, EVENT_DEFAULT) },
            "cuEventCreate",
        )?;
        d.check(
            unsafe { (d.event_create)(&mut t.end, EVENT_DEFAULT) },
            "cuEventCreate",
        )?;
        d.check(
            unsafe { (d.event_record)(t.start, null_mut()) },
            "cuEventRecord",
        )?;
        Ok(t)
    }
}

impl Timer<'_> {
    /// 记下终点，等它完成，返回起点到终点的毫秒数。
    pub fn stop_ms(&self) -> Result<f32, String> {
        let d = &self.gpu.drv;
        self.gpu.bind()?;
        d.check(
            unsafe { (d.event_record)(self.end, null_mut()) },
            "cuEventRecord",
        )?;
        d.check(
            unsafe { (d.event_synchronize)(self.end) },
            "cuEventSynchronize",
        )?;
        let mut ms = 0f32;
        d.check(
            unsafe { (d.event_elapsed_time)(&mut ms, self.start, self.end) },
            "cuEventElapsedTime",
        )?;
        Ok(ms)
    }
}

impl Drop for Timer<'_> {
    fn drop(&mut self) {
        if self.gpu.bind().is_ok() {
            for e in [self.start, self.end] {
                if !e.is_null() {
                    unsafe { (self.gpu.drv.event_destroy)(e) };
                }
            }
        }
    }
}
