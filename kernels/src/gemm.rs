//! 共享内存分块 SGEMM，带批次：对每个批次 z（网格的 z 维），
//! `C_z[M×N] = A_z[M×K] · B_z[K×N]`，行优先；`A_z = a + z·sa`，B、C 同理。
//! B 的行距是 `ldb`（可以大于 N，隐式卷积靠它在补过边的输入上开窗）；`accumulate` 非 0 时结果累加进 C。
//!
//! 每块 16×16 线程算 C 的 64×64 子块，每线程 4×4 个元素。线程 (tx, ty) 负责的行是 `ty + 16·i`、
//! 列是 `tx + 16·j`（i, j ∈ 0..4）：相邻线程读共享内存的相邻字（无 bank 冲突），写 C 时相邻线程写
//! 相邻地址（合并访问）。每轮把 A 的 64×16、B 的 16×64 搬进共享内存；边界外补 0，M、N、K 任意。

use core::arch::nvptx::{_block_idx_x, _block_idx_y, _syncthreads, _thread_idx_x, _thread_idx_y};

const BM: usize = 64; // [策略] 每块算的 C 行数；OCR 小模型的输出通道多在 16–256，64 兼顾利用率与复用
const BN: usize = 64; // [策略] 每块算的 C 列数
const BK: usize = 16; // [策略] 每轮沿 K 搬的长度；A、B 子块各 1024 个元素，256 线程每人搬 4 个
const THREADS: usize = 256; // [推导] 16 × 16 线程
const SIDE: usize = 16; // [推导] 线程块边长，BM / 4 = BN / 4
const SA_LD: usize = BM + 1; // [策略] A 子块转置存放，行距补 1 个字，错开转置写入时的 bank
const SMEM_FLOATS: usize = BK * SA_LD + BK * BN; // [推导] 16·65 + 16·64 = 2064 个 f32 = 8256 字节

/// 共享内存的通用地址。`core::arch::nvptx` 没有共享内存接口，用内联 PTX 声明数组再转成通用地址。
/// 数组大小必须等于 `SMEM_FLOATS · 4`。
#[inline(always)]
unsafe fn smem_base() -> *mut f32 {
    let p: u64;
    core::arch::asm!(
        "{{ .shared .align 16 .b8 sgemm_tile[8256]; .reg .u64 t; mov.u64 t, sgemm_tile; cvta.shared.u64 {0}, t; }}",
        out(reg64) p,
    );
    p as *mut f32
}

const _: () = assert!(
    SMEM_FLOATS * 4 == 8256,
    "改了分块大小要同步改 smem_base 里的数组大小"
);

#[unsafe(no_mangle)]
pub unsafe extern "ptx-kernel" fn sgemm(
    a: *const f32,
    b: *const f32,
    c: *mut f32,
    m: u32,
    n: u32,
    k: u32,
    ldb: u32,
    accumulate: u32,
    sa: u64,
    sb: u64,
    sc: u64,
) {
    let z = core::arch::nvptx::_block_idx_z() as usize;
    let a = a.add(z * sa as usize);
    let b = b.add(z * sb as usize);
    let c = c.add(z * sc as usize);
    let (m, n, k, ldb) = (m as usize, n as usize, k as usize, ldb as usize);
    let tx = _thread_idx_x() as usize;
    let ty = _thread_idx_y() as usize;
    let tid = ty * SIDE + tx;
    let row0 = _block_idx_y() as usize * BM;
    let col0 = _block_idx_x() as usize * BN;
    let sm = smem_base();
    let s_a = sm; // 转置：s_a[p·SA_LD + i] = A[row0 + i, kk + p]
    let s_b = sm.add(BK * SA_LD); // s_b[p·BN + j] = B[kk + p, col0 + j]
    let mut acc = [[0.0f32; 4]; 4];
    let mut kk = 0;
    while kk < k {
        for r in 0..BM * BK / THREADS {
            let idx = tid + r * THREADS;
            // A：相邻线程读同一行相邻的 p
            let (i, p) = (idx / BK, idx % BK);
            let (gi, gp) = (row0 + i, kk + p);
            *s_a.add(p * SA_LD + i) = if gi < m && gp < k {
                *a.add(gi * k + gp)
            } else {
                0.0
            };
            // B：相邻线程读同一行相邻的 j
            let (p, j) = (idx / BN, idx % BN);
            let (gp, gj) = (kk + p, col0 + j);
            *s_b.add(p * BN + j) = if gp < k && gj < n {
                *b.add(gp * ldb + gj)
            } else {
                0.0
            };
        }
        _syncthreads();
        for p in 0..BK {
            let mut av = [0.0f32; 4];
            let mut bv = [0.0f32; 4];
            for x in 0..4 {
                av[x] = *s_a.add(p * SA_LD + ty + SIDE * x);
                bv[x] = *s_b.add(p * BN + tx + SIDE * x);
            }
            for i in 0..4 {
                for j in 0..4 {
                    acc[i][j] = core::f32::math::mul_add(av[i], bv[j], acc[i][j]);
                }
            }
        }
        _syncthreads();
        kk += BK;
    }
    for i in 0..4 {
        let row = row0 + ty + SIDE * i;
        if row < m {
            for j in 0..4 {
                let col = col0 + tx + SIDE * j;
                if col < n {
                    let p = c.add(row * n + col);
                    *p = if accumulate != 0 {
                        *p + acc[i][j]
                    } else {
                        acc[i][j]
                    };
                }
            }
        }
    }
}
