use super::*;

impl Scraper {
    pub fn new(config: ScraperConfig) -> Result<Self, ScraperError> {
        let browser_private_network = config.fetcher.ssrf.allow_private_network;
        let browser_allow = browser_allow_list(&config.fetcher.ssrf)?;
        let mut egress = Vec::new();
        for (name, proxy) in &config.egress {
            let mut fc = config.fetcher.clone();
            fc.proxy = Some(proxy.clone());
            egress.push((
                name.clone(),
                Arc::new(Fetcher::new_egress(fc).map_err(ScraperError::Fetcher)?),
            ));
        }
        let fetcher = Arc::new(Fetcher::new(config.fetcher).map_err(ScraperError::Fetcher)?);
        let llm = config
            .llm
            .map(pagesift_llm::ChatClient::new)
            .transpose()
            .map_err(ScraperError::Llm)?;
        let initial_conc = config.policy.rate.global_concurrency.max(1);
        let runtime_tuner = config.adaptive_tuning.then(RuntimeAdaptiveTuner::new);
        let crawl_slots = pagesift_policy::adaptive::AdaptiveConcurrencyGate::new(initial_conc);
        let target_concurrency = AtomicUsize::new(initial_conc);
        Ok(Self {
            crawl_slots,
            target_concurrency,
            runtime_tuner,
            policy: Arc::new({
                let mut p = Policy::new(fetcher, config.policy);
                for (name, f) in egress {
                    p.add_egress(name, f);
                }
                p
            }),
            store: config.store,
            audit: config.audit,
            llm,
            search: config.search,
            browsers: {
                // allow_ips 以 /32、/128 的形式一起交给 Lightpanda；allow_hosts 它不支持（cdp 侧会告警）
                let chain = pagesift_cdp::engine_chain_with_allow_list(
                    &config.browser,
                    browser_private_network,
                    &browser_allow,
                );
                log_engine_chain(&chain);
                chain.into_iter().map(|c| (c, OnceCell::new())).collect()
            },
            launch_failed: Vec::new(),
            launch_notes: Vec::new(),
            browser_private_network,
            browser_allow,
            learner: crate::learn::HostLearner::default(),
        })
        .map(|mut s| {
            s.launch_failed = (0..s.browsers.len()).map(|_| Default::default()).collect();
            s.launch_notes = (0..s.browsers.len()).map(|_| Default::default()).collect();
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

    /// 审计日志（没配置时为 `None`）。
    pub fn audit(&self) -> Option<&Arc<crate::audit::Audit>> {
        self.audit.as_ref()
    }

    pub(crate) fn policy(&self) -> &Policy {
        &self.policy
    }

    pub fn network_stage_snapshots(&self) -> Vec<pagesift_net::StageSnapshot> {
        self.policy.fetcher().stage_snapshots()
    }

    pub fn transport_phase_snapshots(&self) -> Vec<pagesift_net::PhaseSnapshot> {
        self.policy.fetcher().phase_snapshots()
    }

    pub(crate) fn crawl_slots(&self) -> &pagesift_policy::adaptive::AdaptiveConcurrencyGate {
        &self.crawl_slots
    }

    /// Returns current active concurrency limit.
    pub fn concurrency(&self) -> usize {
        self.target_concurrency.load(Ordering::Relaxed)
    }

    /// Dynamically adjusts the global crawl concurrency limit at runtime.
    pub fn set_concurrency(&self, target: usize) {
        let target = target.max(1);
        self.crawl_slots.set_limit(target);
        self.target_concurrency.store(target, Ordering::Release);
    }

    /// Records one completed crawl and opportunistically runs the one-second
    /// adaptive control loop. `try_lock` keeps request completion wait-free when
    /// another worker is sampling telemetry.
    pub(crate) fn observe_crawl(&self, queue_wait: Duration, failed: bool) {
        let Some(tuner) = &self.runtime_tuner else {
            return;
        };
        tuner
            .engine
            .latencies
            .record(queue_wait.as_secs_f64() * 1_000.0);
        let completed = tuner.completed.fetch_add(1, Ordering::Relaxed) + 1;
        let failed_total = if failed {
            tuner.failed.fetch_add(1, Ordering::Relaxed) + 1
        } else {
            tuner.failed.load(Ordering::Relaxed)
        };

        let Ok(mut window) = tuner.window.try_lock() else {
            return;
        };
        let elapsed = window.sampled_at.elapsed();
        if elapsed < RuntimeAdaptiveTuner::SAMPLE_INTERVAL {
            return;
        }

        let delta_completed = completed.saturating_sub(window.completed);
        let delta_failed = failed_total.saturating_sub(window.failed);
        let qps = delta_completed as f64 / elapsed.as_secs_f64().max(0.001);
        let error_rate_pct = if delta_completed == 0 {
            0.0
        } else {
            delta_failed as f64 / delta_completed as f64 * 100.0
        };
        window.sampled_at = Instant::now();
        window.completed = completed;
        window.failed = failed_total;
        drop(window);

        let previous = self.concurrency();
        let knobs = tuner.engine.tick(qps, error_rate_pct, open_fd_count());
        self.set_concurrency(knobs.global_concurrency);
        if previous != knobs.global_concurrency {
            tracing::info!(
                previous,
                target = knobs.global_concurrency,
                qps,
                error_rate_pct,
                queue_p95_ms = tuner.engine.latencies.quantiles().1,
                state = ?tuner.engine.controller.state(),
                "adaptive crawl concurrency changed"
            );
        }
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
        self.scrape_audited("scrape", url, opts, mode).await
    }

    /// 抓一页并写审计记录。`kind` 区分单页抓取和爬取里的页面。
    pub(crate) async fn scrape_audited(
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
        audit.record_scrape(kind, url, opts, &result, raw, started.elapsed());
        // 为快照内部多要的 rawHtml 不交给调用方
        if !opts.wants("rawHtml")
            && let Ok(doc) = &mut result
        {
            doc.raw_html = None;
        }
        result
    }

    /// 单页抓取、爬取里的页面、上传文件解析共同的出口：结果有数据风险时填 `data.warning`。
    pub(super) async fn scrape_inner(
        &self,
        source: Source<'_>,
        opts: &ScrapeOptions,
    ) -> Result<Document, ApiError> {
        let mut result = self.scrape_document(source, opts).await;
        if let Ok(doc) = &mut result {
            crate::notice::fill_warning(doc);
        }
        result
    }

    /// 基础文档（抓取或上传文件解析）之上，再做 `queryFilter`、分块、LLM 格式和变化检测。
    async fn scrape_document(
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

    pub(super) async fn scrape_cached(
        &self,
        url: &str,
        opts: &ScrapeOptions,
        mode: RobotsMode,
    ) -> Result<Document, ApiError> {
        // 审计要存快照时内部多要一份 rawHtml；缓存键仍按调用方的参数算，写缓存前去掉
        let capture =
            !opts.wants("rawHtml") && self.audit.as_ref().is_some_and(|a| a.wants_raw_html());
        let with_raw = capture.then(|| {
            let mut o = opts.clone();
            o.formats.push(pagesift_api::types::Format::new("rawHtml"));
            o
        });
        let fetch_opts = with_raw.as_ref().unwrap_or(opts);
        let Some((store, key)) = self.cache_key(url, opts) else {
            return self.fetch_and_extract(url, fetch_opts, mode).await;
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
        let mut doc = self.fetch_and_extract(url, fetch_opts, mode).await?;
        doc.metadata.cache_state = Some("miss".into());
        let captured = if capture { doc.raw_html.take() } else { None };
        if policy.store_in_cache
            && let Ok(bytes) = serde_json::to_vec(&doc)
        {
            let r = tokio::task::spawn_blocking(move || store.cache().put(&key, &bytes)).await;
            if let Ok(Err(e)) = r {
                tracing::warn!("cache write failed: {e}");
            }
        }
        if captured.is_some() {
            doc.raw_html = captured;
        }
        Ok(doc)
    }

    /// 缓存键：URL 加上影响结果的参数。带自定义请求头、actions 或条件请求时不走缓存（和 Firecrawl 一致）。
    pub(super) fn cache_key(
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
        let mut key = serde_json::json!([
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
        // PDF 的 parsers 改变结果；没给时不加这一项，已有的缓存键不变
        if let (Some(p), Some(items)) = (&opts.parsers, key.as_array_mut()) {
            items.push(p.clone());
        }
        Some((store, key.to_string().into_bytes()))
    }
}

fn open_fd_count() -> usize {
    std::fs::read_dir("/proc/self/fd")
        .map(|entries| entries.count())
        .unwrap_or(0)
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
