//! JS 选项：字段名和 Firecrawl JS SDK 的 `scrapeOptions` 一致，转换成 pagesift 的 [`api::types::ScrapeOptions`]。

use std::collections::HashMap;
use std::time::Duration;

use napi_derive::napi;
use pagesift::api::ApiError;
use pagesift::api::types::{self as api, Format};
use serde_json::Value;

/// `Scraper` 的构造参数。
#[napi(object)]
#[derive(Default)]
pub struct ScraperConfig {
    /// 请求头里的 User-Agent。
    pub user_agent: Option<String>,
    /// 允许抓取内网、回环地址（默认拒绝，防 SSRF）。
    pub allow_private_network: Option<bool>,
    /// 同一主机两次请求之间的最小间隔，毫秒，默认 1000；robots.txt 的 Crawl-delay 优先。
    pub request_interval_ms: Option<u32>,
    /// 同一主机同时进行的请求数，默认 2。
    pub per_host_concurrency: Option<u32>,
    /// 浏览器渲染时最多启动的浏览器进程数。
    pub max_browsers: Option<u32>,
}

impl ScraperConfig {
    pub fn into_core(self) -> pagesift::ScraperConfig {
        let mut c = pagesift::ScraperConfig::default();
        if let Some(ua) = self.user_agent {
            c.fetcher.user_agent = ua;
        }
        if let Some(allow) = self.allow_private_network {
            c.fetcher.ssrf.allow_private_network = allow;
        }
        if let Some(ms) = self.request_interval_ms {
            c.policy.rate.default_interval = Duration::from_millis(ms.into());
        }
        if let Some(n) = self.per_host_concurrency {
            c.policy.rate.per_host_concurrency = n.max(1) as usize;
        }
        if let Some(n) = self.max_browsers {
            c.browser.max_browsers = n.max(1) as usize;
        }
        c
    }
}

const FORMATS: &[&str] = &["markdown", "html", "rawHtml", "links", "images"];

/// `ScrapeOptions` 和 `ScrapeManyOptions` 共用同一组字段（napi 的对象类型不支持继承）。
macro_rules! scrape_options {
    ($(#[$meta:meta])* $name:ident { $($extra:tt)* }) => {
        $(#[$meta])*
        #[napi(object)]
        #[derive(Default)]
        pub struct $name {
            $($extra)*
            /// 输出哪些内容，默认 `['markdown']`。
            #[napi(ts_type = "Array<Format | { type: Format }>")]
            pub formats: Option<Vec<Value>>,
            /// 只保留正文，去掉导航、页眉页脚、侧边栏，默认 true。
            pub only_main_content: Option<bool>,
            /// `auto`（默认，正文太少时改用浏览器渲染）、`http` 或 `browser`。
            #[napi(ts_type = "'auto' | 'http' | 'browser'")]
            pub engine: Option<String>,
            /// 总超时，毫秒，至少 1000。
            pub timeout: Option<u32>,
            /// 浏览器渲染时页面加载后再等待多久，毫秒。
            pub wait_for: Option<u32>,
            /// 额外的请求头。
            pub headers: Option<HashMap<String, String>>,
            /// 只保留匹配这些选择器的元素（暂不支持，传非空数组会报 UNSUPPORTED_OPTION）。
            pub include_tags: Option<Vec<String>>,
            /// 去掉匹配这些选择器的元素（暂不支持，传非空数组会报 UNSUPPORTED_OPTION）。
            pub exclude_tags: Option<Vec<String>>,
            /// 这一页不检查 robots.txt（只对单页抓取有效）。
            pub ignore_robots_txt: Option<bool>,
            /// 浏览器渲染时模拟手机。
            pub mobile: Option<bool>,
            /// 浏览器渲染时屏蔽广告，默认 true。
            pub block_ads: Option<bool>,
            /// 条件请求：`If-None-Match`。
            pub if_none_match: Option<String>,
            /// 条件请求：`If-Modified-Since`（HTTP 日期）。
            pub if_modified_since: Option<String>,
        }

        impl $name {
            pub fn to_api(&self) -> Result<api::ScrapeOptions, ApiError> {
                let mut o = api::ScrapeOptions::default();
                if let Some(f) = &self.formats {
                    o.formats = f
                        .iter()
                        .map(|v| serde_json::from_value::<Format>(v.clone()))
                        .collect::<Result<_, _>>()
                        .map_err(|e| ApiError::invalid(format!("formats: {e}")))?;
                }
                if let Some(v) = self.only_main_content {
                    o.only_main_content = v;
                }
                o.engine = self.engine.clone();
                o.timeout = self.timeout.map(u64::from);
                o.wait_for = self.wait_for.map(u64::from).unwrap_or(0);
                if let Some(h) = &self.headers {
                    o.headers = h.iter().map(|(k, v)| (k.clone(), v.clone())).collect();
                }
                o.include_tags = self.include_tags.clone().unwrap_or_default();
                o.exclude_tags = self.exclude_tags.clone().unwrap_or_default();
                o.ignore_robots_txt = self.ignore_robots_txt.unwrap_or(false);
                o.mobile = self.mobile.unwrap_or(false);
                if let Some(v) = self.block_ads {
                    o.block_ads = v;
                }
                o.if_none_match = self.if_none_match.clone();
                o.if_modified_since = self.if_modified_since.clone();
                validate(&o)?;
                Ok(o)
            }
        }
    };
}

// 宏参数里的文档用 `#[doc = "..."]`：写成 `///` 会以原始字符串传进来，生成的 d.ts 里多出 `r"`。
scrape_options!(
    #[doc = "单页抓取选项，字段和 Firecrawl JS SDK 的 `scrapeOptions` 一致。"]
    ScrapeOptions {}
);

scrape_options!(
    #[doc = "`scrapeMany` 的选项：`ScrapeOptions` 的全部字段，加上并发数。"]
    ScrapeManyOptions {
        #[doc = "同时进行的抓取数，默认 8；同一主机仍受限速约束。"]
        pub concurrency: Option<u32>,
    }
);

/// 和 HTTP 接口的校验一致：做不到的事明确报错，不悄悄降级。
fn validate(o: &api::ScrapeOptions) -> Result<(), ApiError> {
    if let Some(e) = o.engine.as_deref()
        && !matches!(e, "http" | "browser" | "auto")
    {
        return Err(ApiError::invalid(format!(
            "engine must be http, browser or auto, got {e:?}"
        )));
    }
    if let Some(f) = o
        .formats
        .iter()
        .find(|f| !FORMATS.contains(&f.kind.as_str()))
    {
        return Err(ApiError::unsupported(format!(
            "format {:?} is not supported (supported: {})",
            f.kind,
            FORMATS.join(", ")
        )));
    }
    if !o.include_tags.is_empty() || !o.exclude_tags.is_empty() {
        return Err(ApiError::unsupported(
            "includeTags and excludeTags are not supported yet",
        ));
    }
    if let Some(t) = o.timeout {
        if t < 1000 {
            return Err(ApiError::invalid("timeout must be at least 1000 ms"));
        }
        if o.wait_for > t / 2 {
            return Err(ApiError::invalid("waitFor must not exceed half of timeout"));
        }
    }
    if o.wait_for > 60_000 {
        return Err(ApiError::invalid("waitFor must not exceed 60000 ms"));
    }
    Ok(())
}

pub fn check_url(raw: &str) -> Result<(), ApiError> {
    match url::Url::parse(raw) {
        Ok(u) if matches!(u.scheme(), "http" | "https") && u.host().is_some() => Ok(()),
        Ok(_) => Err(ApiError::invalid(format!(
            "only http(s) URLs are supported: {raw}"
        ))),
        Err(e) => Err(ApiError::invalid(format!("invalid URL {raw:?}: {e}"))),
    }
}
