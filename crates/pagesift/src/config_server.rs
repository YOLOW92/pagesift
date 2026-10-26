//! `[server]` 配置段：监听地址、API key 与限额、webhook，以及组装 HTTP 服务的配置。

use std::time::Duration;

use anyhow::{Context, Result, bail};
use serde::Deserialize;

use super::{Config, SecretValue};

#[derive(Debug, Default, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Server {
    pub host: Option<String>,
    pub port: Option<u16>,
    #[serde(default)]
    pub api_keys: Vec<SecretValue>,
    pub public_url: Option<String>,
    pub max_batch_concurrency: Option<usize>,
    /// 爬取和批量抓取任务存进数据目录，重启后恢复（默认开）。
    pub persist_jobs: Option<bool>,
    /// 单独设限额的 API key（也都是有效的 key）。
    #[serde(default)]
    pub keys: Vec<ApiKey>,
    /// 其余 key 的限额；不鉴权时所有调用方合用。不写就是不限。
    #[serde(default)]
    pub limits: Limits,
    #[serde(default)]
    pub webhook: Webhook,
    /// 服务端时限，防慢速连接和挂死的请求。不写的项用默认值。
    #[serde(default)]
    pub timeouts: ServerTimeouts,
}

/// `[server.timeouts]`：都是秒数，必须大于 0。
#[derive(Debug, Default, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ServerTimeouts {
    /// 读完请求头（HTTP/1），默认 30。
    pub header_read_secs: Option<u64>,
    /// 读完请求体（从请求进来算起的总时间），默认 30。
    pub body_secs: Option<u64>,
    /// `/v2/parse` 上传文件的请求体，默认 120（50 MB 要求至少约 0.42 MB/s）。
    pub upload_body_secs: Option<u64>,
    /// 整个请求到响应头发出，默认 360（同步 scrape 的 `timeout` 上限 300 秒再加 60 秒）。
    pub request_secs: Option<u64>,
}

impl ServerTimeouts {
    fn to_api(&self) -> Result<pagesift_api::Timeouts> {
        let mut t = pagesift_api::Timeouts::default();
        for (name, value, slot) in [
            (
                "header_read_secs",
                self.header_read_secs,
                &mut t.header_read,
            ),
            ("body_secs", self.body_secs, &mut t.body),
            (
                "upload_body_secs",
                self.upload_body_secs,
                &mut t.upload_body,
            ),
            ("request_secs", self.request_secs, &mut t.request),
        ] {
            match value {
                Some(0) => bail!("server.timeouts.{name} must be greater than 0"),
                Some(secs) => *slot = Duration::from_secs(secs),
                None => {}
            }
        }
        Ok(t)
    }
}

/// 按 API key 的限额（不写的项不限）。
#[derive(Debug, Default, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Limits {
    pub requests_per_minute: Option<u32>,
    pub max_concurrent_jobs: Option<usize>,
    pub pages_per_day: Option<u64>,
}

impl Limits {
    fn to_api(&self, name: Option<String>) -> pagesift_api::KeyLimits {
        pagesift_api::KeyLimits {
            name,
            requests_per_minute: self.requests_per_minute,
            max_concurrent_jobs: self.max_concurrent_jobs,
            pages_per_day: self.pages_per_day,
        }
    }
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ApiKey {
    pub key: SecretValue,
    /// 日志、监控和配额记录里用的名字；不写时用 key 的指纹。
    pub name: Option<String>,
    pub requests_per_minute: Option<u32>,
    pub max_concurrent_jobs: Option<usize>,
    pub pages_per_day: Option<u64>,
}

#[derive(Debug, Default, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Webhook {
    /// 允许 webhook 发到内网地址（默认拒绝，和抓取的 SSRF 策略分开设）。
    #[serde(default)]
    pub allow_private_network: bool,
    #[serde(default)]
    pub allow_cidrs: Vec<String>,
    /// 每个事件最多投递几次（默认 5）。
    pub max_attempts: Option<u32>,
    pub timeout_secs: Option<u64>,
    /// 签名密钥：每个 webhook 请求带 `X-Firecrawl-Signature: sha256=<HMAC>`。
    pub secret: Option<SecretValue>,
}

impl Config {
    /// HTTP 服务的配置：key、限额、webhook。存储和命令行参数由调用方补上。
    pub fn api(&self) -> Result<pagesift_api::Config> {
        let s = &self.server;
        let mut c = pagesift_api::Config {
            api_keys: self.api_keys()?,
            public_url: s.public_url.clone(),
            max_batch_concurrency: s.max_batch_concurrency,
            default_limits: s.limits.to_api(None),
            timeouts: s.timeouts.to_api()?,
            ..Default::default()
        };
        for k in &s.keys {
            let key = k.key.resolve().context("server.keys")?;
            c.key_limits.insert(
                key,
                pagesift_api::KeyLimits {
                    name: k.name.clone(),
                    requests_per_minute: k.requests_per_minute,
                    max_concurrent_jobs: k.max_concurrent_jobs,
                    pages_per_day: k.pages_per_day,
                },
            );
        }
        c.webhook.ssrf.allow_private_network = s.webhook.allow_private_network;
        for cidr in &s.webhook.allow_cidrs {
            match cidr.parse() {
                Ok(parsed) => c.webhook.ssrf.allow_cidrs.push(parsed),
                Err(e) => bail!("invalid CIDR {cidr:?} in webhook.allow_cidrs: {e}"),
            }
        }
        if let Some(n) = s.webhook.max_attempts {
            c.webhook.max_attempts = n.max(1);
        }
        if let Some(t) = s.webhook.timeout_secs {
            c.webhook.timeout = Duration::from_secs(t);
        }
        if let Some(k) = &s.webhook.secret {
            c.webhook.secret = Some(k.resolve().context("server.webhook.secret")?);
        }
        Ok(c)
    }
}
