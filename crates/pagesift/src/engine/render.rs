//! 浏览器渲染：按内核链逐个尝试，取正文最完整的结果。

use super::*;

impl Scraper {
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
            return Err(ApiError::unsupported(format!(
                "browser rendering is unavailable: {}",
                self.browser_unavailable_reason()
            )));
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
        // 拿到结果之后还失败了的内核：结果正文太少、又没有内核能接手时要告诉调用方
        let mut failed_after: Vec<&'static str> = Vec::new();
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
                    if best.is_some() {
                        failed_after.push(engine.name());
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
                content_type,
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
                content_type,
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
        if let Some(kind) = unsupported_content_type(r.content_type.as_deref(), r.html.as_bytes()) {
            return Err(ApiError::unsupported(format!(
                "unsupported content type `{kind}`: pagesift extracts HTML/text; use `pagesift download` or `media-extract` for binary media"
            )));
        }
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
        // 前一个内核只渲染出壳（正文太少），后面能接手的内核都失败了：照常返回，但要说出来
        let chars = text_weight(&r.doc.markdown);
        if !failed_after.is_empty()
            && r.status.is_none_or(|s| s < 400)
            && chars < RENDER_FALLBACK_MIN_CHARS
        {
            warnings.push(format!(
                "render_incomplete: {} rendered only {chars} characters and {} could not render the page; the content may not have loaded",
                r.engine,
                failed_after.join(", ")
            ));
        }
        let mut meta = Metadata {
            url: Some(r.final_url.to_string()),
            source_url: Some(url.to_string()),
            status_code: r.status.unwrap_or(200),
            content_type: r.content_type.clone(),
            proxy_used: "basic".into(),
            ..Default::default()
        };
        fill_metadata(&mut meta, &r.doc.metadata);
        let mut ext = PagesiftMeta {
            engine: Some("browser".into()),
            browser_engine: Some(r.engine.into()),
            ..Default::default()
        };
        fill_structured(&mut ext, &r.doc);
        fill_fingerprints(&mut ext, &r.doc);
        if r.doc.metadata.video_fallback {
            warnings.push(
                "video_metadata: no readable body text; used the page's embedded video metadata"
                    .into(),
            );
        }
        warnings.extend(r.doc.coverage.warnings());
        let raw_bytes = Some(thin_basis_bytes(&r.html, &r.doc.markdown));
        let video_fallback = r.doc.metadata.video_fallback;
        Ok(finish(
            document(r.doc, opts, r.html),
            meta,
            ext,
            timings,
            warnings,
            raw_bytes,
            video_fallback,
            started,
        ))
    }
}

/// 正文提取是纯 CPU 计算，放到阻塞线程池里，不占异步运行时的线程。
/// HTML 移交给阻塞线程、处理完原样交还，省掉一次整页复制。
/// 到渲染为止已经记下的开始时间、各阶段耗时和警告。
pub(super) struct Progress {
    pub(super) started: Instant,
    pub(super) timings: BTreeMap<String, f64>,
    pub(super) warnings: Vec<String>,
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
        pagesift_cdp::Error::Navigation(m) if m.contains("ERR_CERT_DATE_INVALID") => {
            ErrorCode::TlsCertExpired
        }
        pagesift_cdp::Error::Navigation(m) if m.contains("ERR_CERT_COMMON_NAME_INVALID") => {
            ErrorCode::TlsHostnameMismatch
        }
        pagesift_cdp::Error::Navigation(m) if m.contains("ERR_CERT_AUTHORITY_INVALID") => {
            ErrorCode::TlsUntrustedRoot
        }
        pagesift_cdp::Error::Navigation(m) if m.contains("ERR_CERT") => ErrorCode::TlsError,
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
    content_type: Option<String>,
    timings: pagesift_cdp::Timings,
    extract: Duration,
    doc: pagesift_content::Document,
    html: String,
}
