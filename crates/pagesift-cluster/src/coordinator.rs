//! `Coordinator`：单主机与多服务器的全部差别都收在它后面。
//!
//! 结果类型是泛型参数 `A`，不绑定具体的结果类型：单主机直接传所有权
//! （`LocalCoordinator<Artifact>`，零序列化）；多服务器经帧协议传已编码的字节，
//! 工人一侧要求 `A: Into<Vec<u8>>`，网关一侧拿到的是 `Vec<u8>`（编码由 `pagesift-sink` 的 Artifact 格式决定）。

use std::future::Future;
use std::pin::Pin;

use crate::types::{
    Error, Lease, Owner, Priors, Renewal, Resources, Scope, SiteKey, StatsDelta, Task, TaskId,
};

pub type BoxFuture<'a, T> = Pin<Box<dyn Future<Output = T> + Send + 'a>>;

pub trait Coordinator<A: Send + 'static>: Send + Sync {
    /// 提交任务（只在网关或单主机上有意义）。
    fn submit(&self, task: Task) -> BoxFuture<'_, Result<TaskId, Error>>;
    /// 准入器有余量时拉取一个任务；没有可做的立即返回 `None`。
    fn pull(&self, free: Resources) -> BoxFuture<'_, Option<Lease>>;
    /// 续约；可能带回 [`Renewal::Cancel`]。在 `lease.renew_after` 之后调用。
    fn renew<'a>(&'a self, lease: &'a Lease) -> BoxFuture<'a, Result<Renewal, Error>>;
    /// 提交结果。同一租约重复提交是幂等的（返回 `Ok`，结果不重复交付）；
    /// 令牌过期（任务已被重新分配、或来自旧任期）返回 [`Error::Fenced`]。
    fn commit<'a>(&'a self, lease: &'a Lease, result: A) -> BoxFuture<'a, Result<(), Error>>;
    /// 取消任务（网关改状态，持有者在下一次续约时得知）。
    fn cancel(&self, task: TaskId) -> BoxFuture<'_, Result<bool, Error>>;
    /// 站点的负责者（单主机总是 [`Owner::Local`]）。
    fn owner_of(&self, site: SiteKey) -> Owner;
    /// 发送统计增量（不等待）。`delta.seq` 由调用方按种类递增编号。
    fn publish_stats(&self, delta: StatsDelta);
    /// 已知的先验；多服务器下是本地缓存，没有时返回 `None` 并在后台向网关请求。
    fn priors(&self, scope: Scope) -> Option<Priors>;
}

/// 结果出口：账本接受一次提交后把结果交给它（任务记录、webhook、流式输出……）。
/// 实现必须按 `TaskId` 去重：单主机崩溃恢复后同一任务可能被交付第二次。
pub trait Deliver<A>: Send + Sync {
    fn deliver(&self, task: TaskId, result: A);
}

/// 完成的任务（[`Deliver`] 的通道实现送出的元素）。
#[derive(Debug)]
pub struct Completed<A> {
    pub task: TaskId,
    pub result: A,
}

impl<A: Send> Deliver<A> for tokio::sync::mpsc::UnboundedSender<Completed<A>> {
    fn deliver(&self, task: TaskId, result: A) {
        // 接收端已关闭说明上层在退出，结果无人要
        let _ = self.send(Completed { task, result });
    }
}
