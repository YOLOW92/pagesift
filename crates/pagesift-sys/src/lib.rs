//! 进程级调优。整个 pagesift 只有这里有 unsafe：调用 glibc 的 `mallopt`。
//!
//! 只应该由 pagesift 自己的可执行程序调用；Python、Node 绑定跑在别人的进程里，
//! 不能替宿主改分配器参数。

#![deny(unsafe_code)]

/// 固定 glibc 的 mmap 阈值。
///
/// glibc 默认会在释放大块内存后把 mmap 阈值调高（最高 32 MB），之后页面大小的缓冲区
/// （几百 KB 到几 MB 的 HTML、DOM、Markdown）都改从堆里分配，用完不还给系统。
/// 并发爬取时 RSS 因此是实际使用量的两倍多。固定在 256 KB 后这些块用完立即归还：
/// 本地 20 站点爬取的峰值 RSS 从 188 MB 降到约 75 MB，CPU 多约 8%。
/// 非 glibc 平台什么也不做。
pub fn tune_allocator() {
    #[cfg(all(target_os = "linux", target_env = "gnu"))]
    {
        /// 显式设置后 glibc 不再动态调整阈值。
        const MMAP_THRESHOLD: libc::c_int = 256 * 1024;
        // SAFETY: mallopt 只修改分配器参数，任何时候调用都安全；返回 0 表示参数不被接受，忽略即可。
        #[allow(unsafe_code)]
        unsafe {
            libc::mallopt(libc::M_MMAP_THRESHOLD, MMAP_THRESHOLD);
        }
    }
}
