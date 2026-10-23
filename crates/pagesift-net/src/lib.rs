//! pagesift 的抓取层：带 SSRF 防护的 HTTP 客户端。
//!
//! - DNS 解析结果逐个校验，只要有一个非公网地址就整体拒绝；
//! - 重定向手动处理，每一跳重新做 SSRF 校验，并回调 [`RedirectGuard`]；
//! - 响应大小上限、字符集识别、条件请求、按主机下发的授权凭证。
//!
//! 已知限制：走 HTTP 代理时目标域名由代理解析，只能校验 URL 本身（协议、IP 字面量、内部域名）。

#![forbid(unsafe_code)]

pub mod budget;
mod charset;
pub mod checkpoint;
mod client;
mod credentials;
mod dns;
mod fetch;
pub mod media;
pub mod scheduler;
mod session;
mod ssrf;
mod ua;

pub use budget::{
    BackpressureController, BudgetStatus, DEFAULT_MAX_BYTES, DEFAULT_WARN_BYTES, LeakDetector,
    MemGuard,
};
pub use checkpoint::{RecoveryEngine, TaskSnapshot};
pub use media::{
    ChunkFetcher, DashAdaptationSet, DashError, DashManifest, DashRepresentation, DashSegment,
    HlsMasterPlaylist, HlsMediaPlaylist, HlsPlaylist, HlsSegment, HlsVariant, M3u8Error,
    StreamVerifier, VerificationReport, parse_m3u8, parse_mpd,
};
pub use scheduler::{CrawlResult, CrawlTask, DomainLimiter, PushError, RingQueue, WorkerPool};

pub use charset::{CharsetSource, Decoded, decode};
pub use credentials::{HostCredentials, Secret, redact_url};
pub use dns::{BoxFuture, DnsLookup, SystemLookup};
pub use fetch::{
    BoxError, Error, FetchRequest, FetchResponse, FetchStream, Fetcher, FetcherConfig,
    RedirectGuard, RedirectHop, Result, TimeoutPhase, Timings,
};
pub use reqwest::header::HeaderMap;
pub use session::{CookieSession, SessionConfig, SessionError, SessionSnapshot, SessionStats};
pub use ssrf::{Blocked, IpCidr, SsrfPolicy, check_url, is_public};
pub use ua::{DEFAULT_USER_AGENT, UserAgentError, product_token, validate_user_agent};
pub use url::Url;
