//! 抓取引擎：把网络层、礼貌策略、正文提取、PDF 和浏览器渲染串成一次完整的抓取。
//!
//! 流程：robots 模式 → HTTP 抓取（策略层负责冷却、robots、限速）→ 按内容类型分流（PDF / HTML）
//! → `engine: auto` 时正文太少就升级到浏览器渲染 → 组装成 Firecrawl 格式的文档。

use std::collections::BTreeMap;
use std::sync::Arc;
use std::time::{Duration, Instant};

use pagesift_api::types::{Document, Metadata, PagesiftMeta, ScrapeOptions};
use pagesift_api::{ApiError, BoxFuture, Engine, ErrorCode};
use pagesift_cdp::{
    BlockResources, BrowserPool, PoolConfig, PrivateNetworkGuard, RenderOptions, RenderedPage,
    RequestGuard, SubRequest, Verdict,
};
use pagesift_net::{FetchRequest, Fetcher, FetcherConfig, Url};
use pagesift_policy::{Policy, PolicyConfig, RequestKind, RobotsMode};
use tokio::sync::OnceCell;

mod extract;
mod fetch;
mod frames;
mod output;
mod scraper;
use extract::*;
use fetch::*;
use frames::*;
pub(crate) use output::*;

/// F-1.9：HTTP 抓到的正文少于这么多（加权）字符时，`engine: auto` 升级到浏览器渲染。
/// 中日韩文字一个字按 [`CJK_WEIGHT`] 个字符算（信息量和几个拉丁字母相当），见 [`text_weight`]。
pub const AUTO_RENDER_MIN_CHARS: usize = 1200;
/// 已经认出是一篇文章（有发布时间或 `og:type=article`）时，正文到这么多加权字符就不再渲染：
/// 短新闻 HTTP 已经拿全了。AiSeek 的生产数据里，63% 的渲染拿到的文字和 HTTP 一样多，
/// 多半就是这类短文章。
pub const AUTO_RENDER_ARTICLE_MIN_CHARS: usize = 300;
pub const CJK_WEIGHT: usize = 3;

/// 正文的加权长度：空白不算，中日韩文字按 [`CJK_WEIGHT`] 算。
pub fn text_weight(s: &str) -> usize {
    s.chars()
        .filter(|c| !c.is_whitespace())
        .map(|c| if is_cjk(c) { CJK_WEIGHT } else { 1 })
        .sum()
}

fn is_cjk(c: char) -> bool {
    matches!(c as u32,
        0x3040..=0x30FF   // 平假名、片假名
        | 0x3400..=0x4DBF // CJK 扩展 A
        | 0x4E00..=0x9FFF // CJK 统一汉字
        | 0xAC00..=0xD7AF // 谚文音节
        | 0xF900..=0xFAFF // CJK 兼容汉字
        | 0x20000..=0x2FA1F)
}

/// `engine: auto` 时 HTTP 的结果是否还要用浏览器渲染。
pub fn needs_render(doc: &pagesift_content::Document) -> bool {
    let w = text_weight(&doc.markdown);
    if w >= AUTO_RENDER_MIN_CHARS {
        return false;
    }
    let m = &doc.metadata;
    let article = m.published_time.is_some()
        || m.extra
            .iter()
            .any(|(k, v)| k == "og:type" && v.eq_ignore_ascii_case("article"));
    !(article && w >= AUTO_RENDER_ARTICLE_MIN_CHARS)
}
/// 浏览器渲染出的正文少于这么多字符时，换下一个内核再试（Lightpanda 的脚本支持不全，
/// 单页应用可能渲染不出内容）。最后一个内核的结果照常返回。
pub const RENDER_FALLBACK_MIN_CHARS: usize = 200;

#[derive(Clone, Default)]
pub struct ScraperConfig {
    pub fetcher: FetcherConfig,
    pub policy: PolicyConfig,
    pub browser: PoolConfig,
    /// 抓取结果缓存（`maxAge`、`minAge`、`storeInCache`）；不设时不缓存。
    pub store: Option<pagesift_store::Store>,
    /// `summary`、`json` 格式和 `/v2/extract` 用的 OpenAI 兼容服务。
    pub llm: Option<pagesift_llm::LlmConfig>,
    /// `/v2/search` 用的搜索后端。
    pub search: Option<Arc<dyn pagesift_search::SearchBackend>>,
    /// 命名出口（名字，代理地址）：请求的 `egress` 选其中一个，其余抓取设置和默认出口相同。
    pub egress: Vec<(String, String)>,
}

/// 创建 [`Scraper`] 失败：网络层或 LLM 客户端的配置不对。
#[derive(Debug)]
pub enum ScraperError {
    Fetcher(pagesift_net::Error),
    Llm(pagesift_llm::LlmError),
}

impl std::fmt::Display for ScraperError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Fetcher(e) => write!(f, "fetcher configuration: {e}"),
            Self::Llm(e) => write!(f, "LLM configuration: {e}"),
        }
    }
}

impl std::error::Error for ScraperError {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        match self {
            Self::Fetcher(e) => Some(e),
            Self::Llm(e) => Some(e),
        }
    }
}

pub struct Scraper {
    policy: Arc<Policy>,
    store: Option<pagesift_store::Store>,
    llm: Option<pagesift_llm::ChatClient>,
    search: Option<Arc<dyn pagesift_search::SearchBackend>>,
    /// 网络层放开了内网（`allowPrivateNetwork`）时，浏览器的子请求也一样放开。
    browser_private_network: bool,
    /// 按优先级排列的内核（见 [`pagesift_cdp::engine_chain`]）。每个池第一次需要时才创建；
    /// 创建失败时记住结果，不反复尝试。
    browsers: Vec<(PoolConfig, OnceCell<Option<Arc<BrowserPool>>>)>,
    /// 每个内核最近一次启动失败的时间：之后 [`LAUNCH_RETRY`] 内跳过它，不让每个请求都去试一次。
    launch_failed: Vec<std::sync::Mutex<Option<Instant>>>,
    /// 爬取里同时进行的抓取数，等于全局并发。排队的页面只占一个很小的等待 future，
    /// 拿到名额才创建完整的抓取状态机（十几 KB）：同时爬上千个站时，排队的页面不再各占一份。
    crawl_slots: Arc<tokio::sync::Semaphore>,
    target_concurrency: std::sync::atomic::AtomicUsize,
    held_permits: std::sync::Mutex<Vec<tokio::sync::OwnedSemaphorePermit>>,
    learner: crate::learn::HostLearner,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum EngineChoice {
    Http,
    Browser,
    Auto,
}

impl Engine for Scraper {
    fn scrape<'a>(
        &'a self,
        url: &'a str,
        opts: &'a ScrapeOptions,
    ) -> BoxFuture<'a, Result<Document, ApiError>> {
        Box::pin(Scraper::scrape(self, url, opts))
    }

    fn map<'a>(
        &'a self,
        req: &'a pagesift_api::types::MapRequest,
    ) -> BoxFuture<'a, Result<Vec<pagesift_api::types::MapLink>, ApiError>> {
        Box::pin(Scraper::map(self, req))
    }

    fn crawl<'a>(
        &'a self,
        req: &'a pagesift_api::types::CrawlRequest,
        sink: Arc<dyn pagesift_api::CrawlSink>,
    ) -> BoxFuture<'a, Result<(), ApiError>> {
        Box::pin(Scraper::crawl(self, req, sink))
    }

    fn search<'a>(
        &'a self,
        req: &'a pagesift_api::types::SearchRequest,
    ) -> BoxFuture<'a, Result<serde_json::Value, ApiError>> {
        Box::pin(Scraper::search(self, req))
    }

    fn extract<'a>(
        &'a self,
        req: &'a pagesift_api::types::ExtractRequest,
    ) -> BoxFuture<'a, Result<serde_json::Value, ApiError>> {
        Box::pin(Scraper::extract(self, req))
    }

    fn supported_formats(&self) -> &'static [&'static str] {
        match (self.llm.is_some(), self.store.is_some()) {
            (true, true) => &[
                "markdown",
                "html",
                "rawHtml",
                "links",
                "images",
                "summary",
                "json",
                "changeTracking",
            ],
            (true, false) => &[
                "markdown", "html", "rawHtml", "links", "images", "summary", "json",
            ],
            (false, true) => &[
                "markdown",
                "html",
                "rawHtml",
                "links",
                "images",
                "changeTracking",
            ],
            (false, false) => &["markdown", "html", "rawHtml", "links", "images"],
        }
    }
}

/// 放开内网时浏览器子请求的检查：只限制协议，不查地址。
struct SchemeGuard;

impl RequestGuard for SchemeGuard {
    fn check<'a>(&'a self, req: &'a SubRequest) -> pagesift_cdp::BoxFuture<'a, Verdict> {
        let ok = Url::parse(&req.url)
            .is_ok_and(|u| matches!(u.scheme(), "http" | "https" | "data" | "blob" | "about"));
        Box::pin(async move {
            if ok {
                Verdict::Allow
            } else {
                Verdict::Block("scheme not allowed".into())
            }
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn content(md: &str, published: bool, og_article: bool) -> pagesift_content::Document {
        let mut d = pagesift_content::process("<p>x</p>", &Default::default());
        d.markdown = md.to_string();
        d.metadata.published_time = published.then(|| "2026-10-21T00:00:00Z".into());
        if og_article {
            d.metadata.extra.push(("og:type".into(), "article".into()));
        }
        d
    }

    #[test]
    fn static_learning_cannot_suppress_client_code() {
        for html in [
            "<script>hydrate()</script>",
            "<script src='/app.js'></script>",
            "<SCRIPT TYPE='module' src='/view.js'></SCRIPT>",
            "<body onload='hydrate()'>",
            "<button onclick='hydrate()'>Open</button>",
        ] {
            assert!(has_client_code(html), "{html}");
        }
        for html in [
            "<article><p>Short but complete.</p></article>",
            "<script type='application/ld+json'>{}</script>",
            "<script type='APPLICATION/JSON; charset=utf-8'>{}</script>",
            "<script type='text/plain'>inert sample</script>",
        ] {
            assert!(!has_client_code(html), "{html}");
        }
    }

    #[test]
    fn render_rule_weights_cjk_and_trusts_short_articles() {
        assert_eq!(text_weight("ab c\n"), 3);
        assert_eq!(text_weight("日本語のニュース"), 8 * CJK_WEIGHT);
        // 500 个汉字按 1500 算，不用渲染；500 个拉丁字母要渲染
        assert!(!needs_render(&content(&"字".repeat(500), false, false)));
        assert!(needs_render(&content(&"a".repeat(500), false, false)));
        // 认出是文章且有 300 以上：不渲染；太短的文章照样渲染
        assert!(!needs_render(&content(&"a".repeat(400), true, false)));
        assert!(!needs_render(&content(&"a".repeat(400), false, true)));
        assert!(needs_render(&content(&"a".repeat(100), true, true)));
    }

    #[test]
    fn render_errors_are_classified() {
        use pagesift_cdp::Error as E;
        for (e, code) in [
            (E::Timeout, ErrorCode::Timeout),
            (E::Busy, ErrorCode::Busy),
            (
                E::Navigation("net::ERR_NAME_NOT_RESOLVED".into()),
                ErrorCode::DnsFailure,
            ),
            (
                E::Navigation("net::ERR_CONNECTION_RESET".into()),
                ErrorCode::NetworkError,
            ),
            (E::ConnectionClosed, ErrorCode::UpstreamError),
        ] {
            assert_eq!(render_error_code(&e), code, "{e}");
        }
    }

    #[test]
    fn finds_only_visible_same_origin_and_srcdoc_iframes() {
        let base = Url::parse("https://a.test/page").unwrap();
        let html = r#"<body>
            <iframe src="https://www.googletagmanager.com/ns.html"></iframe>
            <iframe src="/pixel" style="display: none"></iframe>
            <iframe src="/hidden" aria-hidden="true"></iframe>
            <iframe src="/tiny" width="1" height="1"></iframe>
            <iframe src="about:blank"></iframe>
            <iframe src="/comments?id=1&amp;x=2" height="160"></iframe>
            <iframe srcdoc="&lt;p&gt;inline&lt;/p&gt;"></iframe>
            <iframe srcdoc="&lt;p&gt;x&lt;/p&gt;" style="visibility:hidden"></iframe>
        </body>"#;
        let frames = content_iframes(html, &base);
        assert_eq!(frames.len(), 2, "{frames:?}");
        assert_eq!(frames[0].0, "/comments?id=1&x=2");
        assert_eq!(
            frames[0].1.as_ref().map(Url::as_str),
            Some("https://a.test/comments?id=1&x=2")
        );
        assert_eq!(frames[1], ("<p>inline</p>".to_string(), None));
        assert!(content_iframes("<p>no frames</p>", &base).is_empty());
    }

    #[test]
    fn replaces_the_matching_iframe_element() {
        let mut html = r#"<p>a</p><IFRAME src="/x" hidden></IFRAME><iframe src="/x">fallback</iframe><p>b</p>"#
            .to_string();
        assert!(replace_iframe(&mut html, "<div>X</div>", |el| {
            !hidden_frame(el) && src_is(el, "/x")
        }));
        assert_eq!(
            html,
            r#"<p>a</p><IFRAME src="/x" hidden></IFRAME><div>X</div><p>b</p>"#
        );
        assert!(!replace_iframe(&mut html, "", |el| src_is(el, "/missing")));
    }

    #[test]
    fn takes_the_body_of_an_iframe_document() {
        assert_eq!(
            body_inner("<html><head><title>t</title></head><BODY class=x><p>hi</p></body></html>"),
            "<p>hi</p>"
        );
        assert_eq!(body_inner("<p>fragment</p>"), "<p>fragment</p>");
    }

    #[test]
    fn meta_keys_follow_firecrawl_style() {
        assert_eq!(firecrawl_meta_key("og:image"), "ogImage");
        assert_eq!(firecrawl_meta_key("og:site_name"), "ogSiteName");
        assert_eq!(
            firecrawl_meta_key("article:published_time"),
            "articlePublishedTime"
        );
        assert_eq!(firecrawl_meta_key("dc.title"), "dcTitle");
    }
}
