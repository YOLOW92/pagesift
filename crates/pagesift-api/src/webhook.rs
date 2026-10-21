//! Firecrawl v2 的任务 webhook：`started`、`page`、`completed`、`failed` 四种事件。
//!
//! - 每个任务一个投递任务，事件按产生顺序逐个 POST，失败按指数退避重试，次数有上限；
//! - 投递不阻塞爬取：页面事件进有界队列，队列满了就丢弃并计数；开始和结束事件一定排上队；
//! - 配置了密钥时带 `X-Firecrawl-Signature: sha256=<HMAC-SHA256(密钥, 请求体)>`，和 Firecrawl 一样；
//! - 目标地址过 SSRF 校验：请求时校验 URL，投递时的 DNS 解析结果逐个校验，连接的正是校验过的地址，
//!   不跟随重定向，不走环境变量里的代理。

use std::collections::BTreeMap;
use std::net::SocketAddr;
use std::sync::Arc;
use std::time::Duration;

use pagesift_net::{DnsLookup, SsrfPolicy, SystemLookup};
use serde::{Deserialize, Serialize};
use serde_json::{Map, Value, json};
use tokio::sync::mpsc;

use crate::error::{ApiError, ErrorCode};
use crate::metrics::Metrics;
use crate::types::Document;

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
    /// 每个任务排队等待投递的事件数上限，满了丢弃页面事件。
    pub queue: usize,
    /// 签名密钥：设了就给每个请求加 `X-Firecrawl-Signature`，接收方用它验证来源。
    pub secret: Option<String>,
}

impl Default for WebhookSettings {
    fn default() -> Self {
        Self {
            ssrf: SsrfPolicy::default(),
            dns: Arc::new(SystemLookup),
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
}

/// 解析失败或地址被 SSRF 策略拒绝：投递时从错误链里认出来，不再重试。
#[derive(Debug)]
struct Blocked(String);

impl std::fmt::Display for Blocked {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "webhook address blocked: {}", self.0)
    }
}

impl std::error::Error for Blocked {}

/// 按 SSRF 策略校验 DNS 解析结果的解析器。
struct GuardedDns {
    dns: Arc<dyn DnsLookup>,
    policy: SsrfPolicy,
}

impl reqwest::dns::Resolve for GuardedDns {
    fn resolve(&self, name: reqwest::dns::Name) -> reqwest::dns::Resolving {
        let (dns, policy) = (self.dns.clone(), self.policy.clone());
        Box::pin(async move {
            let host = name.as_str().to_string();
            let ips = dns.lookup(&host).await?;
            let blocked = |why: String| -> Box<dyn std::error::Error + Send + Sync> {
                Box::new(Blocked(why))
            };
            if ips.is_empty() {
                return Err(blocked(format!("no address for {host}")));
            }
            for ip in &ips {
                policy.check_ip(*ip).map_err(|e| blocked(e.to_string()))?;
            }
            let addrs: reqwest::dns::Addrs =
                Box::new(ips.into_iter().map(|ip| SocketAddr::new(ip, 0)));
            Ok(addrs)
        })
    }
}

fn is_blocked(e: &reqwest::Error) -> bool {
    let mut src: Option<&(dyn std::error::Error + 'static)> = Some(e);
    while let Some(s) = src {
        if s.is::<Blocked>() {
            return true;
        }
        src = s.source();
    }
    false
}

impl Webhooks {
    pub(crate) fn new(settings: WebhookSettings, metrics: Arc<Metrics>) -> Self {
        let client = reqwest::Client::builder()
            .no_proxy()
            .redirect(reqwest::redirect::Policy::none())
            .dns_resolver(Arc::new(GuardedDns {
                dns: settings.dns.clone(),
                policy: settings.ssrf.clone(),
            }))
            .user_agent(pagesift_net::DEFAULT_USER_AGENT)
            .timeout(settings.timeout)
            .build()
            .expect("webhook client builds with static settings");
        Self {
            client,
            settings: Arc::new(settings),
            metrics,
        }
    }

    pub(crate) fn ssrf(&self) -> &SsrfPolicy {
        &self.settings.ssrf
    }

    /// 为一个任务启动投递任务。`kind` 是事件类型前缀：`crawl` 或 `batch_scrape`。
    pub(crate) fn start(&self, job_id: &str, kind: &'static str, cfg: WebhookConfig) -> Hook {
        let (tx, mut rx) = mpsc::channel::<Value>(self.settings.queue.max(1));
        let this = self.clone();
        let cfg = Arc::new(cfg);
        let worker_cfg = cfg.clone();
        tokio::spawn(async move {
            while let Some(body) = rx.recv().await {
                this.deliver(&worker_cfg, &body).await;
            }
        });
        Hook {
            tx,
            cfg,
            kind,
            job_id: job_id.into(),
            metrics: self.metrics.clone(),
        }
    }

    /// 投递一个事件，失败时按指数退避重试。
    async fn deliver(&self, cfg: &WebhookConfig, body: &Value) {
        let s = &self.settings;
        // IP 字面量不经过解析器，这里再按当前策略校验一次（任务可能是重启前按旧配置收下的）
        if let Err(e) = s.ssrf.check_url(&cfg.url) {
            self.metrics.webhook("blocked");
            tracing::warn!(url = %cfg.url, error = %e, "webhook target blocked by the SSRF policy");
            return;
        }
        let payload = body.to_string();
        let signature = s.secret.as_deref().map(|k| sign(k, payload.as_bytes()));
        let mut backoff = s.initial_backoff;
        for attempt in 1..=s.max_attempts.max(1) {
            let mut req = self
                .client
                .post(&cfg.url)
                .header(reqwest::header::CONTENT_TYPE, "application/json")
                .body(payload.clone());
            if let Some(sig) = &signature {
                req = req.header("x-firecrawl-signature", sig);
            }
            for (k, v) in &cfg.headers {
                req = req.header(k, v);
            }
            let (retry, why) = match req.send().await {
                Ok(r) if r.status().is_success() => {
                    self.metrics.webhook("delivered");
                    return;
                }
                Ok(r) => {
                    let st = r.status();
                    // 超时、限流和服务端错误可以重试；其他 4xx 和重定向重试也没用
                    let retry = st.is_server_error()
                        || st == reqwest::StatusCode::TOO_MANY_REQUESTS
                        || st == reqwest::StatusCode::REQUEST_TIMEOUT;
                    (retry, format!("status {st}"))
                }
                Err(e) if is_blocked(&e) => {
                    self.metrics.webhook("blocked");
                    tracing::warn!(url = %cfg.url, error = %e, "webhook target blocked by the SSRF policy");
                    return;
                }
                Err(e) => (true, e.to_string()),
            };
            if !retry || attempt == s.max_attempts {
                self.metrics.webhook("failed");
                tracing::warn!(url = %cfg.url, attempt, reason = %why, "webhook delivery failed");
                return;
            }
            self.metrics.webhook("retried");
            tracing::debug!(url = %cfg.url, attempt, reason = %why, ?backoff, "retrying webhook");
            tokio::time::sleep(backoff).await;
            backoff = (backoff * 2).min(s.max_backoff);
        }
    }
}

/// `sha256=` 加上请求体的 HMAC-SHA256（小写十六进制）。
fn sign(secret: &str, payload: &[u8]) -> String {
    let key = ring::hmac::Key::new(ring::hmac::HMAC_SHA256, secret.as_bytes());
    let tag = ring::hmac::sign(&key, payload);
    let mut out = String::from("sha256=");
    for b in tag.as_ref() {
        out.push_str(&format!("{b:02x}"));
    }
    out
}

/// 一个任务的 webhook 句柄。全部句柄释放后，投递任务发完排队的事件就退出。
#[derive(Clone)]
pub(crate) struct Hook {
    tx: mpsc::Sender<Value>,
    cfg: Arc<WebhookConfig>,
    kind: &'static str,
    job_id: Arc<str>,
    metrics: Arc<Metrics>,
}

impl Hook {
    pub(crate) fn send(&self, event: Event, data: Vec<Document>, error: Option<String>) {
        if !self.cfg.wants(event) {
            return;
        }
        let mut body = json!({
            "success": error.is_none(),
            "type": format!("{}.{}", self.kind, event.name()),
            "id": &*self.job_id,
            "data": data,
            "metadata": self.cfg.metadata,
        });
        if let Some(e) = error {
            body["error"] = e.into();
        }
        match self.tx.try_send(body) {
            Ok(()) => {}
            Err(mpsc::error::TrySendError::Full(body)) if event != Event::Page => {
                // 开始和结束事件不能丢：另起一个任务等队列空出来，爬取照样不等
                let tx = self.tx.clone();
                tokio::spawn(async move {
                    let _ = tx.send(body).await;
                });
            }
            Err(_) => {
                self.metrics.webhook("dropped");
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn signatures_match_rfc_4231() {
        // RFC 4231 测试用例 2
        assert_eq!(
            sign("Jefe", b"what do ya want for nothing?"),
            "sha256=5bdcc146bf60754e6a042426089575c75a003f089d2739839dec58b964ec3843"
        );
    }

    #[test]
    fn parses_strings_objects_and_rejects_private_targets() {
        let policy = SsrfPolicy::default();
        let c = WebhookConfig::parse(&json!("https://hooks.example.com/x"), &policy).unwrap();
        assert_eq!(c.events, all_events());
        let c = WebhookConfig::parse(
            &json!({"url": "https://hooks.example.com/x", "headers": {"X-Token": "t"},
                    "metadata": {"run": "1"}, "events": ["completed"], "future": true}),
            &policy,
        )
        .unwrap();
        assert!(c.wants(Event::Completed) && !c.wants(Event::Page));
        for (v, code) in [
            (json!("http://127.0.0.1:9000/hook"), ErrorCode::SsrfBlocked),
            (
                json!("http://169.254.169.254/latest"),
                ErrorCode::SsrfBlocked,
            ),
            (json!("http://metadata.internal/"), ErrorCode::SsrfBlocked),
            (json!("file:///etc/passwd"), ErrorCode::SsrfBlocked),
            (
                json!({"url": "https://a.example", "events": ["done"]}),
                ErrorCode::InvalidRequest,
            ),
            (json!(42), ErrorCode::InvalidRequest),
        ] {
            assert_eq!(
                WebhookConfig::parse(&v, &policy).unwrap_err().code,
                code,
                "{v}"
            );
        }
    }
}
