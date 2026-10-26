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

#[path = "config_server.rs"]
mod server;
pub use server::*;

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
    #[serde(default)]
    pub audit: Audit,
    pub llm: Option<Llm>,
    pub search: Option<Search>,
    /// 实际加载的配置文件；内置默认值时为 `None`。
    #[serde(skip)]
    source: Option<PathBuf>,
    /// `PAGESIFT_CONFIG=` 显式关闭了所有配置文件。
    #[serde(skip)]
    disabled: bool,
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

/// 审计日志（见 [`crate::audit`]）。设了 `path` 才开启；命令行 `--audit-log` 优先。
#[derive(Debug, Default, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Audit {
    pub path: Option<PathBuf>,
    /// 单个日志文件上限，超过就轮转成 `<path>.1`（默认 64）。
    pub max_mb: Option<u64>,
    /// 结果可疑时是否保存 HTML 快照（默认开）。
    pub snapshots: Option<bool>,
    /// 快照目录（默认日志旁边的 `snapshots/`）。
    pub snapshot_dir: Option<PathBuf>,
    /// 单个快照上限（KB，默认 2048）。
    pub snapshot_max_kb: Option<usize>,
    /// 每天最多存几个快照（默认 200）。
    pub snapshots_per_day: Option<u32>,
    /// URL 里的凭证是否脱敏（默认是）。
    pub redact: Option<bool>,
    /// 运行方式，由命令行填写，写进每条记录。
    #[serde(skip)]
    pub source: Option<String>,
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
    /// 单独放行的 CIDR 网段（如 10.0.0.0/8, 198.18.0.0/15）。
    #[serde(default)]
    pub allow_cidrs: Vec<String>,
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

fn config_base(
    xdg: Option<&Path>,
    home: Option<&Path>,
    appdata: Option<&Path>,
    userprofile: Option<&Path>,
    windows: bool,
) -> Option<PathBuf> {
    if let Some(base) = xdg.filter(|path| path.is_absolute()) {
        return Some(base.to_path_buf());
    }
    if windows {
        if let Some(base) = appdata.filter(|path| path.is_absolute()) {
            return Some(base.to_path_buf());
        }
        return userprofile
            .filter(|path| path.is_absolute())
            .map(|profile| profile.join(".config"));
    }
    home.filter(|path| path.is_absolute())
        .map(|home| home.join(".config"))
}

impl Config {
    /// 按 `--config`、`PAGESIFT_CONFIG`、平台默认路径的顺序找配置文件。
    ///
    /// `PAGESIFT_CONFIG=`（空值）表示不读任何配置文件，用于 Docker/CI/测试强制默认值。
    /// 默认路径只在文件存在时加载；显式指定的路径不存在仍然报错。
    pub fn load(explicit: Option<&Path>) -> Result<Self> {
        Self::load_resolved(
            explicit,
            std::env::var_os("PAGESIFT_CONFIG"),
            Self::default_path(),
        )
    }

    /// 可注入的加载入口：单元测试不改进程环境。
    fn load_resolved(
        explicit: Option<&Path>,
        env_config: Option<std::ffi::OsString>,
        default_path: Option<PathBuf>,
    ) -> Result<Self> {
        let path = match explicit {
            Some(path) => Some(path.to_path_buf()),
            None => match env_config {
                Some(value) if value.is_empty() => {
                    return Ok(Self {
                        disabled: true,
                        ..Self::default()
                    });
                }
                Some(value) => Some(PathBuf::from(value)),
                None => default_path.filter(|path| path.is_file()),
            },
        };
        let Some(path) = path else {
            return Ok(Self::default());
        };
        let text = std::fs::read_to_string(&path)
            .with_context(|| format!("reading {}", path.display()))?;
        let mut config: Self =
            toml::from_str(&text).with_context(|| format!("parsing {}", path.display()))?;
        config.source = Some(path);
        Ok(config)
    }

    /// 平台默认配置路径：
    ///
    /// - `XDG_CONFIG_HOME` 为绝对路径时：`$XDG_CONFIG_HOME/pagesift/pagesift.toml`
    /// - Linux/macOS：`$HOME/.config/pagesift/pagesift.toml`
    /// - Windows：`%APPDATA%\pagesift\pagesift.toml`，回退
    ///   `%USERPROFILE%\.config\pagesift\pagesift.toml`
    ///
    /// 空值或相对路径一律当作未设置，避免意外读取工作目录下的文件。
    pub fn default_path() -> Option<PathBuf> {
        let xdg = std::env::var_os("XDG_CONFIG_HOME").map(PathBuf::from);
        let home = std::env::var_os("HOME").map(PathBuf::from);
        let appdata = std::env::var_os("APPDATA").map(PathBuf::from);
        let userprofile = std::env::var_os("USERPROFILE").map(PathBuf::from);
        config_base(
            xdg.as_deref(),
            home.as_deref(),
            appdata.as_deref(),
            userprofile.as_deref(),
            cfg!(windows),
        )
        .map(|base| base.join("pagesift").join("pagesift.toml"))
    }

    /// 实际加载的配置文件；没有配置文件时为 `None`。
    pub fn source(&self) -> Option<&Path> {
        self.source.as_deref()
    }

    /// 是否被 `PAGESIFT_CONFIG=` 显式关闭了配置文件加载。
    pub fn config_files_disabled(&self) -> bool {
        self.disabled
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

    /// 配置了审计日志时打开它。
    pub fn open_audit(&self) -> Result<Option<std::sync::Arc<crate::audit::Audit>>> {
        let a = &self.audit;
        let Some(path) = &a.path else {
            return Ok(None);
        };
        let mut ac = crate::audit::AuditConfig::new(path);
        if let Some(mb) = a.max_mb {
            ac.max_bytes = mb.max(1) * 1024 * 1024;
        }
        if a.snapshots == Some(false) {
            ac.snapshot_dir = None;
        } else if let Some(d) = &a.snapshot_dir {
            ac.snapshot_dir = Some(d.clone());
        }
        if let Some(kb) = a.snapshot_max_kb {
            ac.snapshot_max_bytes = kb * 1024;
        }
        if let Some(n) = a.snapshots_per_day {
            ac.snapshots_per_day = n;
        }
        if let Some(r) = a.redact {
            ac.redact = r;
        }
        if let Some(s) = &a.source {
            ac.source = s.clone();
        }
        let audit = crate::audit::Audit::open(ac)
            .with_context(|| format!("opening the audit log {}", path.display()))?;
        Ok(Some(std::sync::Arc::new(audit)))
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
        let mut c = ScraperConfig {
            audit: self.open_audit()?,
            ..Default::default()
        };
        let f = &self.fetch;
        if let Some(ua) = &f.user_agent {
            pagesift_net::validate_user_agent(ua)
                .map_err(|e| anyhow::anyhow!("fetch.user_agent: {e}"))?;
            c.fetcher.user_agent = ua.clone();
        }
        if let Some(t) = f.timeout_secs {
            // `timeout_secs` 是**单次请求的总预算**。以前只写 total_timeout，而等响应头用的是
            // 独立的 first_byte_timeout（默认 30 s），于是配置成 8 s 的请求仍可能等 30 s。
            // 这里让所有阶段都不超过总预算，并标记为显式设置，供自适应调优尊重。
            let budget = Duration::from_secs(t);
            c.fetcher.total_timeout = budget;
            c.fetcher.first_byte_timeout = c.fetcher.first_byte_timeout.min(budget);
            c.fetcher.body_idle_timeout = c.fetcher.body_idle_timeout.min(budget);
            c.fetcher.connect_timeout = c.fetcher.connect_timeout.min(budget);
            c.fetcher.dns_timeout = c.fetcher.dns_timeout.min(budget);
            c.fetcher.tls_timeout = c.fetcher.tls_timeout.min(budget);
            c.fetcher.tail_threshold = c
                .fetcher
                .tail_threshold
                .min((budget / 4).max(Duration::from_millis(500)));
            c.fetcher.timeouts_explicit = true;
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
        for cidr in &f.allow_cidrs {
            match cidr.parse() {
                Ok(parsed) => c.fetcher.ssrf.allow_cidrs.push(parsed),
                Err(e) => bail!("invalid CIDR {cidr:?} in fetch.allow_cidrs: {e}"),
            }
        }
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
#[path = "config_tests.rs"]
mod tests;
