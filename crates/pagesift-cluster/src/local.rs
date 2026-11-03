//! 账本前端：在 [`Ledger`] 上实现 [`Coordinator`]。单主机（`--role all`，默认）直接用它
//! （[`LocalCoordinator`]），网关进程内的提交、取消、统计也经它（见 `gateway`）。
//!
//! 单主机下结果按所有权直接交给出口，不序列化；续约、归属、统计都是内存操作，统计只合并一层。
//! 账本带存储时，崩溃后从日志恢复并以 `term+1` 继续，存储格式与网关相同，
//! 所以把这台机器改成 `--role gateway` 不用迁移数据。

use std::sync::Arc;
use std::time::Duration;

use crate::coordinator::{BoxFuture, Coordinator, Deliver};
use crate::journal::Origin;
use crate::ledger::Ledger;
use crate::types::{
    Error, Fence, Lease, Owner, Priors, Renewal, Resources, Scope, SiteKey, StatsDelta, Task,
    TaskId, WorkerId,
};

/// 结果与字节之间的转换：给出时结果与完成标记写进同一条日志记录（网关），否则不序列化（单主机）。
type Journaled<A> = (fn(A) -> Vec<u8>, fn(Vec<u8>) -> A);

pub struct LedgerFront<A> {
    ledger: Arc<Ledger>,
    sink: Arc<dyn Deliver<A>>,
    origin: Origin,
    journaled: Option<Journaled<A>>,
}

/// 单主机的协调器。
pub type LocalCoordinator<A> = LedgerFront<A>;

impl<A: Send + 'static> LedgerFront<A> {
    /// `ledger` 必须以单主机方式打开（`LedgerConfig::new(clock, true)`）。
    /// `boot` 区分本进程的各次启动（统计增量去重用），通常取启动时刻。
    pub fn new(ledger: Arc<Ledger>, sink: Arc<dyn Deliver<A>>, boot: u64) -> Self {
        assert!(ledger.is_local(), "LocalCoordinator needs a local ledger");
        Self::with(ledger, sink, "local", boot, None)
    }

    pub(crate) fn with(
        ledger: Arc<Ledger>,
        sink: Arc<dyn Deliver<A>>,
        name: &str,
        boot: u64,
        journaled: Option<Journaled<A>>,
    ) -> Self {
        let worker = WorkerId::new(name);
        Self {
            ledger,
            sink,
            origin: Origin { worker, boot },
            journaled,
        }
    }

    pub fn ledger(&self) -> &Arc<Ledger> {
        &self.ledger
    }

    /// 提交；`ran` 是工人上报的执行耗时（`None` 时按分配到提交的时间算）。
    /// `by`：提交的工人（网关收到的提交），租约在别的工人手里时拒绝；进程内为 `None`。
    pub(crate) fn commit_with(
        &self,
        by: Option<&WorkerId>,
        task: TaskId,
        fence: Fence,
        result: A,
        ran: Option<Duration>,
    ) -> Result<(), Error> {
        let sink = &self.sink;
        match self.journaled {
            Some((to, from)) => {
                self.ledger
                    .commit_by(by, task, fence, Some(to(result)), ran, |b| {
                        sink.deliver(task, from(b.unwrap_or_default()))
                    })
            }
            None => self
                .ledger
                .commit_by(by, task, fence, None, ran, |_| sink.deliver(task, result)),
        }
        .map(drop)
    }
}

impl<A: Send + 'static> Coordinator<A> for LedgerFront<A> {
    fn submit(&self, task: Task) -> BoxFuture<'_, Result<TaskId, Error>> {
        Box::pin(async move { self.ledger.submit(task) })
    }

    /// 网关的账本按工人分配，进程内拉取总是 `None`。
    fn pull(&self, free: Resources) -> BoxFuture<'_, Option<Lease>> {
        Box::pin(async move { self.ledger.pull(None, &free, Duration::ZERO).ok().flatten() })
    }

    fn renew<'a>(&'a self, lease: &'a Lease) -> BoxFuture<'a, Result<Renewal, Error>> {
        Box::pin(async move {
            self.ledger
                .renew(lease.task, lease.fence(), None, Duration::ZERO)
        })
    }

    fn commit<'a>(&'a self, lease: &'a Lease, result: A) -> BoxFuture<'a, Result<(), Error>> {
        Box::pin(async move { self.commit_with(None, lease.task, lease.fence(), result, None) })
    }

    fn cancel(&self, task: TaskId) -> BoxFuture<'_, Result<bool, Error>> {
        Box::pin(async move { self.ledger.cancel(task) })
    }

    fn owner_of(&self, site: SiteKey) -> Owner {
        self.ledger.owner_of(site)
    }

    fn publish_stats(&self, delta: StatsDelta) {
        // 种类未登记或表损坏是调用方的编程错误，进程内没有别的去处，丢弃
        let _ = self.ledger.merge_stats(&self.origin, delta);
    }

    fn priors(&self, scope: Scope) -> Option<Priors> {
        Some(self.ledger.priors(scope))
    }
}
