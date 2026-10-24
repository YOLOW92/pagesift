use super::*;

impl Scraper {
    pub(super) async fn fetch_and_extract(
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
        let egress = opts.egress.as_deref().or_else(|| {
            if let Some(p) = opts.proxy.as_deref() {
                if !matches!(p, "basic" | "auto") {
                    return Some(p);
                }
            }
            None
        });
        let fetch = self
            .policy
            .fetch_held_via(fetch_request(url, opts)?, mode, egress);
        let (mut resp, permit) = match request_deadline(opts, started) {
            Some(d) => tokio::time::timeout_at(d.into(), fetch)
                .await
                .map_err(|_| ApiError::new(ErrorCode::Timeout, "request deadline passed"))?,
            None => fetch.await,
        }
        .map_err(policy_error)?;
        let queued = permit.waited();
        timings.insert("queue".into(), ms(queued));
        if queued >= Duration::from_millis(10) {
            if permit.robots_crawl_delay() {
                warnings.push(format!(
                    "request waited {:.0} ms in the rate-limit/admission queue; robots.txt Crawl-delay applies with an effective {:.0} ms interval",
                    ms(queued),
                    ms(permit.interval())
                ));
            } else {
                warnings.push(format!(
                    "request waited {:.0} ms in the rate-limit/admission queue",
                    ms(queued)
                ));
            }
        }
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
            proxy_used: egress.unwrap_or("basic").into(),
            ..Default::default()
        };
        if resp.not_modified {
            ext.timings = timings;
            ext.warnings = warnings;
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
                // HTTP extraction no longer needs its admission slots. Rendering
                // takes a fresh slot below; retaining both can deadlock a batch
                // when every per-host slot is held by an auto-upgrade.
                drop(permit);
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
            drop(permit);
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

    pub(super) async fn render(
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
            mut warnings,
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
        let (permit, deadline) = match request_deadline(opts, started) {
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
        let queued = permit.waited();
        timings
            .entry("queue".into())
            .and_modify(|total| *total += ms(queued))
            .or_insert_with(|| ms(queued));
        if queued >= Duration::from_millis(10) {
            if permit.robots_crawl_delay() {
                warnings.push(format!(
                    "request waited {:.0} ms in the rate-limit/admission queue; robots.txt Crawl-delay applies with an effective {:.0} ms interval",
                    ms(queued),
                    ms(permit.interval())
                ));
            } else {
                warnings.push(format!(
                    "request waited {:.0} ms in the rate-limit/admission queue",
                    ms(queued)
                ));
            }
        }
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

/// 正文提取是纯 CPU 计算，放到阻塞线程池里，不占异步运行时的线程。
/// HTML 移交给阻塞线程、处理完原样交还，省掉一次整页复制。
/// 到渲染为止已经记下的开始时间、各阶段耗时和警告。
pub(super) struct Progress {
    started: Instant,
    timings: BTreeMap<String, f64>,
    warnings: Vec<String>,
}

/// 启动时记一行用哪些浏览器内核、按什么顺序回退，部署后可以从日志确认。
pub(super) fn log_engine_chain(chain: &[PoolConfig]) {
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
pub(super) const LAUNCH_RETRY: Duration = Duration::from_secs(600);

/// 没给 `timeout` 时一次渲染（含换内核）的总时限。
pub(super) const DEFAULT_TIMEOUT: Duration = Duration::from_secs(30);
/// 剩余时间少于这么多就不再开始新的渲染尝试。
pub(super) const MIN_ATTEMPT: Duration = Duration::from_millis(500);

/// 调用方给了 `timeout` 时，整个请求的截止时间。
pub(super) fn request_deadline(opts: &ScrapeOptions, started: Instant) -> Option<Instant> {
    opts.timeout.map(|ms| started + Duration::from_millis(ms))
}

/// 渲染失败归到哪类错误：调用方要据此决定重试、退避还是放弃。
pub(super) fn render_error_code(e: &pagesift_cdp::Error) -> ErrorCode {
    match e {
        pagesift_cdp::Error::Timeout => ErrorCode::Timeout,
        pagesift_cdp::Error::Busy => ErrorCode::Busy,
        pagesift_cdp::Error::Blocked { .. } => ErrorCode::SsrfBlocked,
        pagesift_cdp::Error::Navigation(m) if m.contains("ERR_NAME") => ErrorCode::DnsFailure,
        pagesift_cdp::Error::Navigation(_) => ErrorCode::NetworkError,
        _ => ErrorCode::UpstreamError,
    }
}

/// 一个内核的渲染结果，已经提取好正文。
pub(super) struct Rendered {
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
