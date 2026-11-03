//! pagesift 的 GPU 内核，编成 PTX 后由 `pagesift-gpu` 在运行时经驱动 JIT 载入。
//!
//! 约定（与宿主 `crates/pagesift-gpu` 一致，改一边必须改另一边）：
//! - 所有张量是行优先连续存放的 f32，指针是设备地址；
//! - 一维内核每块 256 线程，下标 = 块号 × 块大小 + 线程号，越界线程直接返回；
//! - SGEMM 每块固定 16×16 线程（见 `gemm.rs`）。
//!
//! 只用 sm_61（Pascal）也有的指令：不写依赖 warp 同步、`shfl.sync` 之类 Volta 才有的代码，
//! 因为 Pascal 用的那份 PTX 是把 sm_70 的文件头改成 sm_61 得到的（见 `scripts/build_kernels.sh`）。
#![no_std]
#![feature(abi_ptx, stdarch_nvptx, asm_experimental_arch, core_float_math)]
#![allow(
    unsafe_op_in_unsafe_fn,
    clippy::missing_safety_doc,
    clippy::too_many_arguments
)]

mod conv;
mod elementwise;
mod gemm;
mod math;
mod seq;
mod tensor;

use core::arch::nvptx::{_block_dim_x, _block_idx_x, _thread_idx_x, trap};

#[panic_handler]
fn panic(_: &core::panic::PanicInfo) -> ! {
    unsafe { trap() }
}

/// 一维网格里本线程的全局下标。
#[inline(always)]
pub(crate) unsafe fn global_index() -> u64 {
    _block_idx_x() as u64 * _block_dim_x() as u64 + _thread_idx_x() as u64
}
