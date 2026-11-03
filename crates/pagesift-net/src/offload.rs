//! CPU 活（解压）交给调用方提供的执行器，还是就地做：按代价决定。
//!
//! 交给执行器要跨线程一来一回（唤醒工人、再唤醒等结果的异步任务），小响应的解压比这一来回还快。
//! 规则（成本比较）：预计就地耗时 = 字节数 × 每字节耗时，大于一次往返的耗时才交出去。
//! 两项都在线测：每字节耗时是解压（就地和交出去的）的总耗时 / 总字节数的比值估计，
//! 每个线程按精度截窗（`pagesift_stat::window::WindowRatio`），窗口够准时发布到一个共享原子量；
//! 往返耗时由执行器提供。任何一项还没有测量时就地执行，和 0.81 一样。
//!
//! 本 crate 不依赖上层：`pagesift` 启动时用 [`set_cpu_executor`] 把它接到自己的 CPU 工人上。
//! 没设置时就地执行（测试、只用网络层的调用方）。

use std::cell::RefCell;
use std::panic::{AssertUnwindSafe, catch_unwind, resume_unwind};
use std::sync::OnceLock;
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::{Duration, Instant};

use pagesift_stat::Delta;
use pagesift_stat::window::WindowRatio;

/// 交给执行器的任务。
pub type CpuJob = Box<dyn FnOnce() + Send + 'static>;

struct Executor {
    spawn: Box<dyn Fn(CpuJob) + Send + Sync + 'static>,
    round_trip: Box<dyn Fn() -> Option<Duration> + Send + Sync + 'static>,
}

static EXECUTOR: OnceLock<Executor> = OnceLock::new();
/// 最近发布的每字节耗时（纳秒，`f64` 的位；0 = 还没有）。
static NS_PER_BYTE: AtomicU64 = AtomicU64::new(0);

thread_local! {
    static WINDOW: RefCell<WindowRatio> = RefCell::new(WindowRatio::default());
}

/// 设置执行器和它的往返耗时估计（进程内只能设置一次，之后的调用被忽略）。
pub fn set_cpu_executor(
    spawn: impl Fn(CpuJob) + Send + Sync + 'static,
    round_trip: impl Fn() -> Option<Duration> + Send + Sync + 'static,
) {
    let _ = EXECUTOR.set(Executor {
        spawn: Box::new(spawn),
        round_trip: Box::new(round_trip),
    });
}

fn record(bytes: usize, took: Duration) {
    let published = WINDOW.with(|w| {
        w.borrow_mut()
            .push(bytes as f64, took.as_nanos() as f64, Delta::default())
    });
    if let Some(r) = published {
        NS_PER_BYTE.store(r.to_bits(), Ordering::Relaxed);
    }
}

fn expected(bytes: usize) -> Option<Duration> {
    match NS_PER_BYTE.load(Ordering::Relaxed) {
        0 => None,
        bits => Some(Duration::from_nanos(
            (f64::from_bits(bits) * bytes as f64) as u64,
        )),
    }
}

/// 处理 `bytes` 字节的 `f`：值得时在执行器上运行并等待结果，否则就地运行。
/// `f` panic 时在调用方原样重新抛出。
pub(crate) async fn on_cpu<T: Send + 'static>(
    bytes: usize,
    f: impl FnOnce() -> T + Send + 'static,
) -> T {
    let timed = move || {
        let t = Instant::now();
        let v = f();
        record(bytes, t.elapsed());
        v
    };
    let Some(exec) = EXECUTOR.get() else {
        return timed();
    };
    let worth =
        matches!(((exec.round_trip)(), expected(bytes)), (Some(hop), Some(work)) if work > hop);
    if !worth {
        return timed();
    }
    let (tx, rx) = tokio::sync::oneshot::channel();
    (exec.spawn)(Box::new(move || {
        let _ = tx.send(catch_unwind(AssertUnwindSafe(timed)));
    }));
    match rx.await {
        Ok(Ok(v)) => v,
        Ok(Err(panic)) => resume_unwind(panic),
        Err(_) => panic!("CPU executor dropped a job"),
    }
}
