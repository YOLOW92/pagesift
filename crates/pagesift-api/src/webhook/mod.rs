//! Firecrawl v2 的任务 webhook：`started`、`page`、`completed`、`failed` 四种事件。
//!
//! - 每个任务一个投递任务，事件按产生顺序逐个 POST，失败按指数退避重试，次数有上限；
//! - 每个事件带 `eventId`（`{任务 id}:{seq}`）和 `seq`（任务内从 1 起单调递增），接收方据此去重；
//! - 事件不丢：有存储时先落盘再投递，重启后从已确认游标续投；没有存储时进有界内存队列，满了背压
//!   （见 [`queue`]）；
//! - 配置了密钥时带 `X-Firecrawl-Signature: sha256=<HMAC-SHA256(密钥, 请求体)>`，和 Firecrawl 一样；
//! - 目标地址过 SSRF 校验：请求时校验 URL，投递时的 DNS 解析结果逐个校验，连接的正是校验过的地址，
//!   不跟随重定向，不走环境变量里的代理。

use std::collections::BTreeMap;
use std::sync::Arc;
use std::time::Duration;

use pagesift_net::{DnsLookup, SsrfPolicy, SystemLookup};
use serde::{Deserialize, Serialize};
use serde_json::{Map, Value};

use crate::error::{ApiError, ErrorCode};
use crate::metrics::Metrics;

mod deliver;
mod queue;
#[cfg(test)]
mod tests;

pub(crate) use queue::{Hook, ack_key, outbox_key, seq_key};

/// 请求里的 `webhook`：Firecrawl 接受一个 URL 字符串，或者带 headers、metadata、events 的对象。
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct WebhookConfig {
    pub url: String,
    #[serde(default)]
    pub headers: BTreeMap<String, String>,
    #[serde(default)]
    pub metadata: Map<String, Value>,
    #[serde(default = "all_events")]
    pub events: Vec<String>,
}

const EVENTS: [&str; 4] = ["started", "page", "completed", "failed"];

fn all_events() -> Vec<String> {
    EVENTS.iter().map(|e| e.to_string()).collect()
}

impl WebhookConfig {
    /// 解析并校验请求里的 `webhook` 字段（含 SSRF 校验）。未知字段忽略。
    pub(crate) fn parse(v: &Value, policy: &SsrfPolicy) -> Result<Self, ApiError> {
        let cfg: Self = match v {
            Value::String(u) => Self {
                url: u.clone(),
                headers: BTreeMap::new(),
                metadata: Map::new(),
                events: all_events(),
            },
            Value::Object(_) => serde_json::from_value(v.clone())
                .map_err(|e| ApiError::invalid(format!("invalid webhook: {e}")))?,
            _ => {
                return Err(ApiError::invalid(
                    "webhook must be a URL or an object with a url",
                ));
            }
        };
        if let Some(e) = cfg.events.iter().find(|e| !EVENTS.contains(&e.as_str())) {
            return Err(ApiError::invalid(format!(
                "unknown webhook event {e:?}: use started, page, completed or failed"
            )));
        }
        for (k, v) in &cfg.headers {
            if reqwest::header::HeaderName::from_bytes(k.as_bytes()).is_err()
                || reqwest::header::HeaderValue::from_str(v).is_err()
            {
                return Err(ApiError::invalid(format!("invalid webhook header {k:?}")));
            }
        }
        policy.check_url(&cfg.url).map_err(|e| {
            ApiError::new(
                ErrorCode::SsrfBlocked,
                format!("webhook URL is not allowed: {e}"),
            )
        })?;
        Ok(cfg)
    }

    fn wants(&self, event: Event) -> bool {
        self.events.iter().any(|e| e == event.name())
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum Event {
    Started,
    Page,
    Completed,
    Failed,
}

impl Event {
    fn name(self) -> &'static str {
        match self {
            Event::Started => "started",
            Event::Page => "page",
            Event::Completed => "completed",
            Event::Failed => "failed",
        }
    }
}

/// webhook 投递的设置（服务端配置，不来自请求）。
#[derive(Clone)]
pub struct WebhookSettings {
    /// 目标地址的 SSRF 策略，默认拒绝一切非公网地址。
    pub ssrf: SsrfPolicy,
    pub dns: Arc<dyn DnsLookup>,
    /// 每个事件最多投递几次（含第一次）。
    pub max_attempts: u32,
    /// 第一次重试前等多久，之后每次翻倍，不超过 `max_backoff`。
    pub initial_backoff: Duration,
    pub max_backoff: Duration,
    /// 单次投递的超时。
    pub timeout: Duration,
    /// 没有存储时每个任务排队等待投递的事件数上限，满了发送方等待（背压），不丢事件。
    /// 有存储时事件排在盘上，不受这个限制。
    pub queue: usize,
    /// 签名密钥：设了就给每个请求加 `X-Firecrawl-Signature`，接收方用它验证来源。
    pub secret: Option<String>,
}

impl Default for WebhookSettings {
    fn default() -> Self {
        Self {
            ssrf: SsrfPolicy::default(),
            dns: Arc::new(SystemLookup),
            // [策略] 下面都是 `[server.webhook]` 可配的运维策略，不做在线估计：接收端是运维方自己的服务，
            // 每个任务只有几次投递样本，生存估计收敛不了；事件已经落盘不丢，这几项只影响送达快慢。
            // 默认值：指数退避 1 s 起、每次翻倍、封顶 60 s，5 次覆盖约 1+2+4+8+16 = 31 s 的短暂故障
            // （AWS Architecture Blog, "Exponential Backoff and Jitter", 2015 的做法）；
            // 单次超时 10 s 与 Firecrawl 的 webhook 超时一致；内存队列 1024 只在存储出错时使用，
            // 按每个事件约 1 KiB 计，单个任务最多占 1 MiB。
            max_attempts: 5,
            initial_backoff: Duration::from_secs(1),
            max_backoff: Duration::from_secs(60),
            timeout: Duration::from_secs(10),
            queue: 1024,
            secret: None,
        }
    }
}

impl std::fmt::Debug for WebhookSettings {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("WebhookSettings")
            .field("ssrf", &self.ssrf)
            .field("max_attempts", &self.max_attempts)
            .field("initial_backoff", &self.initial_backoff)
            .field("max_backoff", &self.max_backoff)
            .field("timeout", &self.timeout)
            .field("queue", &self.queue)
            .field("secret", &self.secret.is_some())
            .finish_non_exhaustive()
    }
}

/// 所有任务共用的投递器（HTTP 客户端和设置）。
#[derive(Clone)]
pub(crate) struct Webhooks {
    client: reqwest::Client,
    settings: Arc<WebhookSettings>,
    metrics: Arc<Metrics>,
    /// 多服务器网关：落盘队列的写入同时复制到温备（与任务记录同一个镜像）。
    pub(crate) mirror: Option<Arc<crate::jobs::mirror::JobMirror>>,
}

impl Webhooks {
    pub(crate) fn new(settings: WebhookSettings, metrics: Arc<Metrics>) -> Self {
        Self {
            client: deliver::client(&settings),
            settings: Arc::new(settings),
            metrics,
            mirror: None,
        }
    }

    pub(crate) fn ssrf(&self) -> &SsrfPolicy {
        &self.settings.ssrf
    }
}
