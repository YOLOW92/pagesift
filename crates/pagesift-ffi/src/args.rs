//! 入参 JSON → 引擎配置和抓取选项。键名和取值规则与 Python 层一致（snake_case）；不认识的键报错，不悄悄忽略。

use std::collections::BTreeMap;
use std::path::PathBuf;
use std::sync::Arc;
use std::time::Duration;

use pagesift_cdp::BrowserSource;
use pagesift_core::ScraperConfig;
use pagesift_core::api::ApiError;
use pagesift_core::api::types::{Format, Location, PdfParser, ScrapeOptions};
use serde::Deserialize;
use serde::de::DeserializeOwned;
use serde_json::{Map, Value};

/// JSON 文本（可以没有）→ 对象。
pub fn object(text: Option<&str>) -> Result<Map<String, Value>, ApiError> {
    let Some(text) = text else {
        return Ok(Map::new());
    };
    match serde_json::from_str(text) {
        Ok(Value::Object(m)) => Ok(m),
        Ok(Value::Null) => Ok(Map::new()),
        Ok(_) => Err(ApiError::invalid("options must be a JSON object")),
        Err(e) => Err(ApiError::invalid(format!("options: {e}"))),
    }
}

/// 把 `keys` 列出的键从 `from` 里拿出来，解析成 `T`；其余的键留给下一步。
pub fn take<T: DeserializeOwned>(
    from: &mut Map<String, Value>,
    keys: &[&str],
) -> Result<T, ApiError> {
    let mut picked = Map::new();
    for k in keys {
        if let Some(v) = from.remove(*k) {
            picked.insert((*k).to_string(), v);
        }
    }
    parse(picked)
}

pub fn parse<T: DeserializeOwned>(m: Map<String, Value>) -> Result<T, ApiError> {
    serde_json::from_value(Value::Object(m)).map_err(|e| ApiError::invalid(format!("options: {e}")))
}

fn default_true() -> bool {
    true
}

/// 抓取选项，对应 Python 的 `ScrapeOptions`。
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct ScrapeArgs {
    formats: Option<Vec<String>>,
    #[serde(default = "default_true")]
    only_main_content: bool,
    engine: Option<String>,
    timeout: Option<u64>,
    #[serde(default)]
    wait_for: u64,
    headers: Option<BTreeMap<String, String>>,
    include_tags: Option<Vec<String>>,
    exclude_tags: Option<Vec<String>>,
    #[serde(default)]
    ignore_robots_txt: bool,
    #[serde(default)]
    mobile: bool,
    languages: Option<Vec<String>>,
    proxy: Option<String>,
    if_none_match: Option<String>,
    if_modified_since: Option<String>,
    #[serde(default)]
    export_images: bool,
    extract_schema: Option<Value>,
    max_age: Option<u64>,
    parsers: Option<Value>,
}

/// `pagesift::Scraper` 支持的格式（和它的 `Engine::supported_formats` 一致）。
const FORMATS: &[&str] = &["markdown", "html", "rawHtml", "links", "images"];

/// 剩下的键都当抓取选项解析并校验（和 Python 的 `Options` 同样的规则）。
pub fn scrape_options(m: Map<String, Value>) -> Result<Arc<ScrapeOptions>, ApiError> {
    let a: ScrapeArgs = parse(m)?;
    let mut o = ScrapeOptions::default();
    if let Some(f) = a.formats {
        o.formats = f
            .iter()
            .map(|k| Format::new(if k == "raw_html" { "rawHtml" } else { k }))
            .collect();
    }
    o.only_main_content = a.only_main_content;
    o.engine = Some(a.engine.unwrap_or_else(|| "auto".into()));
    o.timeout = a.timeout;
    o.wait_for = a.wait_for;
    o.headers = a.headers.unwrap_or_default();
    o.include_tags = a.include_tags.unwrap_or_default();
    o.exclude_tags = a.exclude_tags.unwrap_or_default();
    o.ignore_robots_txt = a.ignore_robots_txt;
    o.mobile = a.mobile;
    o.proxy = a.proxy;
    o.if_none_match = a.if_none_match;
    o.if_modified_since = a.if_modified_since;
    o.export_images = a.export_images;
    o.max_age = a.max_age;
    if let Some(parsers) = a.parsers {
        o.parsers = Some(
            serde_json::from_value(parsers)
                .map_err(|e| ApiError::invalid(format!("parsers: {e}")))?,
        );
    }
    if let Some(schema) = a.extract_schema {
        let mut f = Format::new("extract");
        f.options.insert("schema".into(), schema);
        o.formats.retain(|f| f.kind != "extract");
        o.formats.push(f);
    }
    if let Some(l) = a.languages {
        o.location = Some(Location {
            country: None,
            languages: l,
        });
    }
    validate(&o)?;
    Ok(Arc::new(o))
}

fn validate(o: &ScrapeOptions) -> Result<(), ApiError> {
    PdfParser::from_parsers(o.parsers.as_ref())?;
    if let Some(f) = o
        .formats
        .iter()
        .find(|f| !FORMATS.contains(&f.kind.as_str()) && f.kind != "extract")
    {
        return Err(ApiError::unsupported(format!(
            "format {:?} is not supported (supported: markdown, html, raw_html, links, images)",
            f.kind
        )));
    }
    if let Some(e) = o.engine.as_deref()
        && !matches!(e, "http" | "browser" | "auto")
    {
        return Err(ApiError::invalid(format!(
            "engine must be http, browser or auto, got {e:?}"
        )));
    }
    pagesift_content::TagFilter::new(&o.include_tags, &o.exclude_tags)
        .map_err(ApiError::invalid)?;
    if let Some(t) = o.timeout {
        if t < 1000 {
            return Err(ApiError::invalid(
                "timeout is in milliseconds and must be at least 1000",
            ));
        }
        if o.wait_for > t / 2 {
            return Err(ApiError::invalid(
                "wait_for must not exceed half of timeout",
            ));
        }
    }
    if o.wait_for > 60_000 {
        return Err(ApiError::invalid("wait_for must not exceed 60000 ms"));
    }
    Ok(())
}

/// 引擎配置，对应 Python `Scraper(...)` 的参数，默认值也一样。
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct EngineArgs {
    user_agent: Option<String>,
    #[serde(default)]
    allow_private_network: bool,
    allow_cidrs: Option<Vec<String>>,
    proxy: Option<String>,
    request_interval_ms: Option<u64>,
    per_host_concurrency: Option<usize>,
    max_concurrency: Option<usize>,
    browser_executable: Option<PathBuf>,
    browser_url: Option<String>,
    max_browsers: Option<usize>,
    store_path: Option<PathBuf>,
    #[serde(default)]
    ocr: bool,
    ocr_quality: Option<String>,
    #[serde(default)]
    ocr_formulas: bool,
    models_dir: Option<PathBuf>,
    gpu: Option<String>,
}

pub fn engine_config(m: Map<String, Value>) -> Result<ScraperConfig, ApiError> {
    let a: EngineArgs = parse(m)?;
    let per_host_concurrency = a.per_host_concurrency.unwrap_or(2);
    let max_concurrency = a.max_concurrency.unwrap_or(64);
    let max_browsers = a.max_browsers.unwrap_or(2);
    if per_host_concurrency == 0 || max_concurrency == 0 || max_browsers == 0 {
        return Err(ApiError::invalid("concurrency limits must be at least 1"));
    }
    let mut cfg = ScraperConfig::default();
    if let Some(ua) = a.user_agent {
        cfg.fetcher.user_agent = ua;
    }
    cfg.fetcher.proxy = a.proxy;
    cfg.fetcher.ssrf.allow_private_network = a.allow_private_network;
    for c in a.allow_cidrs.unwrap_or_default() {
        let parsed = c
            .parse()
            .map_err(|e| ApiError::invalid(format!("invalid CIDR {c:?}: {e}")))?;
        cfg.fetcher.ssrf.allow_cidrs.push(parsed);
    }
    cfg.policy.rate.default_interval = Duration::from_millis(a.request_interval_ms.unwrap_or(1000));
    cfg.policy.rate.per_host_concurrency = per_host_concurrency;
    // `max_concurrency` 是用户给的上限：在线调整只在它之下
    cfg.policy.rate.global_concurrency = max_concurrency;
    cfg.policy.rate.global_ceiling = Some(max_concurrency);
    cfg.browser.max_browsers = max_browsers;
    if let Some(dir) = a.store_path {
        let store = pagesift_core::store::Store::open_dir(&dir).map_err(|e| {
            ApiError::invalid(format!("opening the store in {}: {e}", dir.display()))
        })?;
        cfg.store = Some(store);
    }
    cfg.ocr = pagesift_core::config::OcrOptions {
        enabled: a.ocr,
        quality: a.ocr_quality,
        formulas: a.ocr_formulas,
        models_dir: a.models_dir,
        gpu: a.gpu,
    }
    .engine(&cfg.fetcher)
    .map_err(|e| ApiError::invalid(format!("{e:#}")))?;
    // 用库的人就是管理员（和命令行、Python 一样）：crawl、map 的 ignore_robots_txt 由调用方决定
    cfg.policy.allow_crawl_ignore_robots = true;
    cfg.browser.source = match (a.browser_executable, a.browser_url) {
        (Some(_), Some(_)) => {
            return Err(ApiError::invalid(
                "pass either browser_executable or browser_url, not both",
            ));
        }
        (Some(p), None) => BrowserSource::Executable(p),
        (None, Some(u)) => BrowserSource::Remote(u),
        (None, None) => BrowserSource::Auto,
    };
    Ok(cfg)
}
