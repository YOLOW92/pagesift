#![forbid(unsafe_code)]
//! 单主机与多服务器部署的协调层。
//!
//! 业务代码只看见 [`Coordinator`]：
//! - [`LocalCoordinator`]：单主机（`--role all`，默认），进程内队列，结果按所有权交付、不序列化；
//! - [`GatewayCoordinator`]：网关（`--role gateway`），账本逻辑与单主机共用 [`Ledger`]；
//! - [`RemoteCoordinator`]：工人（`--role worker`），经 HTTP/2 帧协议找网关；
//! - [`Standby`]：温备，日志传送 + 接管。
//!
//! 模块：
//! - [`ledger`]：任务、租约、栅栏（`term`+`epoch`）、幂等提交、取消、成员表与站点暂停；
//! - [`journal`]：账本的变更日志（存在 `pagesift-store` 的只追加列表里），恢复与温备回放共用；
//! - [`hrw`]：加权 rendezvous 哈希的站点亲和；
//! - [`stats`]：统计增量合并与先验下发；
//! - [`frame`]、[`transport`]：帧格式、HTTP/2（h2c / TLS / 双向 TLS）、令牌认证、版本协商。

pub mod clock;
mod codec;
pub mod coordinator;
pub mod frame;
pub mod gateway;
pub mod hrw;
pub mod journal;
pub mod ledger;
pub mod local;
pub mod remote;
pub mod standby;
pub mod stats;
pub mod transport;
pub mod types;

pub use clock::{Clock, ManualClock, MonoClock};
pub use coordinator::{BoxFuture, Completed, Coordinator, Deliver};
pub use gateway::{GatewayConfig, GatewayCoordinator};
pub use hrw::HostMap;
pub use journal::{Journal, Origin};
pub use ledger::{
    AccountLimits, AppState, Commit, Expired, Hold, Ledger, LedgerConfig, Refusal, Tick,
    renew_after,
};
pub use local::{LedgerFront, LocalCoordinator};
pub use remote::{RemoteCoordinator, SiteStates, WorkerConfig};
pub use standby::Standby;
pub use transport::auth::{TokenStore, token_hash};
pub use transport::client::Dial;
pub use types::{
    Deadline, Error, Fence, Lease, Owner, Priors, Renewal, Resource, Resources, Scope, SiteKey,
    SiteState, StatKind, StatsDelta, Task, TaskId, WorkerId,
};
