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
    crawl_slots: tokio::sync::Semaphore,
    learner: crate::learn::HostLearner,
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
        let mut egress = Vec::new();
        for (name, proxy) in &config.egress {
            let mut fc = config.fetcher.clone();
            fc.proxy = Some(proxy.clone());
            egress.push((
                name.clone(),
                Arc::new(Fetcher::new(fc).map_err(ScraperError::Fetcher)?),
            ));
        }
        let fetcher = Arc::new(Fetcher::new(config.fetcher).map_err(ScraperError::Fetcher)?);
        let llm = config
            .llm
            .map(pagesift_llm::ChatClient::new)
            .transpose()
            .map_err(ScraperError::Llm)?;
        let crawl_slots = tokio::sync::Semaphore::new(config.policy.rate.global_concurrency.max(1));
        Ok(Self {
            crawl_slots,
            policy: Arc::new({
                let mut p = Policy::new(fetcher, config.policy);
                for (name, f) in egress {
                    p.add_egress(name, f);
                }
                p
            }),
            store: config.store,
            llm,
            search: config.search,
            browsers: {
                let chain = pagesift_cdp::engine_chain(&config.browser, browser_private_network);
                log_engine_chain(&chain);
                chain.into_iter().map(|c| (c, OnceCell::new())).collect()
            },
            launch_failed: Vec::new(),
            browser_private_network,
            learner: crate::learn::HostLearner::default(),
        })
        .map(|mut s| {
            s.launch_failed = (0..s.browsers.len()).map(|_| Default::default()).collect();
            s
        })
    }

    /// 第 `i` 个内核的池；不存在或创建失败时返回 `None`。
    async fn browser(&self, i: usize) -> Option<Arc<BrowserPool>> {
        let (cfg, cell) = self.browsers.get(i)?;
        cell.get_or_init(|| async {
            let guard: Arc<dyn RequestGuard> = if self.browser_private_network {
                Arc::new(SchemeGuard)
            } else {
                Arc::new(PrivateNetworkGuard)
            };
            match BrowserPool::new(cfg.clone(), guard).await {
                Ok(pool) => Some(Arc::new(pool)),
                Err(e) => {
                    let engine = cfg.engine.map_or("browser", |k| k.name());
                    tracing::warn!("{engine} rendering unavailable: {e}");
                    None
                }
            }
        })
        .await
        .clone()
    }

    async fn has_browser(&self) -> bool {
        for i in 0..self.browsers.len() {
            if self.browser(i).await.is_some() {
                return true;
            }
        }
        false
    }

    /// 服务启动时在后台把每个内核启动一次，确认哪些能用（日志里写明），能用的浏览器留着备用。
    pub async fn probe_browsers(&self) {
        for i in 0..self.browsers.len() {
            let Some(pool) = self.browser(i).await else {
                continue;
            };
            let engine = pool.engine().name();
            match pool.probe().await {
                Ok(()) => tracing::info!(engine, "browser engine ready"),
                Err(e) => self.note_launch_failure(i, engine, &e),
            }
        }
    }

    /// 内核启动失败：记下来，接下来一段时间跳过它；只在第一次失败时警告，并说明常见原因。
    fn note_launch_failure(&self, i: usize, engine: &str, e: &pagesift_cdp::Error) {
        let first = self.launch_failed[i]
            .lock()
            .unwrap()
            .replace(Instant::now())
            .is_none();
        if first {
            let msg = e.to_string();
            let hint = if msg.contains("sandbox") {
                ": the browser sandbox needs unprivileged user namespaces, which this host or container \
                 does not allow (check kernel.apparmor_restrict_unprivileged_userns and the container's \
                 seccomp/AppArmor profile); pagesift does not disable the sandbox"
            } else {
                ""
            };
            tracing::warn!(
                engine,
                retry_after_secs = LAUNCH_RETRY.as_secs(),
                "{engine} fallback unavailable: {msg}{hint}"
            );
        }
    }

    pub async fn shutdown(&self) {
        for (_, cell) in &self.browsers {
            if let Some(Some(pool)) = cell.get() {
                pool.shutdown().await;
            }
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

    pub(crate) fn crawl_slots(&self) -> &tokio::sync::Semaphore {
        &self.crawl_slots
    }

    /// 按给定的 robots 模式抓一页；整站爬取和 map 用它，robots 模式由调用方按请求类型决定。
    /// 先查缓存（语义同 Firecrawl 的 maxAge、minAge、storeInCache），没命中再抓取并写入。
    /// `queryFilter`、LLM 格式和 `changeTracking` 都在缓存之外、基于正文 Markdown 做，
    /// 缓存里存的总是完整正文。
    pub(crate) async fn scrape_with(
        &self,
        url: &str,
        opts: &ScrapeOptions,
        mode: RobotsMode,
    ) -> Result<Document, ApiError> {
        let wants_llm = opts.wants("summary") || opts.wants("json");
        let tracking = opts
            .formats
            .iter()
            .find(|f| f.kind == "changeTracking")
            .map(crate::change::Tracking::parse)
            .transpose()?;
        if !wants_llm
            && tracking.is_none()
            && opts.query_filter.is_none()
            && opts.chunking.is_none()
        {
            return self.scrape_cached(url, opts, mode).await;
        }
        if (wants_llm || tracking.as_ref().is_some_and(|t| t.json)) && self.llm.is_none() {
            return Err(ApiError::unsupported(
                "summary and json formats (and the json mode of changeTracking) need an LLM: set [llm] in pagesift.toml",
            ));
        }
        if tracking.is_some() && self.store.is_none() {
            return Err(ApiError::unsupported(
                "changeTracking keeps history in the store: enable [cache] in pagesift.toml",
            ));
        }
        // 这些都基于正文 Markdown；用户没要 markdown 时内部照样取，最后去掉
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
        // 变化检测要和最新的页面比较：调用方没给 maxAge 时不读缓存
        if tracking.is_some() && inner.max_age.is_none() {
            inner.max_age = Some(0);
        }
        let mut doc = self.scrape_cached(url, &inner, mode).await?;
        if let Some(q) = &opts.query_filter {
            apply_query_filter(&mut doc, q).await?;
        }
        if let Some(c) = &opts.chunking {
            doc.chunks = Some(chunk_markdown(
                doc.markdown.as_deref().unwrap_or_default(),
                c,
            )?);
        }
        let md = doc.markdown.clone().unwrap_or_default();
        if let Some(llm) = self.llm.as_ref().filter(|_| wants_llm) {
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
        }
        if let (Some(t), Some(store)) = (&tracking, &self.store) {
            self.track_changes(store, t, url, &mut doc).await?;
        }
        if !wants_md {
            doc.markdown = None;
        }
        Ok(doc)
    }

    /// `changeTracking`：和上一次的正文比较，结果放进 `doc.change_tracking`。
    async fn track_changes(
        &self,
        store: &pagesift_store::Store,
        t: &crate::change::Tracking,
        url: &str,
        doc: &mut Document,
    ) -> Result<(), ApiError> {
        use crate::change::{Current, Previous};
        let ext = doc.metadata.pagesift.get_or_insert_with(Default::default);
        let current = if ext.not_modified {
            Current::NotModified
        } else if matches!(doc.metadata.status_code, 404 | 410) {
            Current::Gone
        } else {
            Current::Page(doc.markdown.as_deref().unwrap_or_default())
        };
        let mut outcome = crate::change::track(store, t, url, current).await?;
        ext.warnings.append(&mut outcome.warnings);
        if let (true, "changed", Previous::Page(prev), Some(llm)) = (
            t.json,
            outcome.tracking.change_status.as_str(),
            &outcome.previous,
            &self.llm,
        ) {
            let schema = t
                .schema
                .clone()
                .unwrap_or_else(|| serde_json::json!({ "type": "object" }));
            let eo = pagesift_llm::ExtractOptions {
                prompt: t.prompt.clone(),
            };
            let cur = doc.markdown.as_deref().unwrap_or_default();
            let (before, after) = futures_util::future::try_join(
                pagesift_llm::extract_json(llm, prev, &schema, &eo),
                pagesift_llm::extract_json(llm, cur, &schema, &eo),
            )
            .await
            .map_err(llm_error)?;
            outcome.tracking.json = Some(crate::change::json_changes(&before, &after));
        }
        doc.change_tracking = Some(outcome.tracking);
        Ok(())
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
            || opts.discover_scripts
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
        let choice = match opts.engine.as_deref() {
            Some("http") => EngineChoice::Http,
            Some("browser") => EngineChoice::Browser,
            _ => EngineChoice::Auto,
        };
        let mut timings = BTreeMap::new();
        let mut warnings = Vec::new();

        if choice == EngineChoice::Browser && opts.egress.is_some() {
            return Err(ApiError::unsupported(
                "egress applies to HTTP fetching only; browser rendering does not use it",
            ));
        }
        if choice == EngineChoice::Browser {
            return self
                .render(
                    url,
                    opts,
                    mode,
                    Progress {
                        started,
                        timings,
                        warnings,
                    },
                    None,
                )
                .await;
        }

        // 并发名额一直拿到提取结束：已下载、待提取的页面数不超过全局并发，机器忙时内存不会堆积
        let fetch =
            self.policy
                .fetch_held_via(fetch_request(url, opts)?, mode, opts.egress.as_deref());
        let (mut resp, _permit) = match request_deadline(opts, started) {
            Some(d) => tokio::time::timeout_at(d.into(), fetch)
                .await
                .map_err(|_| ApiError::new(ErrorCode::Timeout, "request deadline passed"))?,
            None => fetch.await,
        }
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

        let navigation = if opts.wants("links") {
            crate::discovery::header_links(
                &resp.final_url,
                resp.headers
                    .get_all("link")
                    .iter()
                    .filter_map(|h| h.to_str().ok()),
            )
        } else {
            Vec::new()
        };
        let html = resp.take_text().text;
        let t = Instant::now();
        let (doc, html) = process_html(html, &resp.final_url, Some(resp.status), opts).await?;
        timings.insert("extract".into(), ms(t.elapsed()));
        // 拦截页如实报告，不升级到浏览器：用真浏览器去过挑战页就是绕过
        check_barrier(&doc)?;

        let http_chars = text_weight(&doc.markdown);
        if choice == EngineChoice::Auto && needs_render(&doc) {
            if opts.egress.is_some() {
                // 浏览器不走指定的出口，用它渲染出口地址就不一致了
                warnings.push("render_skipped_egress".into());
            } else if self.learner.skip_render(url) && !has_client_code(&html) {
                // 同路径族的静态经验不能覆盖带客户端代码的短页面。
                warnings.push("render_skipped_learned".into());
            } else if self.has_browser().await {
                return self
                    .render(
                        url,
                        opts,
                        mode,
                        Progress {
                            started,
                            timings,
                            warnings,
                        },
                        Some(http_chars),
                    )
                    .await;
            } else {
                warnings.push("render_unavailable".into());
            }
        }

        let mut navigation = navigation;
        if opts.discover_scripts {
            // Release the document's admission permit before dependency fetches:
            // concurrent root pages must not occupy every per-host slot while
            // waiting on their own scripts.
            drop(_permit);
            let discovery_started = Instant::now();
            let work = crate::scripts::discover_script_graph(
                &self.policy,
                &html,
                &resp.final_url,
                Default::default(),
            );
            let graph = match request_deadline(opts, started) {
                Some(d) => tokio::time::timeout_at(d.into(), work).await.map_err(|_| {
                    ApiError::new(ErrorCode::Timeout, "script discovery deadline passed")
                })?,
                None => work.await,
            }
            .map_err(|e| ApiError::invalid(e.to_string()))?;
            if graph.incomplete {
                warnings.push("script_discovery_incomplete".into());
            }
            navigation.extend(
                graph
                    .edges
                    .into_iter()
                    .filter(|e| e.kind == "static-navigation")
                    .map(|e| e.url),
            );
            timings.insert("scriptDiscovery".into(), ms(discovery_started.elapsed()));
        }
        fill_metadata(&mut meta, &doc.metadata);
        fill_fingerprints(&mut ext, &doc);
        let mut result = document(doc, opts, html);
        if let Some(links) = result.links.as_mut() {
            for link in navigation {
                if !links.contains(&link) {
                    links.push(link);
                }
            }
        }
        Ok(finish(result, meta, ext, timings, warnings, started))
    }

    async fn render(
        &self,
        url: &str,
        opts: &ScrapeOptions,
        mode: RobotsMode,
        progress: Progress,
        // `engine: auto` 从 HTTP 升级过来时，HTTP 正文的字符数（用来学这个站值不值得渲染）
        http_chars: Option<usize>,
    ) -> Result<Document, ApiError> {
        let Progress {
            started,
            mut timings,
            warnings,
        } = progress;
        if !self.has_browser().await {
            return Err(ApiError::unsupported(
                "browser rendering is unavailable: no browser engine found (run `pagesift doctor`)",
            ));
        }
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
        // 调用方给了 `timeout` 时它是整个请求的总时限：HTTP 抓取、排队、每个内核的渲染都从里面扣。
        // 没给时（整站爬取）排队不限时，渲染从拿到名额起算默认时限。
        let acquire = self.policy.limiter().acquire(&host, crawl_delay);
        let (_permit, deadline) = match request_deadline(opts, started) {
            Some(d) => match tokio::time::timeout_at(d.into(), acquire).await {
                Ok(p) => (p, d),
                Err(_) => {
                    return Err(ApiError::new(
                        ErrorCode::Timeout,
                        "request deadline passed while waiting for the host's rate limit",
                    ));
                }
            },
            None => (acquire.await, Instant::now() + DEFAULT_TIMEOUT),
        };
        let mut ropts = RenderOptions {
            wait_for: Duration::from_millis(opts.wait_for),
            timeout: deadline.saturating_duration_since(Instant::now()),
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

        // 按优先级逐个内核尝试：渲染失败、页面有 iframe 而内核不渲染 iframe、正文太少时换下一个。
        // 后面的内核都不可用或失败时，用前面拿到的结果。
        let mut best: Option<Rendered> = None;
        let mut last_err = None;
        let from = self
            .learner
            .start_engine(&host)
            .min(self.browsers.len().saturating_sub(1));
        for i in from..self.browsers.len() {
            if self.launch_failed[i]
                .lock()
                .unwrap()
                .is_some_and(|t| t.elapsed() < LAUNCH_RETRY)
            {
                continue;
            }
            let Some(pool) = self.browser(i).await else {
                continue;
            };
            let engine = pool.engine();
            ropts.timeout = deadline.saturating_duration_since(Instant::now());
            if ropts.timeout < MIN_ATTEMPT {
                // 剩下的时间不够再试一个内核：有结果就用，没有就算超时
                last_err.get_or_insert(pagesift_cdp::Error::Timeout);
                break;
            }
            let page = match pool.render(url, &ropts).await {
                Ok(p) => p,
                Err(e) => {
                    tracing::debug!(engine = engine.name(), url, "render failed: {e}");
                    if matches!(
                        e,
                        pagesift_cdp::Error::Launch(_) | pagesift_cdp::Error::Connect(_)
                    ) {
                        self.note_launch_failure(i, engine.name(), &e);
                    }
                    // 完整内核（渲染和 Chrome 一致）报的导航错误是网络层的结果（地址打不开、
                    // 被拦截），换内核也一样，不再往下试
                    let final_err =
                        engine.renders_iframes() && matches!(e, pagesift_cdp::Error::Navigation(_));
                    last_err = Some(e);
                    if final_err {
                        break;
                    }
                    continue;
                }
            };
            let final_url = Url::parse(&page.final_url).unwrap_or_else(|_| target.clone());
            let RenderedPage {
                mut html,
                status,
                timings: page_timings,
                iframes_inlined,
                ..
            } = page;
            let mut missing_iframes = 0;
            if !engine.renders_iframes() && iframes_inlined == 0 {
                let frames = content_iframes(&html, &final_url);
                if !frames.is_empty() {
                    missing_iframes = inline_iframes_http(
                        self.policy.fetcher(),
                        &mut html,
                        frames,
                        ropts.timeout,
                    )
                    .await;
                }
            }
            let t = Instant::now();
            let (doc, html) = process_html(html, &final_url, status, opts).await?;
            let extract = t.elapsed();
            // 错误页正文少是正常的，换内核也一样
            let thin = status.is_none_or(|s| s < 400)
                && text_weight(&doc.markdown) < RENDER_FALLBACK_MIN_CHARS;
            let r = Rendered {
                index: i,
                engine: engine.name(),
                final_url,
                status,
                timings: page_timings,
                extract,
                doc,
                html,
            };
            // 完整内核（moli、Chrome 系）渲染结果一致：正文少就是页面本身少，不再换内核
            if (missing_iframes == 0 && !thin) || engine.renders_iframes() {
                best = Some(r);
                break;
            }
            tracing::debug!(
                engine = engine.name(),
                url,
                missing_iframes,
                thin,
                "trying the next browser engine"
            );
            // 有 iframe 的结果比只是正文少的结果更完整，留先拿到的
            if best.is_none() {
                best = Some(r);
            }
        }
        let Some(r) = best else {
            return Err(match last_err {
                Some(e) => ApiError::new(render_error_code(&e), format!("rendering failed: {e}")),
                None => ApiError::new(ErrorCode::UpstreamError, "no browser engine available"),
            });
        };
        self.learner.record_engine(&host, from, r.index);
        if let Some(n) = http_chars {
            self.learner
                .record_render(url, n, text_weight(&r.doc.markdown));
        }

        let t = &r.timings;
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
        timings.insert("extract".into(), ms(r.extract));
        check_barrier(&r.doc)?;
        let mut meta = Metadata {
            url: Some(r.final_url.to_string()),
            source_url: Some(url.to_string()),
            status_code: r.status.unwrap_or(200),
            proxy_used: "basic".into(),
            ..Default::default()
        };
        fill_metadata(&mut meta, &r.doc.metadata);
        let mut ext = PagesiftMeta {
            engine: Some("browser".into()),
            browser_engine: Some(r.engine.into()),
            ..Default::default()
        };
        fill_fingerprints(&mut ext, &r.doc);
        Ok(finish(
            document(r.doc, opts, r.html),
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
/// 到渲染为止已经记下的开始时间、各阶段耗时和警告。
struct Progress {
    started: Instant,
    timings: BTreeMap<String, f64>,
    warnings: Vec<String>,
}

/// 启动时记一行用哪些浏览器内核、按什么顺序回退，部署后可以从日志确认。
fn log_engine_chain(chain: &[PoolConfig]) {
    let names: Vec<String> = chain
        .iter()
        .filter_map(|c| match &c.source {
            pagesift_cdp::BrowserSource::Executable(p) => Some(format!(
                "{}={}",
                c.engine.map_or("chrome", |k| k.name()),
                p.display()
            )),
            pagesift_cdp::BrowserSource::Remote(u) => Some(format!("remote={u}")),
            pagesift_cdp::BrowserSource::Auto => None,
        })
        .collect();
    if names.is_empty() {
        tracing::info!("no browser engine found; JavaScript rendering is unavailable");
    } else {
        tracing::info!(engines = %names.join(" -> "), "browser engines");
    }
}

/// 内核启动失败后多久再试。
const LAUNCH_RETRY: Duration = Duration::from_secs(600);

/// 没给 `timeout` 时一次渲染（含换内核）的总时限。
const DEFAULT_TIMEOUT: Duration = Duration::from_secs(30);
/// 剩余时间少于这么多就不再开始新的渲染尝试。
const MIN_ATTEMPT: Duration = Duration::from_millis(500);

/// 调用方给了 `timeout` 时，整个请求的截止时间。
fn request_deadline(opts: &ScrapeOptions, started: Instant) -> Option<Instant> {
    opts.timeout.map(|ms| started + Duration::from_millis(ms))
}

/// 渲染失败归到哪类错误：调用方要据此决定重试、退避还是放弃。
fn render_error_code(e: &pagesift_cdp::Error) -> ErrorCode {
    use pagesift_cdp::Error as E;
    match e {
        E::Timeout => ErrorCode::Timeout,
        E::Busy => ErrorCode::Busy,
        E::Blocked { .. } => ErrorCode::SsrfBlocked,
        E::Navigation(m)
            if m.contains("ERR_NAME_NOT_RESOLVED") || m.contains("ERR_NAME_RESOLUTION") =>
        {
            ErrorCode::DnsFailure
        }
        // 导航失败（net::ERR_*、跳到打不开的地址）都是网络层的结果
        E::Navigation(_) => ErrorCode::NetworkError,
        _ => ErrorCode::UpstreamError,
    }
}

/// 一个内核的渲染结果，已经提取好正文。
struct Rendered {
    /// 在内核链里的位置。
    index: usize,
    engine: &'static str,
    final_url: Url,
    status: Option<u16>,
    timings: pagesift_cdp::Timings,
    extract: Duration,
    doc: pagesift_content::Document,
    html: String,
}

/// A thin page carrying client code must still get a chance to render even
/// after static neighbors taught the learner that rendering is usually useless.
/// Data-only scripts do not invalidate that optimization. Unknown script types
/// are conservative: an extra render is preferable to silently losing content.
fn has_client_code(html: &str) -> bool {
    let dom = pagesift_html::Html::parse(html);
    dom.root().descendants().any(|node| {
        let Some(el) = node.as_element() else {
            return false;
        };
        if el
            .attrs()
            .any(|(name, _)| name.starts_with("on") && name.len() > 2)
        {
            return true;
        }
        if el.name() != "script" {
            return false;
        }
        let kind = el
            .attr("type")
            .unwrap_or("")
            .split(';')
            .next()
            .unwrap_or("")
            .trim();
        ![
            "application/json",
            "application/ld+json",
            "text/plain",
            "importmap",
            "speculationrules",
        ]
        .iter()
        .any(|data| kind.eq_ignore_ascii_case(data))
    })
}

/// 可能承载正文的 iframe：同源的 `src`（属性值和解析后的地址）或内联的 `srcdoc`
/// （属性值是文档本身，地址为 `None`）。跨源的 iframe 多是统计、广告、视频和地图嵌入，正文提取本来就不要；
/// 看不见的、空的和 `about:blank` 也不算。
fn content_iframes(html: &str, base: &Url) -> Vec<(String, Option<Url>)> {
    if !html
        .as_bytes()
        .windows(7)
        .any(|w| w[0] == b'<' && w[1..].eq_ignore_ascii_case(b"iframe"))
    {
        return Vec::new();
    }
    let doc = pagesift_html::Html::parse(html);
    doc.root()
        .descendants()
        .filter_map(|n| {
            let el = n
                .as_element()
                .filter(|e| e.tag() == pagesift_html::Tag::Iframe && !hidden_frame(e))?;
            if let Some(doc) = el.attr("srcdoc").filter(|d| !d.trim().is_empty()) {
                return Some((doc.to_string(), None));
            }
            let raw = el
                .attr("src")
                .map(str::trim)
                .filter(|s| !s.is_empty() && !s.starts_with("about:"))?;
            let u = base.join(raw).ok().filter(|u| {
                u.host_str() == base.host_str()
                    && u.port_or_known_default() == base.port_or_known_default()
            })?;
            Some((raw.to_string(), Some(u)))
        })
        .collect()
}

/// 每页最多用 HTTP 内联几个 iframe，多出来的不管（Chrome 系也有上限）。
const MAX_HTTP_IFRAMES: usize = 10;

/// 不渲染 iframe 的内核（Lightpanda）渲染完后，用 HTTP 抓同源 iframe 的文档，把 `<body>` 内容
/// 内联进去，格式和 Chrome 系内联的一样（`<div data-pagesift-iframe>`）。iframe 是页面的子资源，
/// 和浏览器的子请求一样不再单独排限速队列（页面的名额还在手里）。非 2xx 或不是 HTML 的
/// 响应直接丢掉：真浏览器也拿不到正文。`srcdoc` 的文档就在属性里，直接内联。返回没能处理的
/// iframe 数（网络错误、找不到标签），大于 0 时由调用方回退到渲染 iframe 的内核。
async fn inline_iframes_http(
    fetcher: &Fetcher,
    html: &mut String,
    frames: Vec<(String, Option<Url>)>,
    timeout: Duration,
) -> usize {
    let mut missing = 0;
    let mut fetches = Vec::new();
    for (raw, url) in frames {
        match url {
            Some(u) if fetches.len() < MAX_HTTP_IFRAMES => fetches.push((raw, u)),
            Some(_) => {}
            None => {
                let replacement = format!(
                    "<div data-pagesift-iframe=\"about:srcdoc\">{}</div>",
                    body_inner(&raw)
                );
                let found = replace_iframe(html, &replacement, |el| {
                    !hidden_frame(el) && el.attr("srcdoc") == Some(raw.as_str())
                });
                if !found {
                    tracing::debug!("iframe not inlined: srcdoc tag not found");
                    missing += 1;
                }
            }
        }
    }
    let bodies = futures_util::future::join_all(fetches.iter().map(|(_, u)| async move {
        let mut req = FetchRequest::new(u.as_str());
        req.timeout = Some(timeout);
        let mut resp = fetcher.fetch(req).await.ok()?;
        let html_like = resp
            .content_type()
            .is_none_or(|t| t.contains("html") || t.contains("xml"));
        Some(((200..300).contains(&resp.status) && html_like).then(|| resp.take_text().text))
    }))
    .await;
    for ((raw, url), body) in fetches.iter().zip(bodies) {
        let body = match body {
            Some(Some(b)) => b,
            Some(None) => {
                // 有响应但没有正文可内联：去掉这个 iframe，和真浏览器的结果一样是空的
                replace_iframe(html, "", |el| src_is(el, raw));
                continue;
            }
            None => {
                tracing::debug!(src = %url, "iframe not inlined: fetch failed");
                missing += 1;
                continue;
            }
        };
        let inner = body_inner(&body);
        let replacement = format!(
            "<div data-pagesift-iframe=\"{}\">{inner}</div>",
            url.as_str().replace('"', "&quot;")
        );
        if !replace_iframe(html, &replacement, |el| src_is(el, raw)) {
            tracing::debug!(src = %url, raw, "iframe not inlined: tag not found");
            missing += 1;
        }
    }
    missing
}

/// 文档 `<body>` 的内容；找不到 body 标签时返回整个文档。
fn body_inner(doc: &str) -> &str {
    let lower = doc.as_bytes();
    let find = |pat: &[u8], from: usize| {
        lower[from..]
            .windows(pat.len())
            .position(|w| w.eq_ignore_ascii_case(pat))
            .map(|p| p + from)
    };
    let Some(open) = find(b"<body", 0) else {
        return doc;
    };
    let Some(start) = lower[open..]
        .iter()
        .position(|&c| c == b'>')
        .map(|p| open + p + 1)
    else {
        return doc;
    };
    let end = find(b"</body", start).unwrap_or(doc.len());
    &doc[start..end]
}

fn src_is(el: &pagesift_html::Element<'_>, raw: &str) -> bool {
    el.attr("src").map(str::trim) == Some(raw)
}

/// 把第一个满足 `pred` 的 `<iframe>…</iframe>` 换成 `replacement`。
fn replace_iframe(
    html: &mut String,
    replacement: &str,
    pred: impl Fn(&pagesift_html::Element<'_>) -> bool,
) -> bool {
    let b = html.as_bytes();
    let mut i = 0;
    while let Some(off) = b[i..]
        .windows(7)
        .position(|w| w[0] == b'<' && w[1..].eq_ignore_ascii_case(b"iframe"))
    {
        let start = i + off;
        let Some(tag_end) = b[start..]
            .iter()
            .position(|&c| c == b'>')
            .map(|p| start + p + 1)
        else {
            return false;
        };
        i = tag_end;
        let tag = pagesift_html::Html::parse(&html[start..tag_end]);
        let hit = tag.root().descendants().any(|n| {
            n.as_element()
                .is_some_and(|e| e.tag() == pagesift_html::Tag::Iframe && pred(&e))
        });
        if !hit {
            continue;
        }
        let end = b[tag_end..]
            .windows(8)
            .position(|w| w.eq_ignore_ascii_case(b"</iframe"))
            .and_then(|p| {
                let close = tag_end + p;
                b[close..]
                    .iter()
                    .position(|&c| c == b'>')
                    .map(|q| close + q + 1)
            })
            .unwrap_or(tag_end);
        html.replace_range(start..end, replacement);
        return true;
    }
    false
}

/// 看不见的 iframe：统计像素、沙箱脚本、预取用的空框架，不会有正文。
fn hidden_frame(el: &pagesift_html::Element<'_>) -> bool {
    if el.attr("hidden").is_some()
        || el
            .attr("aria-hidden")
            .is_some_and(|v| v.eq_ignore_ascii_case("true"))
        || el
            .attr("role")
            .is_some_and(|v| v.eq_ignore_ascii_case("presentation"))
    {
        return true;
    }
    let tiny = |name| {
        el.attr(name).is_some_and(|v| {
            v.trim()
                .trim_end_matches("px")
                .parse::<f32>()
                .is_ok_and(|n| n <= 2.0)
        })
    };
    if tiny("width") || tiny("height") {
        return true;
    }
    el.attr("style").is_some_and(|s| {
        let s: String = s
            .chars()
            .filter(|c| !c.is_whitespace())
            .collect::<String>()
            .to_ascii_lowercase();
        s.contains("display:none")
            || s.contains("visibility:hidden")
            || s.contains("width:0")
            || s.contains("height:0")
    })
}

/// `chunking`：正文 Markdown 分块。参数格式见 [`pagesift_extract::ChunkStrategy`]（`maxLevel` 这类驼峰写法也认）。
pub(crate) fn chunk_markdown(
    md: &str,
    spec: &serde_json::Value,
) -> Result<Vec<pagesift_api::types::DocChunk>, ApiError> {
    // API 用驼峰，提取库用下划线：只改键名
    let snake = match spec {
        serde_json::Value::Object(m) => serde_json::Value::Object(
            m.iter()
                .map(|(k, v)| {
                    let mut out = String::new();
                    for c in k.chars() {
                        if c.is_ascii_uppercase() {
                            out.push('_');
                            out.push(c.to_ascii_lowercase());
                        } else {
                            out.push(c);
                        }
                    }
                    (out, v.clone())
                })
                .collect(),
        ),
        other => other.clone(),
    };
    let strategy: pagesift_extract::ChunkStrategy = serde_json::from_value(snake)
        .map_err(|e| ApiError::invalid(format!("invalid chunking: {e}")))?;
    let chunks = pagesift_extract::chunk(md, &strategy)
        .map_err(|e| ApiError::invalid(format!("invalid chunking: {e}")))?;
    Ok(chunks
        .into_iter()
        .map(|c| pagesift_api::types::DocChunk {
            text: c.text,
            heading_path: c.heading_path,
            start: c.start,
            end: c.end,
            tokens: c.tokens,
        })
        .collect())
}

/// `queryFilter`：只留下和查询相关的内容，统计写进 `metadata.pagesift.queryFilter`。
async fn apply_query_filter(
    doc: &mut Document,
    q: &pagesift_api::types::QueryFilter,
) -> Result<(), ApiError> {
    let Some(md) = doc.markdown.take() else {
        return Ok(());
    };
    let fo = pagesift_extract::QueryFilterOptions {
        threshold: q.threshold,
        relative: q.relative,
        top_k: q.top_k,
        max_tokens: q.max_tokens,
        min_tokens: q.min_tokens,
        headings: q.headings,
    };
    let query = q.query.clone();
    let (f, before) = cpu_task(move || {
        let f = pagesift_extract::filter_markdown_by_query(&md, &query, &fo);
        (f, pagesift_content::estimate_tokens(&md))
    })
    .await?;
    let ext = doc.metadata.pagesift.get_or_insert_with(Default::default);
    if f.kept == 0 && f.total > 0 {
        ext.warnings.push("query_filter_no_match".into());
    }
    ext.query_filter = Some(pagesift_api::types::QueryFilterStats {
        kept_blocks: f.kept,
        total_blocks: f.total,
        tokens_before: before,
        tokens_after: pagesift_content::estimate_tokens(&f.markdown),
    });
    doc.markdown = Some(f.markdown);
    Ok(())
}

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
        // 完整指纹（原始 HTML 的哈希、再渲染一遍纯文本做规范化哈希）约占提取 CPU 的一成，只在变化检测要用时算
        fingerprints: opts.wants("changeTracking"),
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

/// 正文 Markdown 的哈希总是给（Markdown 只有几 KB，几乎不花时间）；原始 HTML 和规范化纯文本的
/// 哈希只在请求变化检测时算（见 `process_html`），它们要整页再过一遍。
fn fill_fingerprints(ext: &mut PagesiftMeta, doc: &pagesift_content::Document) {
    match &doc.fingerprints {
        Some(f) => {
            ext.content_hash = Some(f.markdown.clone());
            ext.raw_hash = Some(f.raw_html.clone());
            ext.text_hash = Some(f.text.clone());
        }
        None => ext.content_hash = Some(pagesift_content::hash_hex(doc.markdown.as_bytes())),
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
    meta.author = m.author.clone();
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
        P::DnsFailure => ErrorCode::DnsFailure,
        P::NetworkError => ErrorCode::NetworkError,
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
