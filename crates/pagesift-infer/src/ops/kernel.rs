//! 矩阵乘的微内核：把打包好的 A 条（kc×MR）与 B 条（kc×NR）相乘，加到 C 的 MR×NR 块上。
//! x86_64 上运行时检测到 AVX2 + FMA 就用手写内核，否则走可自动向量化的通用内核。

use std::sync::OnceLock;

/// [推导] 微内核的行数、列数：AVX2 有 16 个 ymm 寄存器，6×16 的 C 块用 12 个做累加器
/// （每行 2 个 8 宽向量），再加 2 个装 B 的一行、1 个广播 A，共 15 个，不溢出到栈。
pub(crate) const MR: usize = 6;
pub(crate) const NR: usize = 16;

/// 微内核：`(kc, a, b, c, ldc, mr, nr)`，`a` 是打包好的 kc×MR、`b` 是打包好的 kc×NR，
/// 把乘积加到 C 左上 mr×nr 的位置。
///
/// 调用约定（各实现的 Safety）：`a`、`b` 各有 kc×MR、kc×NR 个可读元素；`c` 起的 mr 行
/// （行距 ldc）、每行 nr 个元素已初始化、可写，且无人并发访问；mr ≤ MR、nr ≤ NR。
pub(crate) type Kernel = unsafe fn(usize, *const f32, *const f32, *mut f32, usize, usize, usize);

/// 本机可用的最快内核（只检测一次）。
pub(crate) fn best() -> Kernel {
    static K: OnceLock<Kernel> = OnceLock::new();
    *K.get_or_init(|| {
        #[cfg(target_arch = "x86_64")]
        if is_x86_feature_detected!("avx2") && is_x86_feature_detected!("fma") {
            return avx2::kernel as Kernel;
        }
        generic as Kernel
    })
}

/// 本机能跑的全部内核（测试逐一对照用）。
#[cfg(test)]
pub(crate) fn all() -> Vec<Kernel> {
    // 只有 x86_64 才往里加
    #[allow(unused_mut)]
    let mut v: Vec<Kernel> = vec![generic];
    #[cfg(target_arch = "x86_64")]
    if is_x86_feature_detected!("avx2") && is_x86_feature_detected!("fma") {
        v.push(avx2::kernel);
    }
    v
}

/// 通用内核：固定大小的累加数组，内层是对连续 NR 个数的乘加，编译器能自动向量化。
///
/// # Safety
/// 见 [`Kernel`] 的调用约定。
unsafe fn generic(
    kc: usize,
    a: *const f32,
    b: *const f32,
    c: *mut f32,
    ldc: usize,
    mr: usize,
    nr: usize,
) {
    // SAFETY：长度由调用约定保证
    let (a, b) = unsafe {
        (
            std::slice::from_raw_parts(a, kc * MR),
            std::slice::from_raw_parts(b, kc * NR),
        )
    };
    let mut acc = [[0f32; NR]; MR];
    for (ap, bp) in a.as_chunks::<MR>().0.iter().zip(b.as_chunks::<NR>().0) {
        for (row, &av) in acc.iter_mut().zip(ap) {
            for (s, &bv) in row.iter_mut().zip(bp) {
                *s += av * bv;
            }
        }
    }
    for (i, row) in acc.iter().enumerate().take(mr) {
        for (j, &v) in row.iter().enumerate().take(nr) {
            // SAFETY：i < mr、j < nr，在调用约定保证的范围里
            unsafe { *c.add(i * ldc + j) += v };
        }
    }
}

#[cfg(target_arch = "x86_64")]
mod avx2 {
    use super::{MR, NR};
    use std::arch::x86_64::*;

    /// AVX2 + FMA 的 6×16 内核。
    ///
    /// # Safety
    /// 见 [`super::Kernel`] 的调用约定；另需 CPU 支持 avx2、fma（由 [`super::best`] 检测）。
    #[target_feature(enable = "avx2,fma")]
    pub(super) unsafe fn kernel(
        kc: usize,
        a: *const f32,
        b: *const f32,
        c: *mut f32,
        ldc: usize,
        mr: usize,
        nr: usize,
    ) {
        // [协议] 一个 ymm 装 8 个 f32；每行 NR = 16 个数用两个
        const V: usize = 8;
        let mut acc = [_mm256_setzero_ps(); 2 * MR];
        let (mut a, mut b) = (a, b);
        // SAFETY：读写范围见调用约定；指针每轮前移一行打包数据，共 kc 轮
        unsafe {
            for _ in 0..kc {
                let b0 = _mm256_loadu_ps(b);
                let b1 = _mm256_loadu_ps(b.add(V));
                for i in 0..MR {
                    let ai = _mm256_set1_ps(*a.add(i));
                    acc[2 * i] = _mm256_fmadd_ps(ai, b0, acc[2 * i]);
                    acc[2 * i + 1] = _mm256_fmadd_ps(ai, b1, acc[2 * i + 1]);
                }
                a = a.add(MR);
                b = b.add(NR);
            }
            if mr == MR && nr == NR {
                for i in 0..MR {
                    let r = c.add(i * ldc);
                    _mm256_storeu_ps(r, _mm256_add_ps(_mm256_loadu_ps(r), acc[2 * i]));
                    let r = r.add(V);
                    _mm256_storeu_ps(r, _mm256_add_ps(_mm256_loadu_ps(r), acc[2 * i + 1]));
                }
            } else {
                let mut t = [0f32; MR * NR];
                for i in 0..MR {
                    _mm256_storeu_ps(t.as_mut_ptr().add(i * NR), acc[2 * i]);
                    _mm256_storeu_ps(t.as_mut_ptr().add(i * NR + V), acc[2 * i + 1]);
                }
                for i in 0..mr {
                    for j in 0..nr {
                        *c.add(i * ldc + j) += t[i * NR + j];
                    }
                }
            }
        }
    }
}
