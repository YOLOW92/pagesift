//! 请求校验：绕过类参数和引擎做不到的功能一律明确报错（PRD 6.5.1），不悄悄降级。

use crate::engine::Engine;
use crate::error::ApiError;
use crate::types::{PdfParser, ScrapeOptions};

/// URL 最长 8 KB：再长的 URL 浏览器和多数服务器都不接受，只会白白占内存。
pub const MAX_URL_LEN: usize = 8 * 1024;
/// 同步 scrape 的 `timeout` 上限（毫秒）；整请求超时按它留余量。
pub const MAX_SCRAPE_TIMEOUT_MS: u64 = 300_000;
/// `waitFor` 上限（毫秒）。
pub const MAX_WAIT_FOR_MS: u64 = 60_000;
/// crawl 的 `limit` 上限。
pub const MAX_CRAWL_LIMIT: usize = 100_000;
/// crawl 的 `maxDiscoveryDepth` / `maxDepth` 上限。
pub const MAX_CRAWL_DEPTH: u64 = 100;
/// map 的 `limit` 上限，和 Firecrawl 文档一致。
pub const MAX_MAP_LIMIT: usize = 100_000;
/// batch scrape、extract 一次最多带多少个 URL。
pub const MAX_URLS: usize = 10_000;

pub fn check_url(raw: &str) -> Result<(), ApiError> {
    if raw.len() > MAX_URL_LEN {
        return Err(ApiError::invalid(format!(
            "URL is too long: {} bytes, the maximum is {MAX_URL_LEN}",
            raw.len()
        )));
    }
    match url::Url::parse(raw) {
        Ok(u) if matches!(u.scheme(), "http" | "https") && u.host().is_some() => Ok(()),
        Ok(_) => Err(ApiError::invalid(format!(
            "only http(s) URLs are supported: {raw}"
        ))),
        Err(e) => Err(ApiError::invalid(format!("invalid URL {raw:?}: {e}"))),
    }
}

pub fn check_scrape_options(opts: &ScrapeOptions, engine: &dyn Engine) -> Result<(), ApiError> {
    if opts.discover_scripts
        && (opts.engine.as_deref() != Some("http")
            || !opts.wants("links")
            || opts.egress.is_some()
            || !opts.headers.is_empty()
            || opts.if_none_match.is_some()
            || opts.if_modified_since.is_some())
    {
        return Err(ApiError::unsupported(
            "discoverScripts requires HTTP engine, links, and no custom headers/egress/conditional request",
        ));
    }
    if let Some(p) = opts.proxy.as_deref()
        && !matches!(p, "basic" | "auto")
    {
        let valid_url = url::Url::parse(p)
            .ok()
            .is_some_and(|u| matches!(u.scheme(), "http" | "https" | "socks5" | "socks5h"));
        if !valid_url {
            return Err(ApiError::unsupported(format!(
                "proxy {p:?} is not supported: use \"basic\", \"auto\", or a valid http/https/socks5 URL"
            )));
        }
    }
    if let Some(e) = opts.engine.as_deref()
        && !matches!(e, "http" | "browser" | "auto")
    {
        return Err(ApiError::invalid(format!(
            "engine must be http, browser or auto, got {e:?}"
        )));
    }
    if let Some(c) = &opts.chunking {
        const KINDS: [&str; 8] = [
            "recursive",
            "headings",
            "paragraphs",
            "sentences",
            "fixed_tokens",
            "sliding_window",
            "overlapping",
            "regex",
        ];
        let kind = c.get("type").and_then(|t| t.as_str());
        if !kind.is_some_and(|k| KINDS.contains(&k)) {
            return Err(ApiError::invalid(format!(
                "chunking.type must be one of {}",
                KINDS.join(", ")
            )));
        }
    }
    let supported = engine.supported_formats();
    if let Some(f) = opts
        .formats
        .iter()
        .find(|f| !supported.contains(&f.kind.as_str()))
    {
        return Err(ApiError::unsupported(format!(
            "format {:?} is not supported by this build (supported: {})",
            f.kind,
            supported.join(", ")
        )));
    }
    if !opts.actions.is_empty() && !engine.supports_actions() {
        return Err(ApiError::unsupported(
            "actions need browser rendering, which is not available in this build",
        ));
    }
    if let Some(t) = opts.timeout {
        if t < 1000 {
            return Err(ApiError::invalid("timeout must be at least 1000 ms"));
        }
        if t > MAX_SCRAPE_TIMEOUT_MS {
            return Err(ApiError::invalid(format!(
                "timeout must not exceed {MAX_SCRAPE_TIMEOUT_MS} ms"
            )));
        }
        if opts.wait_for > t / 2 {
            return Err(ApiError::invalid("waitFor must not exceed half of timeout"));
        }
    }
    if opts.wait_for > MAX_WAIT_FOR_MS {
        return Err(ApiError::invalid(format!(
            "waitFor must not exceed {MAX_WAIT_FOR_MS} ms"
        )));
    }
    // 抓到的是 PDF 时才用得上，但写错的 parsers 在抓取前就报出来（parse 也走这里）
    PdfParser::from_parsers(opts.parsers.as_ref())?;
    Ok(())
}

/// URL 数量的上限检查：batch scrape 和 extract 共用。
pub fn check_url_count(n: usize) -> Result<(), ApiError> {
    if n > MAX_URLS {
        return Err(ApiError::invalid(format!(
            "too many URLs: {n}, the maximum is {MAX_URLS}"
        )));
    }
    Ok(())
}

/// `/v2/parse` 上传文件的大小上限，和 Firecrawl 一致（50 MB）。
pub const MAX_PARSE_FILE_BYTES: usize = 50 * 1024 * 1024;

/// `/v2/parse` 的选项：先过和 scrape 相同的检查，再拒绝只对网址有意义的选项
/// （照 Firecrawl `getUnsupportedParseOptionError`，另加 pagesift 自己的抓取类扩展）。
pub fn check_parse_options(opts: &ScrapeOptions, engine: &dyn Engine) -> Result<(), ApiError> {
    check_scrape_options(opts, engine)?;
    for kind in ["screenshot", "branding", "changeTracking"] {
        if opts.wants(kind) {
            return Err(ApiError::unsupported(format!(
                "parse uploads do not support the {kind} format"
            )));
        }
    }
    let unsupported = [
        (!opts.actions.is_empty(), "actions"),
        (opts.wait_for > 0, "waitFor"),
        (opts.location.is_some(), "location"),
        (opts.mobile, "mobile"),
        (
            opts.engine.as_deref() == Some("browser"),
            "engine \"browser\"",
        ),
        (opts.egress.is_some(), "egress"),
        (opts.discover_scripts, "discoverScripts"),
        (
            opts.if_none_match.is_some() || opts.if_modified_since.is_some(),
            "conditional requests",
        ),
        (
            opts.proxy
                .as_deref()
                .is_some_and(|p| !matches!(p, "basic" | "auto")),
            "proxy other than basic or auto",
        ),
    ];
    if let Some((_, what)) = unsupported.iter().find(|(on, _)| *on) {
        return Err(ApiError::unsupported(format!(
            "parse uploads do not support {what}"
        )));
    }
    Ok(())
}
