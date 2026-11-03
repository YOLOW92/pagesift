//! 单个 webhook 事件的投递：SSRF 守卫的 HTTP 客户端、指数退避重试、HMAC 签名。

use std::net::SocketAddr;
use std::sync::Arc;

use pagesift_net::{DnsLookup, SsrfPolicy};
use serde_json::Value;

use super::{WebhookConfig, WebhookSettings, Webhooks};

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

/// 投递用的 HTTP 客户端：解析结果过 SSRF 校验，不跟随重定向，不走代理。
pub(super) fn client(settings: &WebhookSettings) -> reqwest::Client {
    reqwest::Client::builder()
        .no_proxy()
        .redirect(reqwest::redirect::Policy::none())
        .dns_resolver(Arc::new(GuardedDns {
            dns: settings.dns.clone(),
            policy: settings.ssrf.clone(),
        }))
        .user_agent(pagesift_net::DEFAULT_USER_AGENT)
        .timeout(settings.timeout)
        .build()
        .expect("webhook client builds with static settings")
}

impl Webhooks {
    /// 投递一个事件，失败时按指数退避重试。返回时事件已有结果（送达、重试用尽或地址被拦），
    /// 调用方据此推进已确认游标。送达返回 `None`，没送达返回原因（调用方记成死信）。
    pub(super) async fn deliver(&self, cfg: &WebhookConfig, body: &Value) -> Option<String> {
        let s = &self.settings;
        // IP 字面量不经过解析器，这里再按当前策略校验一次（任务可能是重启前按旧配置收下的）
        if let Err(e) = s.ssrf.check_url(&cfg.url) {
            self.metrics.webhook("blocked");
            tracing::warn!(url = %cfg.url, error = %e, "webhook target blocked by the SSRF policy");
            return Some(format!("target blocked: {e}"));
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
                    return None;
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
                    return Some(format!("target blocked: {e}"));
                }
                Err(e) => (true, e.to_string()),
            };
            if !retry || attempt == s.max_attempts {
                self.metrics.webhook("failed");
                tracing::warn!(url = %cfg.url, attempt, reason = %why, "webhook delivery failed");
                return Some(format!("{why} after {attempt} attempts"));
            }
            self.metrics.webhook("retried");
            tracing::debug!(url = %cfg.url, attempt, reason = %why, ?backoff, "retrying webhook");
            tokio::time::sleep(backoff).await;
            backoff = (backoff * 2).min(s.max_backoff);
        }
        Some("no delivery attempt".into())
    }
}

/// `sha256=` 加上请求体的 HMAC-SHA256（小写十六进制）。
pub(super) fn sign(secret: &str, payload: &[u8]) -> String {
    let key = ring::hmac::Key::new(ring::hmac::HMAC_SHA256, secret.as_bytes());
    let tag = ring::hmac::sign(&key, payload);
    let mut out = String::from("sha256=");
    for b in tag.as_ref() {
        out.push_str(&format!("{b:02x}"));
    }
    out
}
