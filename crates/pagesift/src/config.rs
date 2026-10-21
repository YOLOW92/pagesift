//! 配置（PRD 6.11）：`pagesift.toml`，路径由 `--config` 或 `PAGESIFT_CONFIG` 指定。
//! 优先级：命令行参数 > 环境变量 > 配置文件 > 默认值。
//!
//! 密钥类字段（API key、凭证）既可以直接写值，也可以写成 `{ env = "VAR" }` 从环境变量读，
//! 这样配置文件里不必出现密钥本身。

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::Duration;

use anyhow::{Context, Result, bail};
use pagesift_net::HostCredentials;
use serde::Deserialize;

use crate::ScraperConfig;

#[derive(Debug, Default, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Config {
    #[serde(default)]
    pub server: Server,
    #[serde(default)]
    pub fetch: Fetch,
    #[serde(default)]
    pub policy: Policy,
    #[serde(default)]
    pub browser: Browser,
    #[serde(default)]
    pub cache: Cache,
    #[serde(default)]
    pub credentials: Vec<Credential>,
    /// 命名出口：请求用 `egress` 指定走哪个。
    #[serde(default)]
    pub egress: Vec<Egress>,
    #[serde(default)]
    pub log: Log,
    pub llm: Option<Llm>,
    pub search: Option<Search>,
}

/// 直接写值，或者 `{ env = "VAR" }`。
#[derive(Debug, Clone, Deserialize)]
#[serde(untagged)]
pub enum SecretValue {
    Plain(String),
    Env { env: String },
}

impl SecretValue {
    pub fn resolve(&self) -> Result<String> {
        match self {
            Self::Plain(v) => Ok(v.clone()),
            Self::Env { env } => {
                std::env::var(env).with_context(|| format!("environment variable {env} is not set"))
            }
        }
    }
}

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
    /// 每个事件最多投递几次（默认 5）。
    pub max_attempts: Option<u32>,
    pub timeout_secs: Option<u64>,
    /// 签名密钥：每个 webhook 请求带 `X-Firecrawl-Signature: sha256=<HMAC>`。
    pub secret: Option<SecretValue>,
}

#[derive(Debug, Default, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Log {
    /// `text`（默认）或 `json`；环境变量 `PAGESIFT_LOG_FORMAT` 优先。级别用 `PAGESIFT_LOG`。
    pub format: Option<String>,
}

#[derive(Debug, Default, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Fetch {
    /// 自定义 UA 必须表明是机器人，冒充浏览器的 UA 会被拒绝。
    pub user_agent: Option<String>,
    pub timeout_secs: Option<u64>,
    pub max_body_mb: Option<usize>,
    pub max_redirects: Option<usize>,
    /// 企业网络的出口代理（不是代理池）。
    pub proxy: Option<String>,
    #[serde(default)]
    pub allow_private_network: bool,
    /// 单独放行的主机名（完全匹配），解析到内网地址也可以抓，例如内部服务。
    #[serde(default)]
    pub allow_hosts: Vec<String>,
}

/// 一个命名出口：走指定的代理（`http://`、`https://` 或 `socks5://`）。
#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Egress {
    pub name: String,
    pub proxy: String,
}

#[derive(Debug, Default, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Policy {
    /// 同一主机两次请求之间的最小间隔，robots.txt 的 Crawl-delay 优先。
    pub interval_ms: Option<u64>,
    pub per_host_concurrency: Option<usize>,
    pub global_concurrency: Option<usize>,
    /// 管理员开关：允许 crawl 和 map 请求忽略 robots.txt（F-2.4）。
    #[serde(default)]
    pub allow_crawl_ignore_robots: bool,
}

#[derive(Debug, Default, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Browser {
    /// Chrome、Chromium 或兼容 CDP 的浏览器的可执行文件。
    pub path: Option<PathBuf>,
    /// `path` 的内核种类：`lightpanda`、`moli` 或 `chrome`；不写时按文件名判断。
    pub engine: Option<String>,
    /// 连接已经在运行的浏览器：`ws://…` 或 `http://host:port`。
    pub remote: Option<String>,
    pub max_browsers: Option<usize>,
    pub max_tabs_per_browser: Option<usize>,
    /// 开新标签页前系统至少要剩多少可用内存（MB），0 表示不检查。
    pub min_available_memory_mb: Option<u64>,
    #[serde(default)]
    pub allow_execute_javascript: bool,
}

#[derive(Debug, Default, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Cache {
    pub enabled: Option<bool>,
    pub dir: Option<PathBuf>,
    pub max_mb: Option<u64>,
}

/// 和站点谈好的授权凭证（F-1.11），只发给 `host`。
#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Credential {
    pub host: String,
    #[serde(default)]
    pub headers: BTreeMap<String, SecretValue>,
    #[serde(default)]
    pub cookies: BTreeMap<String, SecretValue>,
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Llm {
    pub base_url: String,
    pub model: String,
    pub api_key: Option<SecretValue>,
    pub timeout_secs: Option<u64>,
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Search {
    /// searxng、brave、bing、google、tavily
    pub backend: String,
    pub endpoint: Option<String>,
    pub api_key: Option<SecretValue>,
    /// Google Programmable Search 的搜索引擎 id（cx）。
    pub engine_id: Option<String>,
}

impl Config {
    /// 按 `--config`、`PAGESIFT_CONFIG` 的顺序找配置文件；都没有时用默认配置。
    pub fn load(explicit: Option<&Path>) -> Result<Self> {
        let path = explicit
            .map(Path::to_path_buf)
            .or_else(|| std::env::var_os("PAGESIFT_CONFIG").map(PathBuf::from));
        let Some(path) = path else {
            return Ok(Self::default());
        };
        let text = std::fs::read_to_string(&path)
            .with_context(|| format!("reading {}", path.display()))?;
        toml::from_str(&text).with_context(|| format!("parsing {}", path.display()))
    }

    pub fn data_dir(&self) -> PathBuf {
        if let Some(d) = std::env::var_os("PAGESIFT_DATA_DIR") {
            return PathBuf::from(d);
        }
        if let Some(d) = &self.cache.dir {
            return d.clone();
        }
        let base = std::env::var_os("XDG_DATA_HOME")
            .map(PathBuf::from)
            .or_else(|| std::env::var_os("HOME").map(|h| PathBuf::from(h).join(".local/share")))
            .unwrap_or_else(|| PathBuf::from("."));
        base.join("pagesift")
    }

    /// 日志是否输出 JSON：`PAGESIFT_LOG_FORMAT` 优先，其次 `[log] format`。
    pub fn log_json(&self) -> Result<bool> {
        let format = std::env::var("PAGESIFT_LOG_FORMAT")
            .ok()
            .or_else(|| self.log.format.clone());
        match format.as_deref() {
            None | Some("text") => Ok(false),
            Some("json") => Ok(true),
            Some(other) => bail!("log format must be text or json, not {other:?}"),
        }
    }

    /// 打开数据目录里的存储（缓存和任务共用）。
    pub fn open_store(&self) -> Result<pagesift_store::Store> {
        let dir = self.data_dir().join("store");
        std::fs::create_dir_all(&dir).with_context(|| format!("creating {}", dir.display()))?;
        let mut sc = pagesift_store::Config::new(&dir);
        if let Some(mb) = self.cache.max_mb {
            sc.max_bytes = Some(mb * 1024 * 1024);
        }
        pagesift_store::Store::open(sc)
            .with_context(|| format!("opening the store in {}", dir.display()))
    }

    /// HTTP 服务的配置：key、限额、webhook。存储和命令行参数由调用方补上。
    pub fn api(&self) -> Result<pagesift_api::Config> {
        let s = &self.server;
        let mut c = pagesift_api::Config {
            api_keys: self.api_keys()?,
            public_url: s.public_url.clone(),
            max_batch_concurrency: s.max_batch_concurrency,
            default_limits: s.limits.to_api(None),
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

    pub fn api_keys(&self) -> Result<Vec<String>> {
        let mut keys: Vec<String> = self
            .server
            .api_keys
            .iter()
            .map(SecretValue::resolve)
            .collect::<Result<_>>()?;
        if let Ok(env) = std::env::var("PAGESIFT_API_KEYS") {
            keys.extend(
                env.split(',')
                    .map(str::trim)
                    .filter(|k| !k.is_empty())
                    .map(String::from),
            );
        }
        Ok(keys)
    }

    /// 组装引擎配置；`with_cache` 为 true 且配置没有关掉缓存时打开存储。
    pub fn scraper(&self, with_cache: bool) -> Result<ScraperConfig> {
        let mut c = ScraperConfig::default();
        let f = &self.fetch;
        if let Some(ua) = &f.user_agent {
            pagesift_net::validate_user_agent(ua)
                .map_err(|e| anyhow::anyhow!("fetch.user_agent: {e}"))?;
            c.fetcher.user_agent = ua.clone();
        }
        if let Some(t) = f.timeout_secs {
            c.fetcher.total_timeout = Duration::from_secs(t);
        }
        if let Some(mb) = f.max_body_mb {
            c.fetcher.max_body_bytes = mb * 1024 * 1024;
        }
        if let Some(n) = f.max_redirects {
            c.fetcher.max_redirects = n;
        }
        c.fetcher.proxy = f.proxy.clone();
        c.fetcher.ssrf.allow_private_network = f.allow_private_network;
        c.fetcher.ssrf.allow_hosts = f.allow_hosts.clone();
        for e in &self.egress {
            if e.name.is_empty() || c.egress.iter().any(|(n, _)| *n == e.name) {
                bail!("egress names must be unique and non-empty: {:?}", e.name);
            }
            c.egress.push((e.name.clone(), e.proxy.clone()));
        }
        for cred in &self.credentials {
            let mut hc = HostCredentials::new(cred.host.clone());
            for (k, v) in &cred.headers {
                hc = hc.header(
                    k.clone(),
                    v.resolve()
                        .with_context(|| format!("credentials for {}", cred.host))?,
                );
            }
            for (k, v) in &cred.cookies {
                hc = hc.cookie(
                    k.clone(),
                    v.resolve()
                        .with_context(|| format!("credentials for {}", cred.host))?,
                );
            }
            c.fetcher.credentials.push(hc);
        }

        let p = &self.policy;
        if let Some(ms) = p.interval_ms {
            c.policy.rate.default_interval = Duration::from_millis(ms);
        }
        if let Some(n) = p.per_host_concurrency {
            c.policy.rate.per_host_concurrency = n;
        }
        if let Some(n) = p.global_concurrency {
            c.policy.rate.global_concurrency = n;
        }
        c.policy.allow_crawl_ignore_robots = p.allow_crawl_ignore_robots;

        let b = &self.browser;
        if b.path.is_some() && b.remote.is_some() {
            bail!("browser.path and browser.remote are mutually exclusive");
        }
        if let Some(path) = &b.path {
            c.browser.source = pagesift_cdp::BrowserSource::Executable(path.clone());
        }
        if let Some(e) = &b.engine {
            c.browser.engine = Some(pagesift_cdp::EngineKind::parse(e).with_context(|| {
                format!("browser.engine must be lightpanda, moli or chrome, not {e:?}")
            })?);
        }
        if let Some(remote) = &b.remote {
            c.browser.source = pagesift_cdp::BrowserSource::Remote(remote.clone());
        }
        if let Some(n) = b.max_browsers {
            c.browser.max_browsers = n;
        }
        if let Some(n) = b.max_tabs_per_browser {
            c.browser.max_tabs_per_browser = n;
        }
        if let Some(n) = b.min_available_memory_mb {
            c.browser.min_available_memory_mb = n;
        }
        c.browser.allow_execute_javascript = b.allow_execute_javascript;

        if with_cache && self.cache.enabled.unwrap_or(true) {
            c.store = Some(self.open_store()?);
        }

        if let Some(l) = &self.llm {
            let key = match &l.api_key {
                Some(k) => Some(k.resolve().context("llm.api_key")?),
                None => std::env::var("PAGESIFT_LLM_API_KEY").ok(),
            };
            let mut lc = pagesift_llm::LlmConfig::new(l.base_url.clone(), l.model.clone(), key);
            if let Some(t) = l.timeout_secs {
                lc.timeout = Duration::from_secs(t);
            }
            c.llm = Some(lc);
        }
        if let Some(s) = &self.search {
            c.search = Some(self.search_backend(s)?);
        }
        Ok(c)
    }

    fn search_backend(&self, s: &Search) -> Result<Arc<dyn pagesift_search::SearchBackend>> {
        use pagesift_search::*;
        let opts = HttpOptions::default();
        let key = match &s.api_key {
            Some(k) => Some(k.resolve().context("search.api_key")?),
            None => std::env::var("PAGESIFT_SEARCH_API_KEY").ok(),
        };
        let need_key = || {
            key.clone()
                .with_context(|| format!("search backend {} needs api_key", s.backend))
        };
        let backend: Arc<dyn SearchBackend> = match s.backend.as_str() {
            "searxng" => {
                let endpoint = s
                    .endpoint
                    .clone()
                    .context("search backend searxng needs endpoint")?;
                Arc::new(SearxngBackend::new(endpoint, key.clone(), opts)?)
            }
            "brave" => Arc::new(match &s.endpoint {
                Some(e) => BraveBackend::with_endpoint(e.clone(), need_key()?, opts)?,
                None => BraveBackend::new(need_key()?, opts)?,
            }),
            "bing" => Arc::new(match &s.endpoint {
                Some(e) => BingBackend::with_endpoint(e.clone(), need_key()?, opts)?,
                None => BingBackend::new(need_key()?, opts)?,
            }),
            "google" => {
                let cx = s
                    .engine_id
                    .clone()
                    .context("search backend google needs engine_id")?;
                Arc::new(GoogleCseBackend::new(need_key()?, cx, opts)?)
            }
            "tavily" => Arc::new(match &s.endpoint {
                Some(e) => TavilyBackend::with_endpoint(e.clone(), need_key()?, opts)?,
                None => TavilyBackend::new(need_key()?, opts)?,
            }),
            other => bail!(
                "unknown search backend {other:?}: use searxng, brave, bing, google or tavily"
            ),
        };
        Ok(backend)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// `api()` 会解析 `api_keys` 里引用的环境变量；测试里只看 keys、限额和 webhook。
    fn cfg_without_env_keys(cfg: &Config) -> pagesift_api::Config {
        let mut c = Config::default();
        c.server.limits = Limits {
            requests_per_minute: cfg.server.limits.requests_per_minute,
            max_concurrent_jobs: cfg.server.limits.max_concurrent_jobs,
            pages_per_day: cfg.server.limits.pages_per_day,
        };
        c.server.keys = cfg
            .server
            .keys
            .iter()
            .map(|k| ApiKey {
                key: k.key.clone(),
                name: k.name.clone(),
                requests_per_minute: k.requests_per_minute,
                max_concurrent_jobs: k.max_concurrent_jobs,
                pages_per_day: k.pages_per_day,
            })
            .collect();
        c.server.webhook.max_attempts = cfg.server.webhook.max_attempts;
        c.api().unwrap()
    }

    #[test]
    fn parses_a_full_config_and_rejects_browser_like_user_agents() {
        let cfg: Config = toml::from_str(
            r#"
            [server]
            port = 4000
            api_keys = ["k1", { env = "PAGESIFT_TEST_KEY_UNSET" }]

            [server.limits]
            requests_per_minute = 600

            [[server.keys]]
            key = "team-a-key"
            name = "team-a"
            requests_per_minute = 60
            max_concurrent_jobs = 2
            pages_per_day = 5000

            [server.webhook]
            max_attempts = 3

            [log]
            format = "json"

            [fetch]
            user_agent = "acme-bot/1.0 (+https://acme.example/bot)"
            timeout_secs = 20
            allow_hosts = ["docs.internal"]

            [[egress]]
            name = "tunnel-a"
            proxy = "socks5://127.0.0.1:1081"

            [policy]
            interval_ms = 500
            allow_crawl_ignore_robots = true

            [browser]
            max_browsers = 1

            [cache]
            enabled = false

            [[credentials]]
            host = "partner.example"
            headers = { Authorization = "Bearer abc" }
            cookies = { session = { env = "HOME" } }

            [llm]
            base_url = "http://localhost:11434/v1"
            model = "qwen3"
            "#,
        )
        .unwrap();
        assert_eq!(cfg.server.port, Some(4000));
        assert!(cfg.api_keys().is_err(), "unset env secret is an error");
        let k = &cfg.server.keys[0];
        assert_eq!(k.name.as_deref(), Some("team-a"));
        assert_eq!(k.pages_per_day, Some(5000));
        let api = cfg_without_env_keys(&cfg);
        assert_eq!(api.key_limits["team-a-key"].max_concurrent_jobs, Some(2));
        assert_eq!(api.default_limits.requests_per_minute, Some(600));
        assert_eq!(api.webhook.max_attempts, 3);
        assert!(!api.webhook.ssrf.allow_private_network);
        assert_eq!(cfg.server.limits.requests_per_minute, Some(600));
        assert_eq!(cfg.server.webhook.max_attempts, Some(3));
        if std::env::var_os("PAGESIFT_LOG_FORMAT").is_none() {
            assert!(cfg.log_json().unwrap());
        }
        let sc = cfg.scraper(true).unwrap();
        assert!(sc.store.is_none(), "cache disabled");
        assert_eq!(sc.fetcher.credentials.len(), 1);
        assert_eq!(sc.policy.rate.default_interval, Duration::from_millis(500));
        assert!(sc.policy.allow_crawl_ignore_robots);
        assert_eq!(sc.llm.as_ref().unwrap().model, "qwen3");
        assert_eq!(sc.fetcher.ssrf.allow_hosts, ["docs.internal"]);
        assert_eq!(
            sc.egress,
            [(
                "tunnel-a".to_string(),
                "socks5://127.0.0.1:1081".to_string()
            )]
        );
        let dup: Config = toml::from_str(
            "[[egress]]\nname = \"a\"\nproxy = \"socks5://h:1\"\n[[egress]]\nname = \"a\"\nproxy = \"socks5://h:2\"",
        )
        .unwrap();
        assert!(dup.scraper(false).is_err(), "duplicate egress names");

        let bad: Config =
            toml::from_str("[fetch]\nuser_agent = \"Mozilla/5.0 (Windows NT 10.0)\"").unwrap();
        assert!(bad.scraper(false).is_err());
        assert!(
            toml::from_str::<Config>("[fetch]\nstealth = true").is_err(),
            "unknown keys are rejected"
        );
    }

    #[test]
    fn example_config_parses() {
        let cfg: Config = toml::from_str(include_str!("../../../pagesift.example.toml")).unwrap();
        assert_eq!(cfg.server.persist_jobs, Some(true));
    }
}
