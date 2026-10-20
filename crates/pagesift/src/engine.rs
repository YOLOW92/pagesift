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
    BlockResources, BrowserPool, PoolConfig, PrivateNetworkGuard, RenderOptions, RequestGuard,
    SubRequest, Verdict,
};
use pagesift_net::{FetchRequest, Fetcher, FetcherConfig, Url};
use pagesift_policy::{Policy, PolicyConfig, RequestKind, RobotsMode};
use tokio::sync::OnceCell;

/// F-1.9：HTTP 抓到的正文少于这么多字符时，`engine: auto` 升级到浏览器渲染。
pub const AUTO_RENDER_MIN_CHARS: usize = 1200;

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
    browser_config: PoolConfig,
    /// 网络层放开了内网（`allowPrivateNetwork`）时，浏览器的子请求也一样放开。
    browser_private_network: bool,
    /// 浏览器第一次需要时才启动；找不到浏览器时记住结果，不反复尝试。
    browser: OnceCell<Option<Arc<BrowserPool>>>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum EngineChoice {
    Http,
    Browser,
    Auto,
}

impl Scraper {
    pub fn new(config: ScraperConfig) -> Result<Self, ScraperError> {
        let browser_private_network = config.fetcher.ssrf.allow_private_network;
        let fetcher = Arc::new(Fetcher::new(config.fetcher).map_err(ScraperError::Fetcher)?);
        let llm = config
            .llm
            .map(pagesift_llm::ChatClient::new)
            .transpose()
            .map_err(ScraperError::Llm)?;
        Ok(Self {
            policy: Arc::new(Policy::new(fetcher, config.policy)),
            store: config.store,
            llm,
            search: config.search,
            browser_config: config.browser,
            browser_private_network,
            browser: OnceCell::new(),
        })
    }

    async fn browser(&self) -> Option<Arc<BrowserPool>> {
        self.browser
            .get_or_init(|| async {
                let guard: Arc<dyn RequestGuard> = if self.browser_private_network {
                    Arc::new(SchemeGuard)
                } else {
                    Arc::new(PrivateNetworkGuard)
                };
                match BrowserPool::new(self.browser_config.clone(), guard).await {
                    Ok(pool) => Some(Arc::new(pool)),
                    Err(e) => {
                        tracing::warn!("browser rendering unavailable: {e}");
                        None
                    }
                }
            })
            .await
            .clone()
    }

    pub async fn shutdown(&self) {
        if let Some(Some(pool)) = self.browser.get() {
            pool.shutdown().await;
        }
    }

    pub async fn scrape(&self, url: &str, opts: &ScrapeOptions) -> Result<Document, ApiError> {
        let mode = self
            .policy
            .robots_mode(RequestKind::Scrape, opts.ignore_robots_txt)
            .map_err(policy_error)?;
        self.scrape_with(url, opts, mode).await
    }

    pub(crate) fn policy(&self) -> &Policy {
        &self.policy
    }

    /// 按给定的 robots 模式抓一页；整站爬取和 map 用它，robots 模式由调用方按请求类型决定。
    /// 先查缓存（语义同 Firecrawl 的 maxAge、minAge、storeInCache），没命中再抓取并写入。
    pub(crate) async fn scrape_with(
        &self,
        url: &str,
        opts: &ScrapeOptions,
        mode: RobotsMode,
    ) -> Result<Document, ApiError> {
        let wants_llm = opts.wants("summary") || opts.wants("json");
        if !wants_llm {
            return self.scrape_cached(url, opts, mode).await;
        }
        let Some(llm) = &self.llm else {
            return Err(ApiError::unsupported(
                "summary and json formats need an LLM: set [llm] in pagesift.toml",
            ));
        };
        // LLM 格式基于正文 Markdown；用户没要 markdown 时内部照样取，最后去掉
        let mut inner = opts.clone();
        inner
            .formats
            .retain(|f| f.kind != "summary" && f.kind != "json");
        let wants_md = inner.wants("markdown");
        if !wants_md {
            inner
                .formats
                .push(pagesift_api::types::Format::new("markdown"));
        }
        let mut doc = self.scrape_cached(url, &inner, mode).await?;
        let md = doc.markdown.clone().unwrap_or_default();
        if opts.wants("summary") {
            let summary = pagesift_llm::summarize(llm, &md, &Default::default())
                .await
                .map_err(llm_error)?;
            doc.summary = Some(summary);
        }
        if let Some(f) = opts.formats.iter().find(|f| f.kind == "json") {
            let schema = f
                .options
                .get("schema")
                .cloned()
                .unwrap_or_else(|| serde_json::json!({ "type": "object" }));
            let eo = pagesift_llm::ExtractOptions {
                prompt: f
                    .options
                    .get("prompt")
                    .and_then(|p| p.as_str())
                    .map(String::from),
            };
            doc.json = Some(
                pagesift_llm::extract_json(llm, &md, &schema, &eo)
                    .await
                    .map_err(llm_error)?,
            );
        }
        if !wants_md {
            doc.markdown = None;
        }
        Ok(doc)
    }

    async fn scrape_cached(
        &self,
        url: &str,
        opts: &ScrapeOptions,
        mode: RobotsMode,
    ) -> Result<Document, ApiError> {
        let Some((store, key)) = self.cache_key(url, opts) else {
            return self.fetch_and_extract(url, opts, mode).await;
        };
        let policy = pagesift_store::CachePolicy::from_firecrawl(
            opts.max_age,
            opts.min_age,
            Some(opts.store_in_cache),
        );
        let lookup = {
            let (store, key) = (store.clone(), key.clone());
            tokio::task::spawn_blocking(move || store.cache().lookup(&key, &policy))
                .await
                .map_err(|e| ApiError::new(ErrorCode::Internal, e.to_string()))?
        };
        match lookup {
            Ok(pagesift_store::CacheLookup::Hit(entry)) => {
                if let Ok(mut doc) = serde_json::from_slice::<Document>(&entry.value) {
                    doc.metadata.cache_state = Some("hit".into());
                    doc.metadata.cached_at = Some(pagesift_api::rfc3339(entry.stored_at));
                    return Ok(doc);
                }
            }
            Ok(pagesift_store::CacheLookup::NoCachedData) => {
                return Err(ApiError::new(
                    ErrorCode::NotFound,
                    "no cached data matches minAge for this URL",
                ));
            }
            Ok(pagesift_store::CacheLookup::Miss) => {}
            // 缓存读不出来不影响抓取
            Err(e) => tracing::warn!("cache lookup failed: {e}"),
        }
        let mut doc = self.fetch_and_extract(url, opts, mode).await?;
        doc.metadata.cache_state = Some("miss".into());
        if policy.store_in_cache
            && let Ok(bytes) = serde_json::to_vec(&doc)
        {
            let r = tokio::task::spawn_blocking(move || store.cache().put(&key, &bytes)).await;
            if let Ok(Err(e)) = r {
                tracing::warn!("cache write failed: {e}");
            }
        }
        Ok(doc)
    }

    /// 缓存键：URL 加上影响结果的参数。带自定义请求头、actions 或条件请求时不走缓存（和 Firecrawl 一致）。
    fn cache_key(
        &self,
        url: &str,
        opts: &ScrapeOptions,
    ) -> Option<(pagesift_store::Store, Vec<u8>)> {
        let store = self.store.clone()?;
        if !opts.headers.is_empty()
            || !opts.actions.is_empty()
            || opts.if_none_match.is_some()
            || opts.if_modified_since.is_some()
        {
            return None;
        }
        let mut formats: Vec<&str> = opts.formats.iter().map(|f| f.kind.as_str()).collect();
        formats.sort_unstable();
        let key = serde_json::json!([
            1,
            url,
            formats,
            opts.only_main_content,
            opts.include_tags,
            opts.exclude_tags,
            opts.engine.as_deref().unwrap_or("auto"),
            opts.mobile,
            opts.wait_for,
            opts.block_ads,
            opts.location,
        ]);
        Some((store, key.to_string().into_bytes()))
    }

    async fn fetch_and_extract(
        &self,
        url: &str,
        opts: &ScrapeOptions,
        mode: RobotsMode,
    ) -> Result<Document, ApiError> {
        let started = Instant::now();
        let choice = match opts.engine.as_deref() {
            Some("http") => EngineChoice::Http,
            Some("browser") => EngineChoice::Browser,
            _ => EngineChoice::Auto,
        };
        let mut timings = BTreeMap::new();
        let mut warnings = Vec::new();

        if choice == EngineChoice::Browser {
            return self
                .render(url, opts, mode, started, timings, warnings)
                .await;
        }

        // 并发名额一直拿到提取结束：已下载、待提取的页面数不超过全局并发，机器忙时内存不会堆积
        let (mut resp, _permit) = self
            .policy
            .fetch_held(fetch_request(url, opts)?, mode)
            .await
            .map_err(policy_error)?;
        timings.insert("fetch".into(), ms(resp.timings.total));
        let mut ext = PagesiftMeta {
            redirect_chain: resp
                .redirect_chain
                .iter()
                .map(|h| h.url.to_string())
                .collect(),
            etag: resp.etag().map(str::to_string),
            last_modified: resp.last_modified().map(str::to_string),
            not_modified: resp.not_modified,
            engine: Some("http".into()),
            ..Default::default()
        };
        if resp.truncated {
            warnings.push("response body exceeded the size limit and was truncated".into());
        }
        let mut meta = Metadata {
            url: Some(resp.final_url.to_string()),
            source_url: Some(url.to_string()),
            status_code: resp.status,
            content_type: resp.content_type().map(str::to_string),
            proxy_used: "basic".into(),
            ..Default::default()
        };
        if resp.not_modified {
            ext.timings = timings;
            meta.pagesift = Some(ext);
            return Ok(Document {
                metadata: meta,
                ..Default::default()
            });
        }

        if is_pdf(&resp.body, resp.content_type()) {
            let t = Instant::now();
            let body = resp.body;
            let pdf = cpu_task(move || pagesift_pdf::extract(&body))
                .await?
                .map_err(|e| {
                    ApiError::new(ErrorCode::UpstreamError, format!("could not read PDF: {e}"))
                })?;
            timings.insert("pdf".into(), ms(t.elapsed()));
            if pdf.scanned {
                warnings
                    .push("scanned PDF: pages have no text layer and OCR is not supported".into());
            }
            meta.title = pdf.metadata.title.clone();
            meta.extra
                .insert("numPages".into(), pdf.metadata.page_count.into());
            return Ok(finish(
                Document {
                    markdown: opts.wants("markdown").then_some(pdf.markdown),
                    ..Default::default()
                },
                meta,
                ext,
                timings,
                warnings,
                started,
            ));
        }

        let html = resp.take_text().text;
        let t = Instant::now();
        let (doc, html) = process_html(html, &resp.final_url, Some(resp.status), opts).await?;
        timings.insert("extract".into(), ms(t.elapsed()));
        // 拦截页如实报告，不升级到浏览器：用真浏览器去过挑战页就是绕过
        check_barrier(&doc)?;

        if choice == EngineChoice::Auto && doc.markdown.chars().count() < AUTO_RENDER_MIN_CHARS {
            if self.browser().await.is_some() {
                return self
                    .render(url, opts, mode, started, timings, warnings)
                    .await;
            }
            warnings.push("render_unavailable".into());
        }

        fill_metadata(&mut meta, &doc.metadata);
        fill_fingerprints(&mut ext, &doc);
        Ok(finish(
            document(doc, opts, html),
            meta,
            ext,
            timings,
            warnings,
            started,
        ))
    }

    async fn render(
        &self,
        url: &str,
        opts: &ScrapeOptions,
        mode: RobotsMode,
        started: Instant,
        mut timings: BTreeMap<String, f64>,
        warnings: Vec<String>,
    ) -> Result<Document, ApiError> {
        let Some(pool) = self.browser().await else {
            return Err(ApiError::unsupported(
                "browser rendering is unavailable: no Chrome/Chromium found (run `pagesift doctor`)",
            ));
        };
        let target = Url::parse(url).map_err(|e| ApiError::invalid(e.to_string()))?;
        let host = target.host_str().unwrap_or_default().to_string();
        let crawl_delay = match mode {
            RobotsMode::Respect => self
                .policy
                .check_robots(&target)
                .await
                .map_err(policy_error)?,
            RobotsMode::Ignore => None,
        };
        let _permit = self.policy.limiter().acquire(&host, crawl_delay).await;
        let ropts = RenderOptions {
            wait_for: Duration::from_millis(opts.wait_for),
            timeout: Duration::from_millis(opts.timeout.unwrap_or(30_000)),
            mobile: opts.mobile,
            languages: opts
                .location
                .as_ref()
                .map(|l| l.languages.clone())
                .unwrap_or_default(),
            block: BlockResources {
                ads: opts.block_ads,
                fonts: true,
                media: true,
                // 只取 HTML，图片不影响正文：src 属性还在，加载失败和离线时一样。以后支持截图时要放开。
                images: true,
            },
            ..RenderOptions::default()
        };
        let page = pool.render(url, &ropts).await.map_err(|e| {
            ApiError::new(ErrorCode::UpstreamError, format!("rendering failed: {e}"))
        })?;
        let t = &page.timings;
        for (k, v) in [
            ("render", t.total),
            ("render_acquire", t.acquire),
            ("render_setup", t.setup),
            ("render_navigation", t.navigation),
            ("render_wait", t.wait),
            ("render_html", t.html),
            ("render_close", t.close),
        ] {
            timings.insert(k.into(), ms(v));
        }
        let final_url = Url::parse(&page.final_url).unwrap_or(target);
        let t = Instant::now();
        let (doc, html) = process_html(page.html, &final_url, page.status, opts).await?;
        timings.insert("extract".into(), ms(t.elapsed()));
        check_barrier(&doc)?;
        let mut meta = Metadata {
            url: Some(final_url.to_string()),
            source_url: Some(url.to_string()),
            status_code: page.status.unwrap_or(200),
            proxy_used: "basic".into(),
            ..Default::default()
        };
        fill_metadata(&mut meta, &doc.metadata);
        let mut ext = PagesiftMeta {
            engine: Some("browser".into()),
            ..Default::default()
        };
        fill_fingerprints(&mut ext, &doc);
        Ok(finish(
            document(doc, opts, html),
            meta,
            ext,
            timings,
            warnings,
            started,
        ))
    }
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
        if self.llm.is_some() {
            &[
                "markdown", "html", "rawHtml", "links", "images", "summary", "json",
            ]
        } else {
            &["markdown", "html", "rawHtml", "links", "images"]
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

/// extract 最多处理多少个页面，每页最多取多少字符的正文。
const EXTRACT_MAX_PAGES: usize = 20;
const EXTRACT_MAX_CHARS_PER_PAGE: usize = 20_000;

impl Scraper {
    pub async fn search(
        &self,
        req: &pagesift_api::types::SearchRequest,
    ) -> Result<serde_json::Value, ApiError> {
        let Some(backend) = &self.search else {
            return Err(ApiError::unsupported(
                "search is not configured: set [search] in pagesift.toml",
            ));
        };
        let mut q = pagesift_search::SearchQuery::new(req.query.clone(), req.limit);
        q.lang = req.lang.clone();
        q.country = req.country.clone();
        q.time_range = match req.tbs.as_deref() {
            Some("qdr:d") => Some(pagesift_search::TimeRange::Day),
            Some("qdr:w") => Some(pagesift_search::TimeRange::Week),
            Some("qdr:m") => Some(pagesift_search::TimeRange::Month),
            Some("qdr:y") => Some(pagesift_search::TimeRange::Year),
            _ => None,
        };
        let search = backend.search(&q);
        let results = match req.timeout {
            Some(ms) => tokio::time::timeout(Duration::from_millis(ms), search)
                .await
                .map_err(|_| ApiError::new(ErrorCode::Timeout, "search timed out"))?,
            None => search.await,
        }
        .map_err(|e| {
            ApiError::new(
                ErrorCode::UpstreamError,
                format!("search backend failed: {e}"),
            )
        })?;

        // 给了 scrapeOptions.formats 才抓取结果页面，并把文档字段合并进对应的结果
        let scrape = req
            .scrape_options
            .as_ref()
            .filter(|o| !o.formats.is_empty());
        let docs: Vec<Option<Document>> = match scrape {
            Some(opts) => {
                let futs = results
                    .iter()
                    .map(|r| async move { self.scrape(&r.url, opts).await.ok() });
                futures_util::future::join_all(futs).await
            }
            None => vec![None; results.len()],
        };
        let web: Vec<serde_json::Value> = results
            .into_iter()
            .zip(docs)
            .map(|(r, doc)| {
                let mut item = serde_json::json!({
                    "url": r.url,
                    "title": r.title,
                    "description": r.description,
                    "position": r.position,
                });
                if let Some(doc) = doc
                    && let (Some(obj), Ok(serde_json::Value::Object(d))) =
                        (item.as_object_mut(), serde_json::to_value(&doc))
                {
                    for (k, v) in d {
                        obj.entry(k).or_insert(v);
                    }
                }
                item
            })
            .collect();
        Ok(serde_json::json!({ "web": web }))
    }

    pub async fn extract(
        &self,
        req: &pagesift_api::types::ExtractRequest,
    ) -> Result<serde_json::Value, ApiError> {
        let Some(llm) = &self.llm else {
            return Err(ApiError::unsupported(
                "extract needs an LLM: set [llm] in pagesift.toml",
            ));
        };
        let mut urls: Vec<String> = Vec::new();
        for u in &req.urls {
            if let Some(site) = u.strip_suffix("/*") {
                let map: pagesift_api::types::MapRequest = serde_json::from_value(
                    serde_json::json!({ "url": site, "limit": EXTRACT_MAX_PAGES }),
                )
                .map_err(|e| ApiError::invalid(e.to_string()))?;
                urls.extend(self.map(&map).await?.into_iter().map(|l| l.url));
            } else {
                urls.push(u.clone());
            }
        }
        urls.truncate(EXTRACT_MAX_PAGES);
        let opts = req.scrape_options.clone().unwrap_or_default();
        let opts = &opts;
        let futs = urls
            .iter()
            .map(|u| async move { (u, self.scrape(u, opts).await) });
        let mut corpus = String::new();
        let mut errors = Vec::new();
        for (u, r) in futures_util::future::join_all(futs).await {
            match r {
                Ok(doc) => {
                    let md: String = doc
                        .markdown
                        .unwrap_or_default()
                        .chars()
                        .take(EXTRACT_MAX_CHARS_PER_PAGE)
                        .collect();
                    corpus.push_str(&format!("\n\n## Source: {u}\n\n{md}"));
                }
                Err(e) => errors.push(format!("{u}: {}", e.code.as_str())),
            }
        }
        if corpus.is_empty() {
            return Err(ApiError::new(
                ErrorCode::UpstreamError,
                format!("none of the pages could be scraped ({})", errors.join(", ")),
            ));
        }
        let schema = req
            .schema
            .clone()
            .unwrap_or_else(|| serde_json::json!({ "type": "object" }));
        let eo = pagesift_llm::ExtractOptions {
            prompt: req.prompt.clone(),
        };
        pagesift_llm::extract_json(llm, &corpus, &schema, &eo)
            .await
            .map_err(llm_error)
    }
}

fn llm_error(e: pagesift_llm::LlmError) -> ApiError {
    ApiError::new(ErrorCode::UpstreamError, format!("LLM request failed: {e}"))
}

fn fetch_request(url: &str, opts: &ScrapeOptions) -> Result<FetchRequest, ApiError> {
    let mut req = FetchRequest::new(url);
    for (k, v) in &opts.headers {
        req = req.header(k.clone(), v.clone());
    }
    if let Some(etag) = &opts.if_none_match {
        req = req.if_none_match(etag.clone());
    }
    if let Some(t) = &opts.if_modified_since {
        let t = httpdate::parse_http_date(t).map_err(|_| {
            ApiError::invalid(format!("ifModifiedSince is not an HTTP date: {t:?}"))
        })?;
        req = req.if_modified_since(t);
    }
    req.timeout = opts.timeout.map(Duration::from_millis);
    Ok(req)
}

/// 正文提取是纯 CPU 计算，放到阻塞线程池里，不占异步运行时的线程。
/// HTML 移交给阻塞线程、处理完原样交还，省掉一次整页复制。
async fn process_html(
    html: String,
    base: &Url,
    status: Option<u16>,
    opts: &ScrapeOptions,
) -> Result<(pagesift_content::Document, String), ApiError> {
    let copts = pagesift_content::Options {
        base_url: Some(base.clone()),
        only_main_content: opts.only_main_content,
        status,
        detect_barrier: true,
        clean_html: opts.wants("html"),
        links: opts.wants("links"),
        images: opts.wants("images"),
        fingerprints: true,
        ..Default::default()
    };
    cpu_task(move || (pagesift_content::process(&html, &copts), html)).await
}

/// 同时进行的 CPU 密集任务（正文提取、PDF 解析）不超过核数。
/// 不加限制时，每个并发请求都会占一个阻塞线程，几十个线程各自持有一块 malloc arena，
/// 释放的内存分散在各处还不回系统，峰值内存成倍增加；超过核数的并发也换不来吞吐。
static CPU_SLOTS: std::sync::LazyLock<tokio::sync::Semaphore> = std::sync::LazyLock::new(|| {
    tokio::sync::Semaphore::new(std::thread::available_parallelism().map_or(4, |n| n.get()))
});

async fn cpu_task<T: Send + 'static>(
    f: impl FnOnce() -> T + Send + 'static,
) -> Result<T, ApiError> {
    let _slot = CPU_SLOTS.acquire().await.expect("semaphore never closed");
    tokio::task::spawn_blocking(f)
        .await
        .map_err(|e| ApiError::new(ErrorCode::Internal, e.to_string()))
}

fn check_barrier(doc: &pagesift_content::Document) -> Result<(), ApiError> {
    let Some(b) = &doc.barrier else { return Ok(()) };
    Err(ApiError::new(
        ErrorCode::AccessBarrier,
        format!(
            "the site returned an access barrier ({} {}); pagesift does not bypass it",
            b.vendor,
            b.kind.as_str()
        ),
    )
    .with_details(serde_json::json!({
        "vendor": b.vendor,
        "kind": b.kind.as_str(),
        "confidence": b.confidence,
        "ruleId": b.rule_id,
    })))
}

fn document(doc: pagesift_content::Document, opts: &ScrapeOptions, raw_html: String) -> Document {
    Document {
        markdown: opts.wants("markdown").then_some(doc.markdown),
        html: doc.html,
        raw_html: opts.wants("rawHtml").then_some(raw_html),
        links: doc.links.map(|l| l.into_iter().map(|l| l.url).collect()),
        images: doc.images.map(|i| i.into_iter().map(|i| i.src).collect()),
        ..Default::default()
    }
}

fn fill_fingerprints(ext: &mut PagesiftMeta, doc: &pagesift_content::Document) {
    if let Some(f) = &doc.fingerprints {
        ext.content_hash = Some(f.markdown.clone());
        ext.raw_hash = Some(f.raw_html.clone());
        ext.text_hash = Some(f.text.clone());
    }
}

fn fill_metadata(meta: &mut Metadata, m: &pagesift_content::Metadata) {
    meta.title = m.title.clone();
    meta.description = m.description.clone();
    meta.language = m.language.clone();
    meta.keywords = m.keywords.clone();
    meta.robots = m.robots.clone();
    meta.favicon = m.favicon.clone();
    meta.published_time = m.published_time.clone();
    meta.modified_time = m.modified_time.clone();
    for (k, v) in &m.extra {
        meta.extra
            .entry(firecrawl_meta_key(k))
            .or_insert_with(|| v.clone().into());
    }
}

/// `og:image` → `ogImage`、`article:published_time` → `articlePublishedTime`，和 Firecrawl 的键名风格一致。
fn firecrawl_meta_key(raw: &str) -> String {
    let mut out = String::with_capacity(raw.len());
    let mut upper = false;
    for c in raw.chars() {
        if matches!(c, ':' | '_' | '-' | '.') {
            upper = true;
        } else if upper {
            out.extend(c.to_uppercase());
            upper = false;
        } else {
            out.push(c);
        }
    }
    out
}

fn finish(
    mut doc: Document,
    mut meta: Metadata,
    mut ext: PagesiftMeta,
    mut timings: BTreeMap<String, f64>,
    warnings: Vec<String>,
    started: Instant,
) -> Document {
    timings.insert("total".into(), ms(started.elapsed()));
    ext.timings = timings;
    ext.warnings = warnings;
    meta.pagesift = Some(ext);
    doc.metadata = meta;
    doc
}

fn ms(d: Duration) -> f64 {
    (d.as_secs_f64() * 1000.0 * 100.0).round() / 100.0
}

fn is_pdf(body: &[u8], content_type: Option<&str>) -> bool {
    body.starts_with(b"%PDF-")
        || content_type.is_some_and(|c| c.to_ascii_lowercase().contains("application/pdf"))
}

pub(crate) fn policy_error(e: pagesift_policy::Error) -> ApiError {
    use pagesift_policy::ErrorCode as P;
    let code = match e.code() {
        P::RobotsDisallowed => ErrorCode::RobotsDisallowed,
        P::AccessBarrier => ErrorCode::AccessBarrier,
        P::RateLimited => ErrorCode::RateLimited,
        P::HostCooldown => ErrorCode::HostCooldown,
        P::Timeout => ErrorCode::Timeout,
        P::SsrfBlocked => ErrorCode::SsrfBlocked,
        P::TooLarge => ErrorCode::TooLarge,
        P::UpstreamError => ErrorCode::UpstreamError,
        P::UnsupportedOption => ErrorCode::UnsupportedOption,
        P::InvalidRequest => ErrorCode::InvalidRequest,
        _ => ErrorCode::Internal,
    };
    let retry = e.retry_after().map(|d| d.as_secs().max(1));
    let mut err = ApiError::new(code, e.to_string());
    if let Some(s) = retry {
        err = err.with_retry_after(s);
    }
    err
}

#[cfg(test)]
mod tests {
    use super::*;

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
