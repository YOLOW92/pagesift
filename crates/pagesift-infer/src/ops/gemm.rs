//! 单精度矩阵乘 C = 初值 + A·B（三者都行优先，各带行距）。做法照 BLIS：K、M、N 三向分块，
//! A 打包成 MR 行一条、B 打包成 NR 列一条，微内核（见 [`super::kernel`]）在寄存器里累加。
//! 大矩阵把 C 切成二维的块，多线程动态领取。
//!
//! 新分配的输出不在调用方先清零：每个块由算它的线程先填初值（偏置或 0）再累加，
//! 首次写入的缺页分散到各线程，块也正好热在缓存里。

use std::sync::Mutex;

use super::kernel::{self, Kernel, MR, NR};
use crate::par::{self, SyncPtr};

/// [推导] K 方向块长：一条 B 微面板 KC×NR×4 B = 16 KB，占 L1d（32 KB）的一半，
/// 余下放一条 A 微面板 KC×MR×4 B = 6 KB 和 C 块。
const KC: usize = 256;
/// [推导] M 方向块高（MR 的倍数）：A 块 MC×KC×4 B = 120 KB，放在 L2（i7-10700 每核 256 KB）里
/// 对每条 B 微面板反复用，另一半留给流过的 B。
const MC: usize = 120;
/// [策略] N 方向块宽（NR 的倍数）：B 块 KC×NC×4 B = 256 KB，从 L2/L3 流过；块越宽，A 重复打包越少。
const NC: usize = 256;
/// [策略] 多线程时每个线程平均分到的块数：动态领取，块多一些尾部更均衡，太多则重复打包变多。
const TILES_PER_THREAD: usize = 2;
/// [策略] 为凑够块数缩小块时的下限：再小打包和调用开销就压过计算了。
const MIN_NT: usize = 4 * NR;
const MIN_MT: usize = 4 * MR;

/// C 的初值。
#[derive(Clone, Copy)]
pub(crate) enum Init<'a> {
    /// C 已经有值，在上面累加（目前只有测试用）。
    #[cfg_attr(not(test), allow(dead_code))]
    Accumulate,
    /// C 尚未初始化：先把第 i 行填成 `bias[i]`（`None` 填 0），再累加。
    Fill(Option<&'a [f32]>),
}

impl<'a> Init<'a> {
    /// 从第 `i0` 行开始的子块对应的初值。
    fn rows_from(self, i0: usize) -> Init<'a> {
        match self {
            Init::Fill(Some(b)) => Init::Fill(Some(&b[i0..])),
            other => other,
        }
    }
}

/// 打包 A 的 m×kc 块（`a[i·lda + p]`）：每 MR 行一条，条内按 p 优先、每个 p 连着 MR 个数，不足补 0。
fn pack_a(m: usize, kc: usize, a: &[f32], lda: usize, dst: &mut Vec<f32>) {
    dst.clear();
    dst.resize(m.div_ceil(MR) * kc * MR, 0.0);
    for (ip, out) in dst.chunks_exact_mut(kc * MR).enumerate() {
        let i0 = ip * MR;
        for i in 0..MR.min(m - i0) {
            let row = &a[(i0 + i) * lda..(i0 + i) * lda + kc];
            for (p, &v) in row.iter().enumerate() {
                out[p * MR + i] = v;
            }
        }
    }
}

/// 打包 B 的 kc×n 块（`b[p·ldb + j]`）：每 NR 列一条，条内每个 p 连着 NR 个数，不足补 0。
fn pack_b(kc: usize, n: usize, b: &[f32], ldb: usize, dst: &mut Vec<f32>) {
    dst.clear();
    dst.resize(n.div_ceil(NR) * kc * NR, 0.0);
    for (jp, out) in dst.chunks_exact_mut(kc * NR).enumerate() {
        let j0 = jp * NR;
        let w = NR.min(n - j0);
        for p in 0..kc {
            out[p * NR..p * NR + w].copy_from_slice(&b[p * ldb + j0..p * ldb + j0 + w]);
        }
    }
}

/// 打包缓冲。
#[derive(Default)]
pub(crate) struct Packs {
    pa: Vec<f32>,
    pb: Vec<f32>,
}

/// 一个线程的临时缓冲：打包用的，加上 im2col 的列矩阵。
#[derive(Default)]
pub(crate) struct Scratch {
    pub(crate) packs: Packs,
    pub(crate) cols: Vec<f32>,
}

/// 用过的缓冲放回这里，下次调用接着用。线程是每次现开的，放 thread_local 会随线程销毁，
/// 每次重新分配几百 KB 又要重新缺页。
static POOL: Mutex<Vec<Scratch>> = Mutex::new(Vec::new());

impl Scratch {
    /// 从池里取一份（没有就新建），守卫析构时放回。
    pub(crate) fn take() -> ScratchGuard {
        let s = POOL.lock().unwrap_or_else(|e| e.into_inner()).pop();
        ScratchGuard(s.unwrap_or_default())
    }
}

/// 借出的缓冲，析构时放回池里。
pub(crate) struct ScratchGuard(Scratch);

impl std::ops::Deref for ScratchGuard {
    type Target = Scratch;
    fn deref(&self) -> &Scratch {
        &self.0
    }
}

impl std::ops::DerefMut for ScratchGuard {
    fn deref_mut(&mut self) -> &mut Scratch {
        &mut self.0
    }
}

impl Drop for ScratchGuard {
    fn drop(&mut self) {
        let mut pool = POOL.lock().unwrap_or_else(|e| e.into_inner());
        // [策略] 池里最多留两倍线程数份，同时在用的不会超过线程数，多的直接释放
        if pool.len() < 2 * par::max_threads() {
            pool.push(std::mem::take(&mut self.0));
        }
    }
}

/// 单线程算一块：C[m×n] = 初值 + A[m×k]·B[k×n]（`a`、`b` 从块的左上角开始，`c` 指向 C 块左上角）。
/// 调用方让 m ≤ MC、n ≤ NC 时 A 块能留在 L2。
///
/// # Safety
/// `c` 起的 m 行（行距 ldc）、每行 n 个元素可写（`Init::Accumulate` 时还须已初始化），
/// 且这期间没有别的线程访问这些位置。
#[allow(clippy::too_many_arguments)]
unsafe fn gemm_tile(
    kern: Kernel,
    (m, n, k): (usize, usize, usize),
    a: &[f32],
    lda: usize,
    b: &[f32],
    ldb: usize,
    c: *mut f32,
    ldc: usize,
    init: Init,
    s: &mut Packs,
) {
    if let Init::Fill(bias) = init {
        for i in 0..m {
            let v = bias.map_or(0.0, |b| b[i]);
            for j in 0..n {
                // SAFETY：在调用方保证的可写范围里；用 write 不读未初始化的旧值
                unsafe { c.add(i * ldc + j).write(v) };
            }
        }
    }
    for p0 in (0..k).step_by(KC) {
        let kc = KC.min(k - p0);
        pack_b(kc, n, &b[p0 * ldb..], ldb, &mut s.pb);
        pack_a(m, kc, &a[p0..], lda, &mut s.pa);
        for (jp, bp) in s.pb.chunks_exact(kc * NR).enumerate() {
            let j0 = jp * NR;
            for (ip, ap) in s.pa.chunks_exact(kc * MR).enumerate() {
                let i0 = ip * MR;
                // SAFETY：i0 + mr ≤ m、j0 + nr ≤ n，落在调用方保证的范围里，且已初始化；
                // 打包缓冲长度足够
                unsafe {
                    kern(
                        kc,
                        ap.as_ptr(),
                        bp.as_ptr(),
                        c.add(i0 * ldc + j0),
                        ldc,
                        MR.min(m - i0),
                        NR.min(n - j0),
                    )
                };
            }
        }
    }
}

/// 检查切片够长：A 是 m×k（行距 lda）、B 是 k×n（行距 ldb）。
fn check_ab(
    (m, n, k): (usize, usize, usize),
    (a, lda): (&[f32], usize),
    (b, ldb): (&[f32], usize),
) {
    assert!(lda >= k && ldb >= n, "矩阵乘的行距小于列数");
    assert!(m == 0 || a.len() >= (m - 1) * lda + k, "矩阵乘的 A 太短");
    assert!(k == 0 || b.len() >= (k - 1) * ldb + n, "矩阵乘的 B 太短");
}

/// 单线程的 C = 初值 + A·B，给已经在外层并行的调用方（如按像素分段的 im2col）用。
///
/// # Safety
/// 同 [`gemm_tile`]：`c` 起的 m 行（行距 ldc）、每行 n 个元素可写，这期间只有本线程访问。
#[allow(clippy::too_many_arguments)]
pub(crate) unsafe fn gemm_serial(
    (m, n, k): (usize, usize, usize),
    a: &[f32],
    lda: usize,
    b: &[f32],
    ldb: usize,
    c: *mut f32,
    ldc: usize,
    init: Init,
    s: &mut Packs,
) {
    check_ab((m, n, k), (a, lda), (b, ldb));
    let kern = kernel::best();
    for i0 in (0..m).step_by(MC) {
        for j0 in (0..n).step_by(NC) {
            let dims = (MC.min(m - i0), NC.min(n - j0), k);
            // SAFETY：子块落在整块里，整块的可写性由调用方保证
            unsafe {
                gemm_tile(
                    kern,
                    dims,
                    &a[i0 * lda..],
                    lda,
                    &b[j0..],
                    ldb,
                    c.add(i0 * ldc + j0),
                    ldc,
                    init.rows_from(i0),
                    s,
                )
            };
        }
    }
}

/// 多线程时 C 的块大小：先按缓存取 MC×NC，块数不够每线程 TILES_PER_THREAD 块时，
/// 先缩 N 向（重复打包 A 的代价小），再缩 M 向。
fn tile_shape(m: usize, n: usize, threads: usize) -> (usize, usize) {
    let (mut mt, mut nt) = (MC.min(m), NC.min(n));
    if threads > 1 {
        let want = threads * TILES_PER_THREAD;
        let tiles = |mt: usize, nt: usize| m.div_ceil(mt) * n.div_ceil(nt);
        while tiles(mt, nt) < want && nt > MIN_NT {
            nt = (nt / 2).next_multiple_of(NR).max(MIN_NT);
        }
        while tiles(mt, nt) < want && mt > MIN_MT {
            mt = (mt / 2).next_multiple_of(MR).max(MIN_MT);
        }
    }
    (mt, nt)
}

/// C = 初值 + A[m×k]·B[k×n]，工作量够大时多线程。
///
/// # Safety
/// `c` 起的 m 行（行距 ldc ≥ n）、每行 n 个元素可写（`Init::Accumulate` 时还须已初始化），
/// 且调用期间没有别处访问这些位置。
#[allow(clippy::too_many_arguments)]
pub(crate) unsafe fn gemm(
    kern: Kernel,
    (m, n, k): (usize, usize, usize),
    a: &[f32],
    lda: usize,
    b: &[f32],
    ldb: usize,
    c: *mut f32,
    ldc: usize,
    init: Init,
) {
    if m == 0 || n == 0 {
        return;
    }
    check_ab((m, n, k), (a, lda), (b, ldb));
    let threads = par::threads_for(m * n * k, usize::MAX);
    let (mt, nt) = tile_shape(m, n, threads);
    let (tm, tn) = (m.div_ceil(mt), n.div_ceil(nt));
    let cp = SyncPtr::new(c);
    par::for_each(tm * tn, threads, Scratch::take, |s, t| {
        let (i0, j0) = ((t / tn) * mt, (t % tn) * nt);
        let dims = (mt.min(m - i0), nt.min(n - j0), k);
        // SAFETY：每个块号只被领取一次，块与块在 C 里互不重叠；范围由调用方保证
        unsafe {
            gemm_tile(
                kern,
                dims,
                &a[i0 * lda..],
                lda,
                &b[j0..],
                ldb,
                cp.get().add(i0 * ldc + j0),
                ldc,
                init.rows_from(i0),
                &mut s.packs,
            )
        };
    });
}

/// 新建 C[m×n] = 每行偏置（`None` 为 0）+ A[m×k]·B[k×n]，行距 n。
pub(crate) fn gemm_new(
    dims: (usize, usize, usize),
    a: &[f32],
    lda: usize,
    b: &[f32],
    ldb: usize,
    bias: Option<&[f32]>,
) -> Vec<f32> {
    let (m, n, _) = dims;
    if let Some(bias) = bias {
        assert!(bias.len() >= m, "偏置比行数短");
    }
    let mut out = Vec::with_capacity(m * n);
    // SAFETY：容量 m·n，行距 n；Init::Fill 让每个元素先被写入，全部写完才 set_len
    unsafe {
        gemm(
            kernel::best(),
            dims,
            a,
            lda,
            b,
            ldb,
            out.as_mut_ptr(),
            n,
            Init::Fill(bias),
        );
        out.set_len(m * n);
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    /// 两种内核、两种初值都和按定义的三重循环（f64 累加）对照：含 k > KC、m、n 不是 MR、NR 的倍数、
    /// 行距大于列数、k = 0。
    #[test]
    fn kernels_match_naive() {
        // [策略] 覆盖边角：1、k = 0、不足一个微块、恰好一个微块、跨 K 块、跨 M/N 块、足以多线程
        let dims = [
            (1, 1, 1),
            (3, 5, 0),
            (5, 15, 3),
            (6, 16, 256),
            (7, 17, 257),
            (13, 300, 70),
            (121, 33, 520),
            (130, 513, 90),
        ];
        let mut seed = 1u32;
        let mut rnd = move || {
            // [协议] Numerical Recipes 的线性同余参数
            seed = seed.wrapping_mul(1_664_525).wrapping_add(1_013_904_223);
            // [推导] 高 24 位映到 [0, 1)，再映到 [-1, 1)
            (seed >> 8) as f32 / (1u32 << 24) as f32 * 2.0 - 1.0
        };
        for kern in kernel::all() {
            for &(m, n, k) in &dims {
                for fill in [false, true] {
                    // [策略] 行距比列数多 3，检查不会写到行距的空隙里
                    let (lda, ldb, ldc) = (k + 3, n + 3, n + 3);
                    let a: Vec<f32> = (0..m * lda).map(|_| rnd()).collect();
                    let b: Vec<f32> = (0..k * ldb).map(|_| rnd()).collect();
                    let c0: Vec<f32> = (0..m * ldc).map(|_| rnd()).collect();
                    let bias: Vec<f32> = (0..m).map(|_| rnd()).collect();
                    let mut want = c0.clone();
                    for i in 0..m {
                        for j in 0..n {
                            let mut s = 0f64;
                            for p in 0..k {
                                s += a[i * lda + p] as f64 * b[p * ldb + j] as f64;
                            }
                            let base = if fill { bias[i] } else { c0[i * ldc + j] };
                            want[i * ldc + j] = (base as f64 + s) as f32;
                        }
                    }
                    let mut got = c0.clone();
                    let init = if fill {
                        Init::Fill(Some(&bias))
                    } else {
                        Init::Accumulate
                    };
                    // SAFETY：got 有 m 行、行距 ldc，且已初始化
                    unsafe {
                        gemm(
                            kern,
                            (m, n, k),
                            &a,
                            lda,
                            &b,
                            ldb,
                            got.as_mut_ptr(),
                            ldc,
                            init,
                        )
                    };
                    for (idx, (g, w)) in got.iter().zip(&want).enumerate() {
                        // [策略] 相对误差 1e-4，量级不足 1 时按 1 算
                        assert!(
                            (g - w).abs() <= 1e-4 * w.abs().max(1.0),
                            "({m},{n},{k}) fill={fill} 第 {idx} 个：{g} ≠ {w}"
                        );
                        if idx % ldc >= n {
                            assert_eq!(g.to_bits(), c0[idx].to_bits(), "写到了行距空隙");
                        }
                    }
                }
            }
        }
    }
}
