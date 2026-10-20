//! JS 选项，转换成 pagesift 的 [`api::types::ScrapeOptions`] 和 [`pagesift_content::Options`]。

use std::collections::HashMap;
use std::time::Duration;

use napi_derive::napi;
use pagesift::api::ApiError;
use pagesift::api::types::{self as api, Format};

use crate::error::Fallible;

/// `Scraper` 的构造参数。
#[napi(object)]
#[derive(Default)]
pub struct ScraperConfig {
    /// 请求头里的 User-Agent。
    pub user_agent: Option<String>,
    /// 允许抓取内网、回环地址（默认拒绝，防 SSRF；只作用于 HTTP 抓取）。
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

fn parse_formats(formats: &Option<Vec<String>>) -> Fallible<Vec<Format>> {
    let Some(list) = formats else {
        return Ok(vec![Format::new("markdown")]);
    };
    if let Some(bad) = list.iter().find(|f| !FORMATS.contains(&f.as_str())) {
        return Err(ApiError::unsupported(format!(
            "format {bad:?} is not supported (supported: {})",
            FORMATS.join(", ")
        )));
    }
    Ok(list.iter().map(|f| Format::new(f)).collect())
}

/// `convert`、`convertAsync`、`convertMany` 的选项。
#[napi(object)]
#[derive(Default)]
pub struct ConvertOptions {
    /// 页面地址：把相对链接转成绝对地址，也写进 `metadata.url`。
    pub url: Option<String>,
    /// 只保留正文（去掉导航、页眉页脚、侧边栏等），默认 true。
    pub only_main_content: Option<bool>,
    /// 计算哪些输出，默认 `['markdown']`；只算需要的，越少越快。
    #[napi(ts_type = "Format[]")]
    pub formats: Option<Vec<String>>,
}

pub struct ConvertPlan {
    pub url: Option<String>,
    pub opts: pagesift_content::Options,
    pub markdown: bool,
    pub raw_html: bool,
}

impl ConvertOptions {
    pub fn plan(&self) -> Fallible<ConvertPlan> {
        let formats = parse_formats(&self.formats)?;
        let wants = |k: &str| formats.iter().any(|f| f.kind == k);
        let base_url = match &self.url {
            Some(u) => Some(
                url::Url::parse(u)
                    .map_err(|e| ApiError::invalid(format!("invalid url {u:?}: {e}")))?,
            ),
            None => None,
        };
        Ok(ConvertPlan {
            url: self.url.clone(),
            opts: pagesift_content::Options {
                base_url,
                only_main_content: self.only_main_content.unwrap_or(true),
                detect_barrier: false,
                clean_html: wants("html"),
                links: wants("links"),
                images: wants("images"),
                ..Default::default()
            },
            markdown: wants("markdown"),
            raw_html: wants("rawHtml"),
        })
    }
}

/// 单页和批量抓取共用同一组字段（napi 的对象类型不支持继承）。
macro_rules! scrape_options {
    ($(#[$meta:meta])* $name:ident { $($extra:tt)* }) => {
        $(#[$meta])*
        #[napi(object)]
        #[derive(Default)]
        pub struct $name {
            $($extra)*
            /// 计算哪些输出，默认 `['markdown']`；只算需要的，越少越快。
            #[napi(ts_type = "Format[]")]
            pub formats: Option<Vec<String>>,
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
            pub fn to_api(&self) -> Fallible<api::ScrapeOptions> {
                let mut o = api::ScrapeOptions {
                    formats: parse_formats(&self.formats)?,
                    engine: self.engine.clone(),
                    timeout: self.timeout.map(u64::from),
                    wait_for: self.wait_for.map(u64::from).unwrap_or(0),
                    ignore_robots_txt: self.ignore_robots_txt.unwrap_or(false),
                    mobile: self.mobile.unwrap_or(false),
                    if_none_match: self.if_none_match.clone(),
                    if_modified_since: self.if_modified_since.clone(),
                    ..Default::default()
                };
                if let Some(v) = self.only_main_content {
                    o.only_main_content = v;
                }
                if let Some(h) = &self.headers {
                    o.headers = h.iter().map(|(k, v)| (k.clone(), v.clone())).collect();
                }
                if let Some(v) = self.block_ads {
                    o.block_ads = v;
                }
                validate(&o)?;
                Ok(o)
            }
        }
    };
}

// 宏参数里的文档用 `#[doc = "..."]`：写成 `///` 会以原始字符串传进来，生成的 d.ts 里多出 `r"`。
scrape_options!(
    #[doc = "单页抓取选项。"]
    ScrapeOptions {}
);

scrape_options!(
    #[doc = "批量抓取选项：`ScrapeOptions` 的全部字段，加上并发数。"]
    BatchOptions {
        #[doc = "同时进行的抓取数，默认 16；同一主机仍受限速约束。"]
        pub concurrency: Option<u32>,
    }
);

/// 和 HTTP 接口的校验一致：做不到的事明确报错，不悄悄降级。
fn validate(o: &api::ScrapeOptions) -> Fallible<()> {
    if let Some(e) = o.engine.as_deref()
        && !matches!(e, "http" | "browser" | "auto")
    {
        return Err(ApiError::invalid(format!(
            "engine must be http, browser or auto, got {e:?}"
        )));
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

pub fn check_url(raw: &str) -> Fallible<()> {
    match url::Url::parse(raw) {
        Ok(u) if matches!(u.scheme(), "http" | "https") && u.host().is_some() => Ok(()),
        Ok(_) => Err(ApiError::invalid(format!(
            "only http(s) URLs are supported: {raw}"
        ))),
        Err(e) => Err(ApiError::invalid(format!("invalid URL {raw:?}: {e}"))),
    }
}
