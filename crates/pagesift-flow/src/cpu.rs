//! CPU 工人：每核一个常驻线程；各有本地队列，空闲时从相邻工人的队尾偷任务。
//!
//! 取代"信号量 + `spawn_blocking`"：阻塞线程池按需起线程，几十个线程各持一块 malloc arena，
//! 线程局部的工作区（HTML 解析缓冲、提取缓冲）也随线程冷启动。固定的工人让这些工作区一直是热的。
//!
//! 同时在跑的 CPU 任务数等于工人数，与原来的许可数相同；排队的任务不占线程。
//!
//! 排队有上限：在跑加排队的任务合计不超过 2 × 工人数（每个工人一个在跑、一个排着）。
//! [`Pool::run`] 拿不到名额时异步地等（FIFO），等待中的调用方被取消就不占名额；
//! [`Pool::spawn`]（不等结果的解压）拿不到名额时在调用方线程就地执行，与"不值得交出去"时相同。
//! 0.90 的队列没有上限：慢任务加大量提交时排队数和它们持有的输入一起无界增长。
//!
//! 工人不逐核硬绑，只绑到所在 NUMA 节点的 CPU 集合，节点内由调度器安排：
//! - 逐核硬绑时工人和同核的异步线程互相抢、又迁不走，本机并发 16 吞吐掉一半；0.70 在 192 vCPU 上
//!   逐逻辑核硬绑 -33.9%、绑节点集合 +8.63%（docs/history/0.70_p2_step2_numa_executor.md）；
//! - 偷任务先偷同节点的工人，整个节点都没活时才跨节点（0.70 实测跨节点任务只占 0.163%，
//!   docs/history/0.70_p2_step4_3_numa_dispatch.md）。单节点机器上等于不绑。
//!
//! 没有全局锁：每个工人自己睡（`thread::park`），提交方只唤醒一个在睡的工人。全局只有一个
//! "排队数"原子量（独占一条缓存行）；派发延迟按工人各记各的，读的时候汇总。核多（上百）时，
//! 每次提交都去抢同一把锁、同一条缓存行，会成为全池的瓶颈（0.50 的全局信号量就是这样）。
//! 派发延迟用按精度截窗的均值（`pagesift_stat::window`），不用累计均值。
//!
//! 空闲时不调 `malloc_trim`：实测并发 1 时每个请求后都会全池空闲，trim 把堆页还给系统，
//! 下一个请求再缺页，单请求延迟中位数慢 8%。什么时候还内存由内存预算决定（`pagesift_net::resources`）。

use std::cell::Cell;
use std::collections::VecDeque;
use std::panic::{AssertUnwindSafe, catch_unwind};
use std::sync::atomic::{AtomicBool, AtomicU64, AtomicUsize, Ordering};
use std::sync::{Arc, Mutex, OnceLock};
use std::thread::{JoinHandle, Thread};
use std::time::{Duration, Instant};

use pagesift_stat::Delta;
use pagesift_stat::window::WindowMean;

type Job = Box<dyn FnOnce() + Send + 'static>;

/// 任务在工人上 panic 了。工人本身照常继续。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Panicked;

impl std::fmt::Display for Panicked {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("CPU task panicked")
    }
}

impl std::error::Error for Panicked {}

/// 一个工人的状态，按缓存行对齐，避免相邻工人互相失效。
/// [协议] 128 字节：x86 相邻行预取成对取 64 字节行、Apple M 系列行宽 128（同 crossbeam `CachePadded`）
#[repr(align(128))]
struct Slot {
    queue: Mutex<VecDeque<Job>>,
    sleeping: AtomicBool,
    thread: OnceLock<Thread>,
    /// 本工人执行的任务的派发延迟（入队到开始执行）：当前窗口（只有本工人写）和最近发布的窗口均值（纳秒，0 = 还没有）。
    window: Mutex<WindowMean>,
    dispatch_ns: AtomicU64,
    /// 偷任务的顺序：同节点的工人在前，其余节点在后。
    steal: Vec<usize>,
}

/// 独占一条缓存行的原子量。
/// [协议] 128 字节对齐，理由同 [`Slot`]
#[repr(align(128))]
struct Padded<T>(T);

thread_local! {
    /// 当前线程是第几个工人（不是工人时为 `usize::MAX`）。
    static ME: Cell<usize> = const { Cell::new(usize::MAX) };
}

struct Shared {
    slots: Vec<Slot>,
    /// 在跑加排队的名额（见模块说明）。
    permits: Arc<tokio::sync::Semaphore>,
    /// 已入队、还没被取走的任务数。
    pending: Padded<AtomicUsize>,
    shutdown: AtomicBool,
    next: Padded<AtomicUsize>,
}

impl Shared {
    fn take(&self, me: usize) -> Option<Job> {
        if let Some(job) = lock(&self.slots[me].queue).pop_front() {
            return Some(job);
        }
        // 从队尾偷，减少和队主抢同一端；同节点的在前
        self.slots[me]
            .steal
            .iter()
            .find_map(|&v| lock(&self.slots[v].queue).pop_back())
    }

    fn worker(&self, me: usize, pin: Option<(PinFn, CpuSet)>) {
        if let Some((pin, cpus)) = pin {
            pin(&cpus);
        }
        let slot = &self.slots[me];
        ME.with(|c| c.set(me));
        let _ = slot.thread.set(std::thread::current());
        loop {
            if let Some(job) = self.take(me) {
                self.pending.0.fetch_sub(1, Ordering::AcqRel);
                job();
                continue;
            }
            // 先声明要睡，再看一眼有没有活；提交方先加 pending 再看谁在睡。两边都用 SeqCst，
            // 至少有一方看到对方（Dekker 式握手），不会漏掉唤醒；`unpark` 先到时 `park` 立即返回。
            slot.sleeping.store(true, Ordering::SeqCst);
            let idle = self.pending.0.load(Ordering::SeqCst) == 0;
            if idle && !self.shutdown.load(Ordering::SeqCst) {
                std::thread::park();
            }
            slot.sleeping.store(false, Ordering::SeqCst);
            if idle && self.shutdown.load(Ordering::SeqCst) {
                return;
            }
        }
    }

    /// 唤醒一个在睡的工人：先看任务所在队列的主人，再按编号往后找。
    fn wake_one(&self, at: usize) {
        let n = self.slots.len();
        for d in 0..n {
            let slot = &self.slots[(at + d) % n];
            if slot.sleeping.load(Ordering::SeqCst) {
                if let Some(t) = slot.thread.get() {
                    t.unpark();
                }
                return;
            }
        }
    }
}

fn lock<T>(m: &Mutex<T>) -> std::sync::MutexGuard<'_, T> {
    m.lock().unwrap_or_else(|e| e.into_inner())
}

/// 一组本地的 CPU（一个 NUMA 节点允许使用的逻辑 CPU）。
pub type CpuSet = Vec<usize>;
/// 把当前线程绑到给定 CPU 集合的函数；返回是否成功。
pub type PinFn = fn(&[usize]) -> bool;

/// 把 `workers` 个工人按各组 CPU 数的比例分到各组（最大余数法，Hamilton 1792）：
/// 先给每组份额的整数部分，剩下的按小数部分从大到小各补一个。
fn spread(workers: usize, groups: &[CpuSet]) -> Vec<usize> {
    let total: usize = groups.iter().map(Vec::len).sum();
    if groups.is_empty() || total == 0 {
        return vec![0; workers];
    }
    let quota: Vec<f64> = groups
        .iter()
        .map(|g| workers as f64 * g.len() as f64 / total as f64)
        .collect();
    let mut count: Vec<usize> = quota.iter().map(|q| q.floor() as usize).collect();
    let mut order: Vec<usize> = (0..groups.len()).collect();
    order.sort_by(|&a, &b| quota[b].fract().total_cmp(&quota[a].fract()));
    let left = workers - count.iter().sum::<usize>();
    for &g in order.iter().take(left) {
        count[g] += 1;
    }
    // 按组交错排列工人编号，与异步线程的交错顺序一致
    let mut out = Vec::with_capacity(workers);
    let mut used = vec![0; groups.len()];
    while out.len() < workers {
        for g in 0..groups.len() {
            if used[g] < count[g] && out.len() < workers {
                out.push(g);
                used[g] += 1;
            }
        }
    }
    out
}

/// CPU 工人池。
pub struct Pool {
    shared: Arc<Shared>,
    threads: Mutex<Vec<JoinHandle<()>>>,
}

impl Pool {
    /// 起 `workers` 个工人，不绑核（Python、Node 宿主、测试）。
    pub fn new(workers: usize) -> std::io::Result<Self> {
        Self::with_groups(workers, &[], None)
    }

    /// 起 `workers` 个工人，按 `groups`（各 NUMA 节点的 CPU 集合）分组；给了 `pin` 时每个工人用它
    /// 把自己限制在所在组的 CPU 集合内。`groups` 为空时不分组。
    pub fn with_groups(
        workers: usize,
        groups: &[CpuSet],
        pin: Option<PinFn>,
    ) -> std::io::Result<Self> {
        let n = workers.max(1);
        let group_of = spread(n, groups);
        let steal_order = |me: usize| -> Vec<usize> {
            let (mut near, mut far) = (vec![], vec![]);
            for d in 1..n {
                let v = (me + d) % n;
                if group_of[v] == group_of[me] {
                    near.push(v);
                } else {
                    far.push(v);
                }
            }
            near.extend(far);
            near
        };
        let shared = Arc::new(Shared {
            slots: (0..n)
                .map(|i| Slot {
                    queue: Mutex::new(VecDeque::new()),
                    sleeping: AtomicBool::new(false),
                    thread: OnceLock::new(),
                    window: Mutex::new(WindowMean::default()),
                    dispatch_ns: AtomicU64::new(0),
                    steal: steal_order(i),
                })
                .collect(),
            // [推导] 每个工人一个在跑、一个排队：工人做完手上的立刻有下一个，不空等异步一侧补货；
            // 再多排只是把等待从异步任务挪进队列，不增加吞吐
            permits: Arc::new(tokio::sync::Semaphore::new(n * 2)),
            pending: Padded(AtomicUsize::new(0)),
            shutdown: AtomicBool::new(false),
            next: Padded(AtomicUsize::new(0)),
        });
        let mut threads = Vec::with_capacity(n);
        for (i, &group) in group_of.iter().enumerate() {
            let shared = shared.clone();
            let pin = pin.zip(groups.get(group).cloned());
            threads.push(
                std::thread::Builder::new()
                    .name(format!("pagesift-cpu-{i}"))
                    .spawn(move || shared.worker(i, pin))?,
            );
        }
        Ok(Self {
            shared,
            threads: Mutex::new(threads),
        })
    }

    pub fn workers(&self) -> usize {
        self.shared.slots.len()
    }

    /// 在跑加排队的任务数上限。
    pub fn capacity(&self) -> usize {
        self.workers() * 2
    }

    /// 已入队、还没开始执行的任务数。
    pub fn queued(&self) -> usize {
        self.shared.pending.0.load(Ordering::Relaxed)
    }

    /// 一次跨线程往返（交给工人、再把结果交回）的平均耗时估计：派发延迟的均值 × 2
    /// （去程唤醒工人、回程唤醒等结果的任务，两次唤醒同量级）。还没有样本时为 `None`。
    pub fn round_trip(&self) -> Option<Duration> {
        let (ns, n) = self.shared.slots.iter().fold((0u64, 0u64), |(ns, n), s| {
            match s.dispatch_ns.load(Ordering::Relaxed) {
                0 => (ns, n),
                v => (ns + v, n + 1),
            }
        });
        (n > 0).then(|| Duration::from_nanos(ns / n * 2))
    }

    fn submit(&self, key: Option<u64>, job: Job) {
        let shared = &self.shared;
        let n = shared.slots.len();
        let at = match key {
            Some(k) => (k % n as u64) as usize,
            None => shared.next.0.fetch_add(1, Ordering::Relaxed) % n,
        };
        let queued = Instant::now();
        let stats = shared.clone();
        let job: Job = Box::new(move || {
            // 记在实际执行它的工人名下：只有这个线程写，没有争用
            if let Some(s) = stats.slots.get(ME.with(Cell::get)) {
                let waited = queued.elapsed().as_nanos() as f64;
                if let Some(mean) = lock(&s.window).push(waited, Delta::default()) {
                    s.dispatch_ns.store((mean as u64).max(1), Ordering::Relaxed);
                }
            }
            job();
        });
        // 先发布待处理计数再入队：工人不会在 pending 增加之前偷走任务导致 fetch_sub 下溢。
        shared.pending.0.fetch_add(1, Ordering::SeqCst);
        lock(&shared.slots[at].queue).push_back(job);
        shared.wake_one(at);
    }

    /// 在工人上执行 `f`。`key` 相同的任务优先进同一个工人的队列（同一站点的页面复用同一份热缓存）；
    /// `None` 时轮流分配。
    pub async fn run<T: Send + 'static>(
        &self,
        key: Option<u64>,
        f: impl FnOnce() -> T + Send + 'static,
    ) -> Result<T, Panicked> {
        let permit = Arc::clone(&self.shared.permits)
            .acquire_owned()
            .await
            .map_err(|_| Panicked)?;
        let (tx, rx) = tokio::sync::oneshot::channel();
        self.submit(
            key,
            Box::new(move || {
                // 调用方在排队期间已取消（接收端释放）：直接放弃尚未开始的任务并归还名额。
                if tx.is_closed() {
                    drop(permit);
                    return;
                }
                let _ = tx.send(catch_unwind(AssertUnwindSafe(f)));
                drop(permit);
            }),
        );
        match rx.await {
            Ok(Ok(v)) => Ok(v),
            _ => Err(Panicked),
        }
    }

    /// 把一个不需要结果的任务交给工人（例如交给 `pagesift-net` 的解压）。
    /// 名额用完时在当前线程就地执行（见模块说明）。
    pub fn spawn(&self, f: impl FnOnce() + Send + 'static) {
        let Ok(permit) = Arc::clone(&self.shared.permits).try_acquire_owned() else {
            let _ = catch_unwind(AssertUnwindSafe(f));
            return;
        };
        self.submit(
            None,
            Box::new(move || {
                let _ = catch_unwind(AssertUnwindSafe(f));
                drop(permit);
            }),
        );
    }
}

impl Drop for Pool {
    fn drop(&mut self) {
        self.shared.shutdown.store(true, Ordering::SeqCst);
        for s in &self.shared.slots {
            if let Some(t) = s.thread.get() {
                t.unpark();
            }
        }
        for t in lock(&self.threads).drain(..) {
            let _ = t.join();
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test(flavor = "multi_thread")]
    async fn runs_everything_bounded_by_workers_and_survives_panics() {
        let pool = Arc::new(Pool::new(3).unwrap());
        let running = Arc::new(AtomicUsize::new(0));
        let peak = Arc::new(AtomicUsize::new(0));
        let tasks: Vec<_> = (0..200u64)
            .map(|i| {
                let (pool, running, peak) = (pool.clone(), running.clone(), peak.clone());
                tokio::spawn(async move {
                    pool.run(Some(i), move || {
                        let now = running.fetch_add(1, Ordering::SeqCst) + 1;
                        peak.fetch_max(now, Ordering::SeqCst);
                        std::thread::yield_now();
                        running.fetch_sub(1, Ordering::SeqCst);
                        i * 2
                    })
                    .await
                })
            })
            .collect();
        let mut sum = 0;
        for t in tasks {
            sum += t.await.unwrap().unwrap();
        }
        assert_eq!(sum, (0..200u64).map(|i| i * 2).sum::<u64>());
        assert!(peak.load(Ordering::SeqCst) <= 3);
        assert_eq!(
            pool.run(None, || panic!("boom")).await,
            Err::<(), _>(Panicked)
        );
        assert_eq!(pool.run(None, || 7).await, Ok(7));
    }

    #[test]
    fn spreads_workers_over_numa_groups_in_proportion_and_steals_locally_first() {
        assert_eq!(spread(4, &[vec![0, 1], vec![2, 3]]), [0, 1, 0, 1]);
        let g = spread(6, &[vec![0, 1, 2, 3], vec![4, 5]]);
        assert_eq!((g.iter().filter(|&&x| x == 0).count(), g.len()), (4, 6));
        assert_eq!(spread(3, &[]), [0, 0, 0]);
        let pool = Pool::with_groups(4, &[vec![0, 1], vec![2, 3]], None).unwrap();
        // 工人 0、2 在组 0，1、3 在组 1：工人 0 先偷 2
        assert_eq!(pool.shared.slots[0].steal[0], 2);
    }

    #[test]
    fn idle_workers_wake_for_work_submitted_one_at_a_time() {
        // 每次只交一个任务、等它做完再交下一个：每次都要唤醒一个睡着的工人，漏唤醒就会卡住
        let pool = Pool::new(4).unwrap();
        let rt = tokio::runtime::Builder::new_current_thread()
            .build()
            .unwrap();
        for i in 0..2000u64 {
            assert_eq!(rt.block_on(pool.run(None, move || i)), Ok(i));
        }
        // 不断言 round_trip：它要等窗口均值精度达标才发布，机器满载时唤醒延迟抖动大，可能一直是 None
    }

    /// 慢任务加大量提交：排队数不超过上限，等待中被取消的调用方不留在队列里。
    #[tokio::test(flavor = "multi_thread")]
    async fn queue_is_bounded_and_cancelled_waiters_leave_no_work() {
        let pool = Arc::new(Pool::new(2).unwrap());
        let gate = Arc::new(std::sync::Barrier::new(3));
        let ran = Arc::new(AtomicUsize::new(0));
        let mut tasks = Vec::new();
        for _ in 0..100 {
            let (pool, gate, ran) = (pool.clone(), gate.clone(), ran.clone());
            tasks.push(tokio::spawn(async move {
                pool.run(None, move || {
                    if ran.fetch_add(1, Ordering::SeqCst) < 2 {
                        gate.wait();
                    }
                })
                .await
            }));
        }
        // 两个工人卡在 gate 上：队列里最多再有 capacity − workers 个
        tokio::time::sleep(Duration::from_millis(200)).await;
        assert!(
            pool.queued() <= pool.capacity() - pool.workers(),
            "{}",
            pool.queued()
        );
        // 取消一半等待者，放开工人
        for t in tasks.iter().skip(50) {
            t.abort();
        }
        gate.wait();
        let mut done = 0;
        for t in tasks {
            if let Ok(r) = t.await {
                r.unwrap();
                done += 1;
            }
        }
        assert!(done >= 50);
        assert_eq!(ran.load(Ordering::SeqCst), done);
        assert_eq!(pool.queued(), 0);
        // 名额用完时 spawn 就地执行
        let here = std::thread::current().id();
        let (tx, rx) = std::sync::mpsc::channel();
        let held: Vec<_> = (0..pool.capacity())
            .map(|_| pool.shared.permits.clone().try_acquire_owned().unwrap())
            .collect();
        pool.spawn(move || tx.send(std::thread::current().id()).unwrap());
        assert_eq!(rx.recv().unwrap(), here);
        drop(held);
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn cancelled_queued_run_skips_closure_execution_and_keeps_pending_exact() {
        let pool = Arc::new(Pool::new(1).unwrap());
        let entered = Arc::new(std::sync::Barrier::new(2));
        let release = Arc::new(std::sync::Barrier::new(2));
        let blocker = tokio::spawn({
            let (pool, entered, release) = (pool.clone(), entered.clone(), release.clone());
            async move {
                pool.run(None, move || {
                    entered.wait();
                    release.wait();
                })
                .await
            }
        });
        entered.wait();

        let executed = Arc::new(AtomicUsize::new(0));
        let (done_tx, mut done_rx) = tokio::sync::oneshot::channel::<()>();
        let executed_clone = executed.clone();
        let victim = tokio::spawn({
            let pool = pool.clone();
            async move {
                pool.run(None, move || {
                    executed_clone.fetch_add(1, Ordering::SeqCst);
                    let _ = done_tx.send(());
                })
                .await
            }
        });
        // 等待 queued task 入队后中止任务，再放行阻塞中的唯一工人。
        for _ in 0..100 {
            if pool.queued() > 0 {
                break;
            }
            tokio::task::yield_now().await;
        }
        victim.abort();
        let _ = victim.await;
        // 确认 receiver 已关闭后再放行工人，确保受害任务在弹出后跳过闭包的执行。
        assert!(done_rx.try_recv().is_err());
        release.wait();
        blocker.await.unwrap().unwrap();
        assert_eq!(pool.run(None, || 42).await, Ok(42));
        assert_eq!(executed.load(Ordering::SeqCst), 0);
        assert!(pool.queued() <= pool.capacity());
    }
}
