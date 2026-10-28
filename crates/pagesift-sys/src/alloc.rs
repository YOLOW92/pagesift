//! 小块内存的分配缓存（全局分配器 [`SmallCache`]）。
//!
//! 剖析（callgrind）里 PDF 转换约一成、xlsx 约 6% 的指令在 glibc 的 malloc/free 里：几十万个
//! 几十字节的小对象一个个分配、释放。192 vCPU 双路机器上真实网页的正文抽取过了 32 线程反而
//! 变慢，工作集是 DOM 和字符串分配，也是卡在分配器上（`docs/history/0.70_p4_step4_2_e1.md`）。
//! 这里按"顺应计算机"的办法接管 512 字节以内、对齐不超过 16 字节的分配：
//!
//! - 16 档大小（16、32 … 512 字节），尺寸到档位查表，没有分支；
//! - 每个线程每档一条空闲链表（空闲块的头 8 字节存下一块），后进先出：刚释放的块马上被复用，
//!   它还在 CPU 缓存里；分配、释放都不加锁、不碰原子变量；
//! - 链表空了从系统要一块 64 KB 的连续内存，按需从前往后一块块切（同一档的对象挨在一起，
//!   顺着地址走）；没切到的部分不碰，不占缓存也不占物理页；
//! - 线程缓存和全局池之间整批搬（一批 32 块串成一条链），全局池里存的是"批"的链表，
//!   锁里只摘、挂一个指针，不在锁里走链表；
//! - 全局池按 NUMA 节点分片：线程只和自己所在节点的池打交道。192 vCPU 那轮的教训是跨节点
//!   共享的东西会让缓存行在两个 Socket 之间来回搬，局部性优先（`0.70_p1_cpu_scaling_usl.md`）；
//!   分片按节点而不按 CPU，线程在节点内怎么迁移都不影响（`0.70_p2_step2_numa_executor.md`
//!   证伪了逐逻辑 CPU 绑定）。块在哪个节点释放就留在哪个节点：同一轮还证实远端首次分配不是
//!   主因，不值得为每次释放查块的归属；
//! - 每个线程缓存的总量不超过它分到的那份 L2（L2 大小 ÷ 共用它的逻辑 CPU 数）：超出的块
//!   反正已经不在这个核的缓存里，留在线程里没有好处，还会在 192 个线程时把固定开销堆大
//!   （`0.70_p4_memory_envelope.md`）。每档的上限从一批起步，向全局池要一次涨一批：只在
//!   线程真的反复要这一档时才多留；
//! - 线程退出时缓存整体还给全局池。
//!
//! 更大的、对齐要求更高的照旧交给系统分配器（大块还受 [`crate::tune_allocator`] 的 mmap 阈值
//! 管着，用完马上还给系统）。切出来的 64 KB 块不还给系统：小对象的峰值用量通常不大，留着
//! 反复用。

#![allow(unsafe_code)]

use std::alloc::{GlobalAlloc, Layout, System};
use std::cell::{Cell, UnsafeCell};
use std::ptr;
use std::sync::atomic::{AtomicBool, AtomicU8, AtomicUsize, Ordering};

/// 各档的块大小。都是 16 的倍数：从 64 字节对齐的大块切出来，每块都 16 字节对齐。
const SIZES: [usize; 16] = [
    16, 32, 48, 64, 80, 96, 112, 128, 160, 192, 224, 256, 320, 384, 448, 512,
];
const CLASSES: usize = SIZES.len();
const MAX_SMALL: usize = 512;
const MAX_ALIGN: usize = 16;
/// 向系统要的大块。
const CHUNK: usize = 64 * 1024;
/// 线程缓存和全局池之间一次搬多少块。
const BATCH: usize = 32;
/// 全局池的分片数，节点号对它取模。只是容量：单节点机器只用第 0 片，没碰过的分片不占物理内存。
const SHARDS: usize = 64;
/// CPU 号到分片的表能登记的 CPU 数，更大的 CPU 号归第 0 片。
const MAX_CPUS: usize = 4096;

/// `(size + 15) / 16` → 档位。
const CLASS_OF: [u8; MAX_SMALL / 16 + 1] = {
    let mut t = [0u8; MAX_SMALL / 16 + 1];
    let mut i = 0;
    while i < t.len() {
        let size = if i == 0 { 1 } else { i * 16 };
        let mut c = 0;
        while SIZES[c] < size {
            c += 1;
        }
        t[i] = c as u8;
        i += 1;
    }
    t
};

/// 这次分配归不归缓存管，归的话是哪一档。
#[inline]
fn class(layout: Layout) -> Option<usize> {
    (layout.size() <= MAX_SMALL && layout.align() <= MAX_ALIGN)
        .then(|| usize::from(CLASS_OF[layout.size().div_ceil(16)]))
}

/// 空闲块：头 8 字节存链上的下一块；一批的第一块再用接下来 8 字节存池里的下一批
/// （最小的块 16 字节，放得下）。
struct Node {
    next: *mut Node,
    next_batch: *mut Node,
}

/// 全局池一个分片的一档：自旋锁保护的"批"链表。
///
/// 每个池独占 128 字节：x86 的相邻行预取一次拉两条 64 字节缓存行，各档的锁要是挨着，
/// 一个线程拿 16 字节档的锁会把隔壁 32 字节档的锁所在的行也抢走。
#[repr(align(128))]
struct Pool {
    locked: AtomicBool,
    batches: UnsafeCell<*mut Node>,
}

// SAFETY: batches 只在持有 locked 时读写。
unsafe impl Sync for Pool {}

impl Pool {
    const fn new() -> Self {
        Pool {
            locked: AtomicBool::new(false),
            batches: UnsafeCell::new(ptr::null_mut()),
        }
    }

    fn with<T>(&self, f: impl FnOnce(&mut *mut Node) -> T) -> T {
        let mut spins = 0u32;
        // 先只读着等，锁空了再抢：等的线程不会反复把缓存行抢成独占
        while self
            .locked
            .compare_exchange_weak(false, true, Ordering::Acquire, Ordering::Relaxed)
            .is_err()
        {
            while self.locked.load(Ordering::Relaxed) {
                spins += 1;
                if spins < 64 {
                    std::hint::spin_loop();
                } else {
                    std::thread::yield_now();
                }
            }
        }
        // SAFETY: 拿到了锁，独占 batches。
        let r = f(unsafe { &mut *self.batches.get() });
        self.locked.store(false, Ordering::Release);
        r
    }

    /// 挂上一条以 null 结尾的链（一批）。
    unsafe fn push(&self, first: *mut Node) {
        self.with(|batches| {
            // SAFETY: first 是调用方独占的空闲块。
            unsafe { (*first).next_batch = *batches };
            *batches = first;
        });
    }

    /// 摘下一批，池空了返回 null。
    unsafe fn pop(&self) -> *mut Node {
        self.with(|batches| {
            let first = *batches;
            if !first.is_null() {
                // SAFETY: 池里每批的第一块存着下一批。
                *batches = unsafe { (*first).next_batch };
            }
            first
        })
    }
}

static POOLS: [[Pool; CLASSES]; SHARDS] = [const { [const { Pool::new() }; CLASSES] }; SHARDS];

/// 有不止一个节点时为 true；否则不必查当前 CPU。
static MULTI_NODE: AtomicBool = AtomicBool::new(false);
/// CPU 号 → 分片。
static CPU_SHARD: [AtomicU8; MAX_CPUS] = [const { AtomicU8::new(0) }; MAX_CPUS];
/// 每个线程最多缓存多少字节；没探测到 L2 时每档一个大块。
static BUDGET: AtomicUsize = AtomicUsize::new(CLASSES * CHUNK);

/// 当前线程所在节点的分片。只在和全局池打交道时调用（每几十次分配一次）。
fn shard() -> &'static [Pool; CLASSES] {
    &POOLS[current_shard()]
}

fn current_shard() -> usize {
    if !MULTI_NODE.load(Ordering::Relaxed) {
        return 0;
    }
    #[cfg(all(target_os = "linux", target_env = "gnu"))]
    {
        // SAFETY: 无参数；glibc 走 rseq/vDSO，不进内核。
        let cpu = unsafe { libc::sched_getcpu() };
        if let Ok(cpu) = usize::try_from(cpu)
            && cpu < MAX_CPUS
        {
            return usize::from(CPU_SHARD[cpu].load(Ordering::Relaxed));
        }
    }
    0
}

/// 登记 CPU 拓扑：`cpu_node` 是 (CPU 号, NUMA 节点号)，`l2_share` 是一个逻辑 CPU 分到的 L2 字节数。
pub(crate) fn configure(cpu_node: &[(usize, usize)], l2_share: Option<usize>) {
    let mut nodes = 0u64;
    for &(cpu, node) in cpu_node {
        let s = node % SHARDS;
        nodes |= 1 << s;
        if cpu < MAX_CPUS {
            CPU_SHARD[cpu].store(s as u8, Ordering::Relaxed);
        }
    }
    MULTI_NODE.store(nodes.count_ones() > 1, Ordering::Relaxed);
    if let Some(b) = l2_share.filter(|&b| b > 0) {
        BUDGET.store(b, Ordering::Relaxed);
    }
}

/// 测试用：假装每个 CPU 是一个节点（线程在 CPU 间迁移时就会碰到不同的分片），并设线程缓存上限。
#[doc(hidden)]
pub fn small_cache_simulate_numa(budget: usize) {
    let cpus: Vec<(usize, usize)> = (0..MAX_CPUS).map(|c| (c, c)).collect();
    configure(&cpus, Some(budget));
}

/// 一个线程的缓存：每档一条空闲链表、长度和上限，正在切的大块（下一块的位置、剩下几块），
/// 以及链表里的总字节数。
struct Cache {
    head: [Cell<*mut Node>; CLASSES],
    len: [Cell<usize>; CLASSES],
    max: [Cell<usize>; CLASSES],
    bump: [Cell<*mut u8>; CLASSES],
    left: [Cell<usize>; CLASSES],
    bytes: Cell<usize>,
}

thread_local! {
    static CACHE: Cache = const {
        Cache {
            head: [const { Cell::new(ptr::null_mut()) }; CLASSES],
            len: [const { Cell::new(0) }; CLASSES],
            max: [const { Cell::new(BATCH) }; CLASSES],
            bump: [const { Cell::new(ptr::null_mut()) }; CLASSES],
            left: [const { Cell::new(0) }; CLASSES],
            bytes: Cell::new(0),
        }
    };
}

impl Cache {
    #[inline]
    unsafe fn alloc(&self, c: usize) -> *mut u8 {
        let head = self.head[c].get();
        if !head.is_null() {
            // SAFETY: 链表里的块都是本缓存独占的空闲块，头 8 字节是 next。
            self.head[c].set(unsafe { (*head).next });
            self.len[c].set(self.len[c].get() - 1);
            self.bytes.set(self.bytes.get() - SIZES[c]);
            return head.cast();
        }
        unsafe { self.refill(c) }
    }

    /// 链表空了：先接着切手上的大块，切完了从本节点的全局池取一批，再没有才向系统要新的大块。
    #[cold]
    unsafe fn refill(&self, c: usize) -> *mut u8 {
        let size = SIZES[c];
        let left = self.left[c].get();
        if left > 0 {
            let p = self.bump[c].get();
            self.left[c].set(left - 1);
            // SAFETY: 还剩 left 块，下一块在大块之内。
            self.bump[c].set(unsafe { p.add(size) });
            return p;
        }
        // 反复来要说明这个线程确实常用这一档，多留一批
        self.max[c].set(self.max[c].get().saturating_add(BATCH));
        let first = unsafe { shard()[c].pop() };
        if !first.is_null() {
            // SAFETY: 摘下来的是一条以 null 结尾的链，归本缓存所有；块是本线程马上要用的，
            // 在锁外数一遍顺便把它们拉进缓存。
            let rest = unsafe { (*first).next };
            let mut n = 0;
            let mut p = rest;
            while !p.is_null() {
                n += 1;
                p = unsafe { (*p).next };
            }
            self.head[c].set(rest);
            self.len[c].set(n);
            self.bytes.set(self.bytes.get() + n * size);
            return first.cast();
        }
        // SAFETY: CHUNK、64 都是合法的大小和对齐。
        let chunk = unsafe { System.alloc(Layout::from_size_align_unchecked(CHUNK, 64)) };
        if chunk.is_null() {
            return ptr::null_mut();
        }
        // 切块的是本线程，按 Linux 的首次触碰，这块内存落在本线程所在的节点上。
        // SAFETY: 第二块还在大块之内。
        self.bump[c].set(unsafe { chunk.add(size) });
        self.left[c].set(CHUNK / size - 1);
        chunk
    }

    #[inline]
    unsafe fn free(&self, c: usize, p: *mut u8) {
        let (len, bytes) = unsafe { self.free_quiet(c, p) };
        if len > self.max[c].get() || bytes > BUDGET.load(Ordering::Relaxed) {
            unsafe { self.spill(c) };
        }
    }

    /// 挂回链表，不看上限；返回这一档的长度和总字节数。
    #[inline]
    unsafe fn free_quiet(&self, c: usize, p: *mut u8) -> (usize, usize) {
        let node = p.cast::<Node>();
        // SAFETY: p 是本分配器分出去、调用方已经不再使用的一块，至少 16 字节。
        unsafe { (&raw mut (*node).next).write(self.head[c].get()) };
        self.head[c].set(node);
        let len = self.len[c].get() + 1;
        self.len[c].set(len);
        let bytes = self.bytes.get() + SIZES[c];
        self.bytes.set(bytes);
        (len, bytes)
    }

    /// 这一档留得比线程用得多，或者整个线程缓存超出了它那份 L2：挪一批到全局池；
    /// 超出 L2 的话每档都挪走一半。
    #[cold]
    unsafe fn spill(&self, c: usize) {
        let pools = shard();
        if self.len[c].get() > self.max[c].get() {
            unsafe { self.release(pools, c, BATCH) };
        }
        if self.bytes.get() > BUDGET.load(Ordering::Relaxed) {
            for c in 0..CLASSES {
                let n = self.len[c].get().div_ceil(2);
                unsafe { self.release(pools, c, n) };
            }
        }
    }

    /// 从第 c 档摘下 n 块，按批挂到全局池。
    unsafe fn release(&self, pools: &[Pool; CLASSES], c: usize, mut n: usize) {
        while n > 0 {
            let mut head = self.head[c].get();
            let (first, k) = unsafe { take(&mut head, n.min(BATCH)) };
            if k == 0 {
                return;
            }
            self.head[c].set(head);
            self.len[c].set(self.len[c].get() - k);
            self.bytes.set(self.bytes.get() - k * SIZES[c]);
            unsafe { pools[c].push(first) };
            n -= k;
        }
    }
}

impl Drop for Cache {
    /// 线程退出：缓存整体还给所在节点的全局池。
    fn drop(&mut self) {
        let pools = shard();
        for (c, &size) in SIZES.iter().enumerate() {
            // 没切完的大块也串成链交出去
            for _ in 0..self.left[c].replace(0) {
                let p = self.bump[c].get();
                // SAFETY: 大块里还没分出去的一块。
                self.bump[c].set(unsafe { p.add(size) });
                unsafe { self.free_quiet(c, p) };
            }
            let n = self.len[c].get();
            unsafe { self.release(pools, c, n) };
        }
    }
}

/// 从链表 `head` 头上摘下最多 `n` 块：返回摘下的那条链（以 null 结尾）和块数。
/// 只在锁外、对线程自己的链表用。
unsafe fn take(head: &mut *mut Node, n: usize) -> (*mut Node, usize) {
    let first = *head;
    if first.is_null() {
        return (first, 0);
    }
    let mut last = first;
    let mut count = 1;
    // SAFETY: 链上的块都是空闲块，头 8 字节是 next。
    while count < n && !unsafe { (*last).next }.is_null() {
        last = unsafe { (*last).next };
        count += 1;
    }
    *head = unsafe { (*last).next };
    unsafe { (*last).next = ptr::null_mut() };
    (first, count)
}

/// 线程缓存已经销毁（线程退出途中）时直接走全局池：摘一批，留一块，其余挂回去。
unsafe fn global_alloc(c: usize) -> *mut u8 {
    let pools = shard();
    let first = unsafe { pools[c].pop() };
    if !first.is_null() {
        // SAFETY: first 是独占的一批。
        let rest = unsafe { (*first).next };
        if !rest.is_null() {
            unsafe { pools[c].push(rest) };
        }
        return first.cast();
    }
    // SAFETY: SIZES[c] 是 16 的倍数，对齐 16 合法。
    unsafe { System.alloc(Layout::from_size_align_unchecked(SIZES[c], MAX_ALIGN)) }
}

/// 小块分配缓存。只应该由 pagesift 自己的可执行程序用作全局分配器（`#[global_allocator]`）；
/// Python、Node 绑定跑在别人的进程里，不替宿主换分配器。
pub struct SmallCache;

// SAFETY: 小块从 64 字节对齐的大块里按 16 的倍数切出，满足 ≤ 16 的对齐；同一块在被释放之前
// 不会再分出去（只在空闲链表里）；其余请求原样交给 System。
unsafe impl GlobalAlloc for SmallCache {
    #[inline]
    unsafe fn alloc(&self, layout: Layout) -> *mut u8 {
        match class(layout) {
            Some(c) => CACHE
                .try_with(|k| unsafe { k.alloc(c) })
                .unwrap_or_else(|_| unsafe { global_alloc(c) }),
            None => unsafe { System.alloc(layout) },
        }
    }

    #[inline]
    unsafe fn dealloc(&self, p: *mut u8, layout: Layout) {
        match class(layout) {
            Some(c) => {
                if CACHE.try_with(|k| unsafe { k.free(c, p) }).is_err() {
                    let node = p.cast::<Node>();
                    // SAFETY: 调用方不再使用 p；单块成一批。
                    unsafe {
                        (&raw mut (*node).next).write(ptr::null_mut());
                        shard()[c].push(node);
                    }
                }
            }
            None => unsafe { System.dealloc(p, layout) },
        }
    }

    #[inline]
    unsafe fn alloc_zeroed(&self, layout: Layout) -> *mut u8 {
        if class(layout).is_none() {
            return unsafe { System.alloc_zeroed(layout) };
        }
        let p = unsafe { self.alloc(layout) };
        if !p.is_null() {
            unsafe { ptr::write_bytes(p, 0, layout.size()) };
        }
        p
    }

    #[inline]
    unsafe fn realloc(&self, p: *mut u8, layout: Layout, new_size: usize) -> *mut u8 {
        // SAFETY: 调用方保证 new_size 按 layout 的对齐取整后不溢出。
        let new = unsafe { Layout::from_size_align_unchecked(new_size, layout.align()) };
        match (class(layout), class(new)) {
            // 同一档：块本来就够大
            (Some(a), Some(b)) if a == b => p,
            // 都是大块：让系统原地扩
            (None, None) => unsafe { System.realloc(p, layout, new_size) },
            _ => {
                let q = unsafe { self.alloc(new) };
                if !q.is_null() {
                    unsafe {
                        ptr::copy_nonoverlapping(p, q, layout.size().min(new_size));
                        self.dealloc(p, layout);
                    }
                }
                q
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn every_small_size_maps_to_the_smallest_class_that_fits() {
        for size in 1..=MAX_SMALL {
            let c = class(Layout::from_size_align(size, 8).unwrap()).unwrap();
            assert!(SIZES[c] >= size, "{size}");
            assert!(c == 0 || SIZES[c - 1] < size, "{size}");
        }
        assert_eq!(class(Layout::from_size_align(0, 1).unwrap()), Some(0));
        assert_eq!(class(Layout::from_size_align(513, 8).unwrap()), None);
        assert_eq!(class(Layout::from_size_align(64, 32).unwrap()), None);
    }

    #[test]
    fn a_node_fits_in_the_smallest_block_and_pools_do_not_share_lines() {
        assert!(std::mem::size_of::<Node>() <= SIZES[0]);
        assert_eq!(std::mem::align_of::<Pool>(), 128);
    }
}
