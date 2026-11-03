//! 多线程的小工具：只用 `std::thread::scope`，每次调用现开线程、用完即收。
//! 开线程有固定代价（几十微秒），所以按工作量决定开几个，小张量直接在当前线程算。

use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Mutex, OnceLock};
use std::thread::Scope;

/// [策略] 每个线程至少分到的工作量（约等于乘加次数）。实测开一个线程连同收回约 11 µs，
/// 单核 AVX2 乘加约 50 G 次/秒，2²¹ ≈ 2 M 次约 40 µs，开线程的代价约为所得的四分之一。
const MIN_WORK_PER_THREAD: usize = 1 << 21;

/// 机器可用的线程数（`available_parallelism`，取不到时按 1）。
pub(crate) fn max_threads() -> usize {
    static N: OnceLock<usize> = OnceLock::new();
    *N.get_or_init(|| std::thread::available_parallelism().map_or(1, |n| n.get()))
}

/// 工作量 `work`（约等于乘加次数）、最多能分成 `items` 份时该开几个线程（含当前线程）。
pub(crate) fn threads_for(work: usize, items: usize) -> usize {
    (work / MIN_WORK_PER_THREAD)
        .clamp(1, max_threads())
        .min(items.max(1))
}

/// 跨线程传可写指针。调用方保证各线程写的位置互不重叠，且指针在 scope 结束前有效。
#[derive(Clone, Copy)]
pub(crate) struct SyncPtr(*mut f32);

// SAFETY：只是把指针带进 scope 里的线程；写入不重叠由使用处保证（见各处 SAFETY 注释）。
unsafe impl Send for SyncPtr {}
unsafe impl Sync for SyncPtr {}

impl SyncPtr {
    pub(crate) fn new(p: *mut f32) -> SyncPtr {
        SyncPtr(p)
    }

    /// 取指针（用方法取而不是读字段，闭包才会捕获整个 `SyncPtr` 而不是裸指针）。
    pub(crate) fn get(self) -> *mut f32 {
        self.0
    }
}

/// 在 scope 里开出编号 `lo+1..hi` 的线程，当前线程做 `lo`。树状地开：每个线程把自己区间的后一半
/// 交给一个新线程、再分剩下的前一半。实测开一个线程连同收回约 11 µs，逐个开 16 个要 160 µs，
/// 树状只要 log₂16 = 4 轮。
fn fan_out<'scope, 'env, F>(s: &'scope Scope<'scope, 'env>, lo: usize, hi: usize, f: &'scope F)
where
    F: Fn(usize) + Sync,
{
    let mut hi = hi;
    while hi - lo > 1 {
        let (mid, end) = (lo + (hi - lo) / 2, hi);
        s.spawn(move || fan_out(s, mid, end, f));
        hi = mid;
    }
    f(lo);
}

/// 把 `0..items` 分给 `threads` 个线程动态领取（原子计数），每个线程先用 `init` 取自己的
/// 临时状态（如打包缓冲），再对领到的每一项调 `f`。`threads ≤ 1` 时在当前线程顺序做完。
pub(crate) fn for_each<S>(
    items: usize,
    threads: usize,
    init: impl Fn() -> S + Sync,
    f: impl Fn(&mut S, usize) + Sync,
) {
    let threads = threads.min(items);
    if threads <= 1 {
        let mut s = init();
        for i in 0..items {
            f(&mut s, i);
        }
        return;
    }
    let next = AtomicUsize::new(0);
    let work = |_: usize| {
        let mut s = init();
        loop {
            let i = next.fetch_add(1, Ordering::Relaxed);
            if i >= items {
                break;
            }
            f(&mut s, i);
        }
    };
    std::thread::scope(|sc| fan_out(sc, 0, threads, &work));
}

/// 一段：(段号, 段)。
type Part<'a> = (usize, &'a mut [f32]);

/// 把 `data` 按 `chunk` 个元素一段切开，连续的若干段归一组，每组交给一个线程；`f(段号, 段)`。
/// 各段工作量相近时用它。
pub(crate) fn for_chunks_mut(
    data: &mut [f32],
    chunk: usize,
    threads: usize,
    f: impl Fn(usize, &mut [f32]) + Sync,
) {
    let mut parts: Vec<Part> = data.chunks_mut(chunk.max(1)).enumerate().collect();
    let threads = threads.min(parts.len());
    if threads <= 1 {
        for (i, s) in parts {
            f(i, s);
        }
        return;
    }
    let per = parts.len().div_ceil(threads);
    // 每组一把锁只是为了把可变借用安全地交给领到它的线程，不会有争用
    let groups: Vec<Mutex<&mut [Part]>> = parts.chunks_mut(per).map(Mutex::new).collect();
    for_each(
        groups.len(),
        threads,
        || (),
        |_, gi| {
            let mut g = groups[gi].lock().unwrap_or_else(|e| e.into_inner());
            for (i, s) in g.iter_mut() {
                f(*i, s);
            }
        },
    );
}
