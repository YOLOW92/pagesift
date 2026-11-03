//! pagesift 的礼貌爬取策略：robots.txt（RFC 9309）、按主机限速、自适应退避，以及统一错误码。
//!
//! [`Policy`] 把这些串起来：冷却检查 → robots → 限速 → 抓取（每一跳重定向再查 robots）→ 记录主机健康状况。
//! 被拒绝时如实返回错误码，不会自动重试。

#![forbid(unsafe_code)]

mod backoff;
mod concurrency;
pub mod egress;
mod error;
pub mod gate;
mod limiter;
mod outage;
mod policy;
mod robots;
pub mod slots;
pub mod topology;

pub use backoff::{Backoff, BackoffConfig, parse_retry_after};
pub use concurrency::wait_price;
pub use egress::{
    Action as EgressAction, EgressClass, EgressPool, Outcome as EgressOutcome, Pick as EgressPick,
    Site,
};
pub use error::{Error, ErrorCode};
pub use gate::{AdaptiveConcurrencyGate, AdaptiveConcurrencyPermit};
pub use limiter::{Permit, RateLimitConfig, RateLimiter};
pub use policy::{Policy, PolicyConfig, RequestKind, RobotsCache, RobotsMode, robots_mode};
pub use robots::{MAX_ROBOTS_BYTES, Robots, RobotsMatcher};
pub use slots::{Slot, Slots};
