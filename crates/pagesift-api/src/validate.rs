//! 请求校验：绕过类参数和引擎做不到的功能一律明确报错（PRD 6.5.1），不悄悄降级。

use crate::engine::Engine;
use crate::error::ApiError;
use crate::types::ScrapeOptions;

pub fn check_url(raw: &str) -> Result<(), ApiError> {
    match url::Url::parse(raw) {
        Ok(u) if matches!(u.scheme(), "http" | "https") && u.host().is_some() => Ok(()),
        Ok(_) => Err(ApiError::invalid(format!(
            "only http(s) URLs are supported: {raw}"
        ))),
        Err(e) => Err(ApiError::invalid(format!("invalid URL {raw:?}: {e}"))),
    }
}

pub fn check_scrape_options(opts: &ScrapeOptions, engine: &dyn Engine) -> Result<(), ApiError> {
    if let Some(p) = opts.proxy.as_deref()
        && !matches!(p, "basic" | "auto")
    {
        return Err(ApiError::unsupported(format!(
            "proxy {p:?} is not supported: pagesift does not bypass anti-bot protections; use \"basic\" or \"auto\""
        )));
    }
    if let Some(e) = opts.engine.as_deref()
        && !matches!(e, "http" | "browser" | "auto")
    {
        return Err(ApiError::invalid(format!(
            "engine must be http, browser or auto, got {e:?}"
        )));
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
        if opts.wait_for > t / 2 {
            return Err(ApiError::invalid("waitFor must not exceed half of timeout"));
        }
    }
    if opts.wait_for > 60_000 {
        return Err(ApiError::invalid("waitFor must not exceed 60000 ms"));
    }
    Ok(())
}
