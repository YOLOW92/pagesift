use super::*;

impl Scraper {
    pub fn new(mut config: ScraperConfig) -> Result<Self, ScraperError> {
        cpu::install();
        let browser_private_network = config.fetcher.ssrf.allow_private_network;
        let browser_allow = browser_allow_list(&config.fetcher.ssrf)?;
        if config.itna && config.browser.args_override.is_none() {
            // Removes the Blink automation marker at the source. The per-page
            // script in pagesift-cdp covers remote browsers that ignore args.
            if let Some(arg) = config
                .browser
                .extra_args
                .iter_mut()
                .find(|a| a.starts_with("--disable-blink-features="))
            {
                if !arg.contains("AutomationControlled") {
                    arg.push_str(",AutomationControlled");
                }
            } else {
                config
                    .browser
                    .extra_args
                    .push("--disable-blink-features=AutomationControlled".into());
            }
        }
        let mut egress = Vec::new();
        for (name, proxy) in &config.egress {
            let mut fc = config.fetcher.clone();
            // 代理地址为空：`direct` 出口，与默认出口的网络设置相同
            if !proxy.is_empty() {
                fc.proxy = Some(proxy.clone());
            }
            let class = config.egress_classes.get(name).copied().unwrap_or_default();
            egress.push((
                name.clone(),
                class,
                Arc::new(Fetcher::new_egress(fc).map_err(ScraperError::Fetcher)?),
            ));
        }
        let fetcher = Arc::new(Fetcher::new(config.fetcher).map_err(ScraperError::Fetcher)?);
        let llm = config
            .llm
            .map(pagesift_llm::ChatClient::new)
            .transpose()
            .map_err(ScraperError::Llm)?;
        let rate = &config.policy.rate;
        let interact_limits = config.interact;
        let initial_conc = rate
            .global_ceiling
            .map_or(rate.global_concurrency, |c| rate.global_concurrency.min(c))
            .max(1);
        let crawl_slots = pagesift_policy::AdaptiveConcurrencyGate::new(initial_conc);
        let chain = pagesift_cdp::engine_chain_with_allow_list(
            &config.browser,
            browser_private_network,
            &browser_allow,
        );
        let sessions = Arc::new(super::interact::Sessions::new(
            interact_limits,
            super::interact::record_budget(&chain),
        ));
        Ok(Self {
            crawl_slots,
            itna: config.itna,
            identities: pagesift_policy::identity_pool::IdentityPool::new(
                pagesift_net::SessionConfig::default(),
                512,
                4,
            ),
            policy: Arc::new({
                // 网络层"继续等待"的价格由全局准入按负载发布（空闲时为 0，超时等于配置值）
                let wait_price = fetcher.config().wait_price.clone();
                let mut p = Policy::new(fetcher, config.policy);
                for (name, class, f) in egress {
                    p.add_egress_class(name, class, f);
                }
                p.limiter().publish_wait_price(wait_price);
                p
            }),
            store: config.store,
            audit: config.audit,
            hooks: config.hooks,
            secrets: config.secrets,
            llm,
            search: config.search,
            ocr: config.ocr,
            browsers: {
                // allow_ips 以 /32、/128 的形式一起交给 Lightpanda；allow_hosts 它不支持（cdp 侧会告警）
                log_engine_chain(&chain);
                chain.into_iter().map(|c| (c, OnceCell::new())).collect()
            },
            launch_failed: Vec::new(),
            launch_notes: Vec::new(),
            launch_broken: Default::default(),
            browser_private_network,
            browser_allow,
            learner: crate::learn::HostLearner::default(),
            planner: super::planner::RenderPlanner::default(),
            render_stats: super::render_stats::RenderStats::new(),
            pages: std::sync::OnceLock::new(),
            sessions,
        })
        .map(|mut s| {
            s.launch_failed = (0..s.browsers.len()).map(|_| Default::default()).collect();
            s.launch_notes = (0..s.browsers.len()).map(|_| Default::default()).collect();
            // 上次运行学到的渲染决策计数（没有存储或格式版本不同时从冷启动开始）
            if let Some(store) = &s.store {
                super::planner::load(&s.planner, store);
            }
            s
        })
    }

    /// 第 `i` 个内核的池；不存在或创建失败时返回 `None`。
    pub(super) async fn browser(&self, i: usize) -> Option<Arc<BrowserPool>> {
        let (cfg, cell) = self.browsers.get(i)?;
        cell.get_or_init(|| async {
            let guard: Arc<dyn RequestGuard> = if self.browser_private_network {
                Arc::new(SchemeGuard)
            } else if self.browser_allow.is_empty() {
                Arc::new(PrivateNetworkGuard)
            } else {
                Arc::new(AllowListGuard::new(self.browser_allow.clone()))
            };
            match BrowserPool::new(cfg.clone(), guard).await {
                Ok(pool) => Some(Arc::new(pool)),
                Err(e) => {
                    let engine = cfg.engine.map_or("browser", |k| k.name());
                    *self.launch_notes[i].lock().unwrap() = Some(format!("{engine}: {e}"));
                    if !matches!(e, pagesift_cdp::Error::NoBrowserFound) {
                        self.launch_broken
                            .store(true, std::sync::atomic::Ordering::Relaxed);
                    }
                    tracing::warn!("{engine} rendering unavailable: {e}");
                    None
                }
            }
        })
        .await
        .clone()
    }

    pub(super) async fn has_browser(&self) -> bool {
        for i in 0..self.browsers.len() {
            if self.browser(i).await.is_some() {
                return true;
            }
        }
        false
    }

    /// 有内核找到了浏览器、但创建失败（不是"一个都没找到"）。
    pub(super) fn browser_launch_failed(&self) -> bool {
        self.launch_broken
            .load(std::sync::atomic::Ordering::Relaxed)
    }

    /// 浏览器不可用时给调用方的说明：逐个内核列出启动失败原因；都还在尝试时退回“没找到”。
    pub(super) fn browser_unavailable_reason(&self) -> String {
        let notes: Vec<String> = self
            .launch_notes
            .iter()
            .filter_map(|n| n.lock().unwrap().clone())
            .collect();
        if notes.is_empty() {
            return "no browser engine found (run `pagesift doctor`)".into();
        }
        let root = notes.iter().any(|n| n.contains("as root"));
        let mut reason = format!(
            "no browser engine could start ({}); run `pagesift doctor` for details",
            notes.join("; ")
        );
        if root && !notes.iter().any(|n| n.contains("unprivileged user")) {
            reason.push_str(
                " — pagesift is running as root: the Chrome sandbox does not work as root and pagesift will not disable it; run as an unprivileged user",
            );
        }
        reason
    }

    /// 把内核链里的每个内核真的启动一次，逐个给出结果（`doctor` 用）：文件在不等于能启动，
    /// 启动即崩的兜底内核要说出来。
    pub async fn probe_engines(&self) -> Vec<EngineProbe> {
        let mut out = Vec::new();
        for i in 0..self.browsers.len() {
            let cfg = &self.browsers[i].0;
            let engine = cfg.engine.map_or("chrome", |k| k.name());
            let location = match &cfg.source {
                pagesift_cdp::BrowserSource::Executable(p) => p.display().to_string(),
                pagesift_cdp::BrowserSource::Remote(u) => u.to_string(),
                pagesift_cdp::BrowserSource::Auto => "auto".into(),
            };
            let result = match self.browser(i).await {
                Some(pool) => pool.probe().await.map_err(|e| e.to_string()),
                None => Err(self.launch_notes[i]
                    .lock()
                    .unwrap()
                    .clone()
                    .unwrap_or_else(|| "could not start".into())),
            };
            out.push(EngineProbe {
                engine,
                location,
                result,
            });
        }
        out
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
    pub(super) fn note_launch_failure(&self, i: usize, engine: &str, e: &pagesift_cdp::Error) {
        let first = self.launch_failed[i]
            .lock()
            .unwrap()
            .replace(Instant::now())
            .is_none();
        if first {
            let msg = e.to_string();
            *self.launch_notes[i].lock().unwrap() = Some(format!("{engine}: {msg}"));
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
        self.sessions.shutdown().await;
        if let Some(store) = &self.store
            && let Err(e) = super::planner::save(&self.planner, store)
        {
            tracing::warn!("saving the render planner failed: {e}");
        }
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

    /// 整站爬取或 map 实际忽略了 robots.txt 时写一条审计记录（没配置审计日志时什么都不做）。
    pub fn audit_robots_ignored(&self, kind: RequestKind, mode: RobotsMode, url: &str) {
        if mode == RobotsMode::Ignore
            && let Some(audit) = &self.audit
        {
            let request = match kind {
                RequestKind::Scrape => "scrape",
                RequestKind::Crawl => "crawl",
                RequestKind::Map => "map",
            };
            audit.record_robots_ignored(request, url, self.policy.robots_ignore_reason(kind));
        }
    }

    /// 审计日志（没配置时为 `None`）。
    pub fn audit(&self) -> Option<&Arc<crate::audit::Audit>> {
        self.audit.as_ref()
    }

    /// 配置的 LLM 客户端（多服务器网关跑 agent 时用）。
    pub fn llm_client(&self) -> Option<&pagesift_llm::ChatClient> {
        self.llm.as_ref()
    }

    /// 配了搜索后端。
    pub fn has_search(&self) -> bool {
        self.search.is_some()
    }

    pub fn policy(&self) -> &Policy {
        &self.policy
    }

    /// 网关模式：此后爬取里的页面交给 `runner` 执行（只能设一次，之后的调用返回 `false`）。
    pub fn route_pages(&self, runner: Arc<dyn crate::PageRunner>) -> bool {
        self.pages.set(runner).is_ok()
    }

    pub fn page_runner(&self) -> Option<&Arc<dyn crate::PageRunner>> {
        self.pages.get()
    }

    /// 渲染决策的计数表（多服务器工人上报统计、导入网关下发的先验用）。
    pub fn planner(&self) -> &super::planner::RenderPlanner {
        &self.planner
    }

    pub fn learner(&self) -> &crate::learn::HostLearner {
        &self.learner
    }

    /// 内核链的长度（换内核决策的族按内核序号区分）。
    pub fn engine_count(&self) -> usize {
        self.browsers.len()
    }

    pub fn network_stage_snapshots(&self) -> Vec<pagesift_net::StageSnapshot> {
        self.policy.fetcher().stage_snapshots()
    }

    pub fn transport_phase_snapshots(&self) -> Vec<pagesift_net::PhaseSnapshot> {
        self.policy.fetcher().phase_snapshots()
    }

    pub fn crawl_slots(&self) -> &pagesift_policy::AdaptiveConcurrencyGate {
        self.sync_crawl_slots();
        &self.crawl_slots
    }

    /// 当前的全局并发目标（限速器的 USL 控制器给出，冷启动等于配置的全局并发）。
    ///
    /// 多服务器网关上页面由工人执行：取在线工人报告的准入容量之和（至少 1），
    /// 爬取的在途上限和名额闸门都跟着它走。单主机没有装执行方，路径不变。
    pub fn concurrency(&self) -> usize {
        if let Some(n) = self.pages.get().and_then(|r| r.capacity()) {
            return n.max(1);
        }
        self.policy.limiter().global_limit()
    }

    /// 爬取里一页结束。并发目标由限速器在名额归还时按观测窗更新（事件驱动），
    /// 这里只把爬取闸门的上限同步过去；取代 0.81 每秒采样一次的运行时调优器。
    pub fn observe_crawl(&self, _queue_wait: Duration, _failed: bool) {
        self.sync_crawl_slots();
    }

    fn sync_crawl_slots(&self) {
        let target = self.concurrency();
        if self.crawl_slots.limit() != target {
            self.crawl_slots.set_limit(target);
        }
    }

    /// 按给定的 robots 模式抓一页；整站爬取和 map 用它，robots 模式由调用方按请求类型决定。
    /// 先查缓存（语义同 Firecrawl 的 maxAge、minAge、storeInCache），没命中再抓取并写入。
    /// `queryFilter`、LLM 格式和 `changeTracking` 都在缓存之外、基于正文 Markdown 做，
    /// 缓存里存的总是完整正文。
    pub async fn scrape_with(
        &self,
        url: &str,
        opts: &ScrapeOptions,
        mode: RobotsMode,
    ) -> Result<Document, ApiError> {
        self.scrape_audited("scrape", url, opts, mode).await
    }

    /// 抓一页并写审计记录。`kind` 区分单页抓取和爬取里的页面。
    pub async fn scrape_audited(
        &self,
        kind: &str,
        url: &str,
        opts: &ScrapeOptions,
        mode: RobotsMode,
    ) -> Result<Document, ApiError> {
        let Some(audit) = &self.audit else {
            return self.scrape_inner(Source::Url(url, mode), opts).await;
        };
        let started = Instant::now();
        let mut result = self.scrape_inner(Source::Url(url, mode), opts).await;
        let raw = result.as_ref().ok().and_then(|d| d.raw_html.as_deref());
        if mode == RobotsMode::Ignore {
            let request = if kind == "scrape" {
                RequestKind::Scrape
            } else {
                RequestKind::Crawl
            };
            let reason = self.policy.robots_ignore_reason(request);
            audit.record_scrape_ignoring_robots(
                kind,
                url,
                opts,
                &result,
                raw,
                started.elapsed(),
                reason,
            );
        } else {
            audit.record_scrape(kind, url, opts, &result, raw, started.elapsed());
        }
        // 为快照内部多要的 rawHtml 不交给调用方
        if !opts.wants("rawHtml")
            && let Ok(doc) = &mut result
        {
            doc.raw_html = None;
        }
        result
    }

    /// 单页抓取、爬取里的页面、上传文件解析共同的出口：结果有数据风险时填 `data.warning`。
    pub(super) async fn scrape_plain(
        &self,
        source: Source<'_>,
        opts: &ScrapeOptions,
    ) -> Result<Document, ApiError> {
        // 选择器写错了在抓取之前就报，不白抓一次
        tag_filter(opts)?;
        if opts.export_images {
            return self.scrape_exporting_images(source, opts).await;
        }
        let mut result = self.scrape_document(source, opts).await;
        if let Ok(doc) = &mut result {
            // 浏览器路径不带 `headers`：拿到的可能是未登录的页面，得说清楚
            if !opts.headers.is_empty()
                && let Some(meta) = &mut doc.metadata.pagesift
                && meta.engine.as_deref() == Some("browser")
            {
                meta.warnings.push(
                    "headers_not_sent: this page was rendered in a browser, which does not send the request's headers"
                        .into(),
                );
            }
            crate::notice::fill_warning(doc);
        }
        result
    }

    /// `exportImages`：内部要图片列表（调用方没要 `images` 格式时最后去掉），不读也不写结果缓存
    /// （缓存里的文档不带附件）；PDF、Office 在转换时导出，网页图片在这之后取回。
    async fn scrape_exporting_images(
        &self,
        source: Source<'_>,
        opts: &ScrapeOptions,
    ) -> Result<Document, ApiError> {
        let mode = match &source {
            Source::Url(_, mode) => *mode,
            Source::Upload(_) => RobotsMode::Respect,
        };
        let mut inner = opts.clone();
        if !inner.wants("images") {
            inner
                .formats
                .push(pagesift_api::types::Format::new("images"));
        }
        inner.max_age = Some(0);
        inner.store_in_cache = false;
        let mut result = self.scrape_document(source, &inner).await;
        if let Ok(doc) = &mut result {
            self.export_web_images(doc, mode).await;
            if !opts.wants("images") {
                doc.images = None;
            }
            crate::notice::fill_warning(doc);
        }
        result
    }

    /// 基础文档（抓取或上传文件解析）之上，再做 `queryFilter`、分块、LLM 格式和变化检测。
    pub(super) async fn scrape_content(
        &self,
        source: Source<'_>,
        opts: &ScrapeOptions,
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
            return self.base_document(source, opts).await;
        }
        let url = match &source {
            Source::Url(url, _) => Some(*url),
            Source::Upload(_) => None,
        };
        if tracking.is_some() && url.is_none() {
            return Err(ApiError::unsupported(
                "changeTracking needs a URL: it is not available for uploaded files",
            ));
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
        let mut doc = self.base_document(source, &inner).await?;
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
        if let (Some(t), Some(store), Some(url)) = (&tracking, &self.store, url) {
            self.track_changes(store, t, url, &mut doc).await?;
        }
        if !wants_md {
            doc.markdown = None;
        }
        Ok(doc)
    }

    async fn base_document(
        &self,
        source: Source<'_>,
        opts: &ScrapeOptions,
    ) -> Result<Document, ApiError> {
        match source {
            Source::Url(url, mode) => self.scrape_cached(url, opts, mode).await,
            Source::Upload(upload) => self.parse_upload(upload, opts).await,
        }
    }

    /// `changeTracking`：和上一次的正文比较，结果放进 `doc.change_tracking`。
    pub(super) async fn track_changes(
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
}

/// 把网络层的 SSRF 放行名单原样映射给浏览器子请求拦截器。
fn browser_allow_list(ssrf: &pagesift_net::SsrfPolicy) -> Result<NetworkAllowList, ScraperError> {
    let mut allow = NetworkAllowList::default();
    for host in &ssrf.allow_hosts {
        allow.allow_host(host.clone());
    }
    for ip in &ssrf.allow_ips {
        allow.allow_ip(*ip);
    }
    for cidr in &ssrf.allow_cidrs {
        let (net, prefix) = match cidr {
            pagesift_net::IpCidr::V4 { net, prefix } => (std::net::IpAddr::V4(*net), *prefix),
            pagesift_net::IpCidr::V6 { net, prefix } => (std::net::IpAddr::V6(*net), *prefix),
        };
        allow
            .allow_cidr_parts(net, prefix)
            .map_err(|e| ScraperError::Fetcher(pagesift_net::Error::Config(e)))?;
    }
    Ok(allow)
}
