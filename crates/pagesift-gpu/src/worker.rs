//! GPU 工人：一个线程独占 CUDA 上下文，其他线程把工作以闭包形式交给它、等结果。

use std::panic::{AssertUnwindSafe, catch_unwind};
use std::sync::Arc;
use std::sync::mpsc::{RecvTimeoutError, Sender, channel};
use std::thread::JoinHandle;
use std::time::Duration;

use crate::exec::Ctx;
use crate::{Gpu, GpuInfo};

/// 工人线程上执行的一段工作。
type Job = Box<dyn for<'c, 'g> FnOnce(&'c Ctx<'g>) + Send + 'static>;

/// [`Worker::call_timeout`] 没等到结果的原因。
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum WaitError {
    /// 到时间了工作还没做完（它留在队列里或继续跑，结果没人要了）。
    TimedOut,
    /// 工人线程没了，或工作 panic 了。
    Failed(String),
}

/// 工人句柄，丢掉时线程退出并释放上下文。
pub struct Worker {
    tx: Option<Sender<Job>>,
    thread: Option<JoinHandle<()>>,
    info: GpuInfo,
}

impl Worker {
    /// 打开第 `index` 块卡并启动工人线程（内核 JIT 在这里完成）。
    /// 打不开时返回原因，调用方退回 CPU。
    pub fn spawn(index: usize) -> Result<Worker, String> {
        let (tx, rx) = channel::<Job>();
        let (ready_tx, ready_rx) = channel();
        let thread = std::thread::Builder::new()
            .name("pagesift-gpu".into())
            .spawn(move || {
                let gpu = match Gpu::open_index(index).and_then(|g| g.info().map(|i| (g, i))) {
                    Ok((g, info)) => {
                        // 主线程等到这个消息才返回；它已经走了也没关系
                        let _ = ready_tx.send(Ok(info));
                        g
                    }
                    Err(e) => {
                        let _ = ready_tx.send(Err(e));
                        return;
                    }
                };
                let ctx = Ctx::new(&gpu);
                for job in rx {
                    job(&ctx);
                }
            })
            .map_err(|e| format!("failed to start the GPU worker thread: {e}"))?;
        let info = ready_rx
            .recv()
            .map_err(|_| "the GPU worker thread exited during startup".to_owned())??;
        Ok(Worker {
            tx: Some(tx),
            thread: Some(thread),
            info,
        })
    }

    /// 打开时查到的设备情况。
    pub fn info(&self) -> &GpuInfo {
        &self.info
    }

    /// 在工人线程上执行 `f` 并等结果。工作按提交顺序一个接一个执行。
    pub fn call<R: Send>(
        &self,
        f: impl for<'c, 'g> FnOnce(&'c Ctx<'g>) -> R + Send,
    ) -> Result<R, String> {
        let (tx, rx) = channel();
        let job = move |ctx: &Ctx<'_>| {
            // 回复通道断开说明调用方已不在，这时没有人关心结果
            let _ = tx.send(catch_unwind(AssertUnwindSafe(|| f(ctx))));
        };
        let boxed: Box<dyn for<'c, 'g> FnOnce(&'c Ctx<'g>) + Send + '_> = Box::new(job);
        // SAFETY: 擦掉闭包借用的生命周期。下面的 recv 要等到闭包执行完（发回结果）或被丢弃
        // （通道关闭）才返回，所以闭包借用的东西在它存续期间一直有效
        let boxed: Job = unsafe { std::mem::transmute(boxed) };
        let tx = self.tx.as_ref().expect("工人只在 Drop 里关闭");
        tx.send(boxed)
            .map_err(|_| "the GPU worker thread has exited".to_owned())?;
        match rx.recv() {
            Ok(Ok(r)) => Ok(r),
            Ok(Err(_)) => Err("a job on the GPU worker panicked".into()),
            Err(_) => Err("the GPU worker thread has exited".into()),
        }
    }
}

impl Worker {
    /// 同 [`Worker::call`]，但最多等 `wait`。调用方到时间可以走开，所以闭包不能借东西（`'static`）。
    pub fn call_timeout<R: Send + 'static>(
        &self,
        f: impl for<'c, 'g> FnOnce(&'c Ctx<'g>) -> R + Send + 'static,
        wait: Duration,
    ) -> Result<R, WaitError> {
        let (tx, rx) = channel();
        let job: Job = Box::new(move |ctx: &Ctx<'_>| {
            // 回复通道断开说明调用方已不在，这时没有人关心结果
            let _ = tx.send(catch_unwind(AssertUnwindSafe(|| f(ctx))));
        });
        let tx = self.tx.as_ref().expect("工人只在 Drop 里关闭");
        tx.send(job)
            .map_err(|_| WaitError::Failed("the GPU worker thread has exited".into()))?;
        match rx.recv_timeout(wait) {
            Ok(Ok(r)) => Ok(r),
            Ok(Err(_)) => Err(WaitError::Failed("a job on the GPU worker panicked".into())),
            Err(RecvTimeoutError::Timeout) => Err(WaitError::TimedOut),
            Err(RecvTimeoutError::Disconnected) => {
                Err(WaitError::Failed("the GPU worker thread has exited".into()))
            }
        }
    }
}

impl Drop for Worker {
    fn drop(&mut self) {
        drop(self.tx.take());
        if let Some(t) = self.thread.take() {
            // 线程 panic 也不再往外传：Drop 里没有地方报
            let _ = t.join();
        }
    }
}

/// 把工人包成可以共享的句柄。
pub type SharedWorker = Arc<Worker>;

#[cfg(test)]
mod tests {
    use super::*;

    /// 工作没做完时调用方能走开，之后的工作照常；没有显卡时跳过。
    #[test]
    fn call_timeout_walks_away_from_a_slow_job() {
        let Ok(w) = Worker::spawn(0) else {
            eprintln!("没有能用的 N 卡，跳过");
            return;
        };
        let slow = w.call_timeout(
            |_: &Ctx<'_>| std::thread::sleep(Duration::from_millis(200)),
            Duration::from_millis(1),
        );
        assert_eq!(slow, Err(WaitError::TimedOut));
        let next = w.call_timeout(|_: &Ctx<'_>| 7, Duration::from_secs(60));
        assert_eq!(next, Ok(7));
    }
}
