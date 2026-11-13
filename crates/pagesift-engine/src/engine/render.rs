//! 浏览器渲染：按内核链逐个尝试，取正文最完整的结果。

use super::*;

pub(super) use super::render_stats::LAUNCH_RETRY;

impl Scraper {
    #[allow(clippy::too_many_arguments)] // 渲染入口：请求参数、进度、HTTP 升级来的基线和出口，只在抓取流程里调用
    pub(super) async fn render(
        &self,
        url: &str,
        opts: &ScrapeOptions,
        mode: RobotsMode,
        progress: Progress,
        // `engine: auto` 从 HTTP 升级过来时，HTTP 正文的字符数（用来学这个站值不值得渲染）
        http_chars: Option<usize>,
        // `engine: auto` 的早停探针；显式 `engine: browser` 传 `None`
        early_stop: Option<pagesift_cdp::EarlyStop>,
        // 这次请求所用出口的代理：浏览器上下文也走它，只用支持按上下文设代理的 Chrome
        proxy: Option<String>,
    ) -> Result<Document, ApiError> {
        let Progress {
            started,
            mut timings,
            mut warnings,
        } = progress;
        // `keepSession`：只用能做 interact 的内核，渲染完不关页面
        let keep = super::interact::KEEP.try_with(Arc::clone).ok();
        let actions = pagesift_cdp::parse_actions(&serde_json::Value::Array(opts.actions.clone()))
            .map_err(|e| ApiError::invalid(e.to_string()))?;
        // 要动作或要保留页面：只用 Chrome，这些路径只在它上面验证过
        let chrome_only = keep.is_some() || !actions.is_empty() || self.itna;
        if chrome_only && !self.interact_available() {
            return Err(ApiError::unsupported(
                "actions and keepSession need Chrome: no Chrome or Chromium was found on this machine (run `pagesift doctor`)",
            ));
        }
        if !self.has_browser().await {
            let why = format!(
                "browser rendering is unavailable: {}",
                self.browser_unavailable_reason()
            );
            // 一个内核都没找到：这台机器不支持这个选项。找到了但起不来（例如以 root 运行时拒绝启动）：
            // 和启动后立即退出一样是上游错误（0.90 前者报 UPSTREAM_ERROR、后者报 UNSUPPORTED_OPTION，
            // 同一种故障的错误码取决于在哪一步失败）
            return Err(if self.browser_launch_failed() {
                ApiError::new(ErrorCode::UpstreamError, why)
            } else {
                ApiError::unsupported(why)
            });
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
        // 没给时限时用学到的渲染时限（先验 0.81 的 30 s，只收紧不放宽，见 render_stats）
        // 带动作的渲染耗时由调用方的动作决定，不进“这个站要渲染多久”的统计
        let learn_deadline = request_deadline(opts, started).is_none() && actions.is_empty();
        // 没给时限时，动作里写明的等待加在学到的时限上（总量已由动作校验限制）
        let action_wait: Duration = actions
            .iter()
            .map(|a| match a {
                pagesift_cdp::Action::Wait {
                    milliseconds: Some(ms),
                    ..
                } => Duration::from_millis(*ms),
                _ => Duration::ZERO,
            })
            .sum();
        // 截图和 PDF 要看到真实外观：字体也要加载
        let visual = actions.iter().any(|a| {
            matches!(
                a,
                pagesift_cdp::Action::Screenshot(_) | pagesift_cdp::Action::Pdf { .. }
            )
        });
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
            None => (
                acquire.await,
                Instant::now() + self.render_stats.default_timeout() + action_wait,
            ),
        };
        let render_start = Instant::now();
        let queued = permit.waited();
        timings
            .entry("queue".into())
            .and_modify(|total| *total += ms(queued))
            .or_insert_with(|| ms(queued));
        let queue_note = permit.robots_crawl_delay().then(|| permit.interval());
        let paced = !permit.interval().is_zero();
        let mut ropts = RenderOptions {
            network_budget: ITNA_NETWORK_BUDGET.try_with(Arc::clone).ok(),
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
                fonts: !visual,
                media: true,
                // 只取 HTML，图片不影响正文：src 属性还在，加载失败和离线时一样。以后支持截图时要放开。
                // 要动作或保留的页面还要点击和截图，图片要加载（影响布局）。
                images: !chrome_only,
            },
            proxy,
            actions,
            wait_until: wait_until(opts.respond_timing.as_deref()),
            stealth: self.itna,
            ..RenderOptions::default()
        };
        // A JS challenge may need several seconds before it reloads or writes
        // the final DOM. Interactive captchas are not waited for.
        if self.itna {
            ropts.challenge_wait = Duration::from_secs(8).min(ropts.timeout);
        }
        if let Ok(Some(session)) = ITNA_COOKIE_SESSION.try_with(Clone::clone) {
            let cookies = session
                .browser_cookies(&target)
                .map_err(|e| ApiError::invalid(e.to_string()))?;
            if !cookies.is_empty() {
                ropts.cookie_bridge = Some(pagesift_cdp::CookieBridge {
                    origin: target.origin().ascii_serialization(),
                    expected_chrome_major: self
                        .policy
                        .fetcher()
                        .config()
                        .browser_fingerprint
                        .and_then(|p| {
                            p.user_agent()
                                .split_once("Chrome/")?
                                .1
                                .split('.')
                                .next()?
                                .parse()
                                .ok()
                        }),
                    cookies: cookies
                        .into_iter()
                        .map(|c| pagesift_cdp::BridgeCookie {
                            name: c.name.clone(),
                            value: c.value.clone(),
                            path: c.path.clone(),
                            secure: c.secure,
                            http_only: c.http_only,
                            expires_at: c.expires_at,
                        })
                        .collect(),
                });
            }
        }

        // 按优先级逐个内核尝试：渲染失败、页面有 iframe 而内核不渲染 iframe、正文太少时换下一个。
        // 后面的内核都不可用或失败时，用前面拿到的结果。
        let mut best: Option<Rendered> = None;
        let mut last_err = None;
        // 拿到结果之后还失败了的内核：结果正文太少、又没有内核能接手时要告诉调用方
        let mut failed_after: Vec<&'static str> = Vec::new();
        // 换了内核时：换内核决策的族和换之前的质量分，下一个结果出来后记进 planner
        let mut pending_fallback: Option<(String, (Option<f64>, usize))> = None;
        let from = self
            .learner
            .start_engine(&host)
            .min(self.browsers.len().saturating_sub(1));
        // 只用 Chrome 时从头找，不用按主机学到的起点
        let from = if chrome_only { 0 } else { from };
        for i in from..self.browsers.len() {
            if ropts.proxy.is_some() && !proxy_capable(&self.browsers[i].0) {
                continue;
            }
            if chrome_only && !super::interact::interact_capable(&self.browsers[i].0) {
                continue;
            }
            if self.launch_failed[i]
                .lock()
                .unwrap()
                .is_some_and(|t| t.elapsed() < self.render_stats.launch_retry())
            {
                continue;
            }
            let Some(pool) = self.browser(i).await else {
                continue;
            };
            let engine = pool.engine();
            // 裸跳转页需要完整浏览器执行脚本再跟随跳转；Lightpanda 只会白等
            // 一轮网络静默，直接跳到完整内核。
            if early_stop.as_ref().is_some_and(|s| s.cap_navigation) && !engine.renders_iframes() {
                continue;
            }
            // 导航上限对所有内核生效；内容探针只给完整内核，避免 Lightpanda
            // 的薄结果把后面 Chrome 的机会提前掐掉。
            ropts.early_stop = early_stop.clone();
            ropts.early_stop_probe = engine.renders_iframes();
            // HTTP 正文为空的裸跳转页，耐心由 pagesift-cdp 按自己的先验学（0.81 这里另给的
            // "HTTP 耗时 × 20"在 cdp 里从来没被读到：baseline 为 0 时 cdp 只用它自己的式子）
            ropts.nav_patience = match early_stop.as_ref() {
                Some(stop) if stop.baseline_chars > 0 => self.render_stats.nav_patience(),
                _ => None,
            };
            ropts.timeout = deadline.saturating_duration_since(Instant::now());
            if ropts.timeout < self.render_stats.min_attempt(engine) {
                // 剩下的时间不够再试一个内核：有结果就用，没有就算超时
                last_err.get_or_insert(pagesift_cdp::Error::Timeout);
                break;
            }
            // 浏览器标签名额走统一准入门面；名额上限是各池当前容量之和，池内部仍各自排队
            let Ok(_tab) = tokio::time::timeout(
                ropts.timeout,
                self.render_stats.tab_permit(self.tab_capacity()),
            )
            .await
            else {
                last_err.get_or_insert(pagesift_cdp::Error::Busy);
                break;
            };
            ropts.timeout = deadline.saturating_duration_since(Instant::now());
            if let Some(budget) = &ropts.network_budget {
                budget.browser_started();
            }
            let rendered = match &keep {
                Some(slot) => pool.render_kept(url, &ropts).await.map(|(p, kept)| {
                    slot.put(kept, pool.clone());
                    p
                }),
                None => pool.render(url, &ropts).await,
            };
            let page = match rendered {
                Ok(p) => {
                    self.render_stats.note_launch_ok(i);
                    let t = &p.timings;
                    self.render_stats
                        .note_first_byte(engine, t.acquire + t.setup + t.navigation);
                    p
                }
                Err(e) => {
                    tracing::debug!(engine = engine.name(), url, "render failed: {e}");
                    if matches!(
                        e,
                        pagesift_cdp::Error::Launch(_) | pagesift_cdp::Error::Connect(_)
                    ) {
                        self.note_launch_failure(i, engine.name(), &e);
                        self.render_stats.note_launch_failure(i);
                    }
                    if best.is_some() {
                        failed_after.push(engine.name());
                    }
                    // 完整内核（渲染和 Chrome 一致）报的导航错误是网络层的结果（地址打不开、
                    // 被拦截），换内核也一样，不再往下试
                    let final_err =
                        engine.renders_iframes() && matches!(e, pagesift_cdp::Error::Navigation(_));
                    last_err = Some(e);
                    if final_err || self.itna {
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
                early_stopped,
                first_content,
                actions: outputs,
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
            let gained = http_chars.is_some_and(|n| text_weight(&doc.markdown) > n);
            if ropts.actions.is_empty() {
                self.render_stats.note_render(
                    page_timings.navigation,
                    page_timings.wait,
                    page_timings.total,
                    first_content,
                    gained,
                );
            }
            if let Some((family, before)) = pending_fallback.take() {
                let after = (
                    super::planner::quality(&doc, html.len()),
                    text_weight(&doc.markdown),
                );
                self.record_fallback(&family, before, after);
            }
            // 错误页正文少是正常的，换内核也一样。0.81 的规则（正文太少）是候选门，门内按学到的后验决定
            let thin = status.is_none_or(|s| s < 400)
                && text_weight(&doc.markdown) < RENDER_FALLBACK_MIN_CHARS
                && !engine.renders_iframes()
                && missing_iframes == 0
                && {
                    let family = super::planner::fallback_family(url, i);
                    let q = super::planner::quality(&doc, html.len());
                    let (go, _) = self.plan_fallback(&family, true, q, opts, started);
                    if go {
                        pending_fallback = Some((family, (q, text_weight(&doc.markdown))));
                    }
                    go
                };
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
                actions: (!ropts.actions.is_empty()).then(|| outputs.to_firecrawl_json()),
            };
            // 早停拿到的就是当前 DOM 的快照，不再换内核重试。
            if early_stopped {
                best = Some(r);
                break;
            }
            // 完整内核（moli、Chrome 系）渲染结果一致：正文少就是页面本身少，不再换内核
            if (missing_iframes == 0 && !thin) || engine.renders_iframes() {
                let mut r = r;
                // 内核间合并：前一个内核（不渲染 iframe 的）有完整内核没有的块时，
                // 按块合并，合并后的质量分不低于完整内核的结果才用
                if engine.renders_iframes()
                    && let Some(prev) = best.as_ref()
                {
                    merge_engines(&mut r.doc, r.html.len(), &prev.doc, prev.html.len());
                }
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
        if learn_deadline {
            // 到时限没拿到结果 = 截尾；拿到了 = 完成；别的失败与等多久无关，不计
            let timed_out =
                best.is_none() && matches!(last_err, Some(pagesift_cdp::Error::Timeout));
            if best.is_some() || timed_out {
                self.render_stats
                    .note_default_timeout(render_start.elapsed(), best.is_some());
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
        // 渲染有没有增益由调用方（`fetch.rs`）按质量分记进 planner
        let _ = http_chars;

        let t = &r.timings;
        // 礼貌限速让请求等得比渲染本身还久时才提醒（同 `fetch.rs`）
        if paced && queued > t.total {
            match queue_note {
                Some(interval) => warnings.push(format!(
                    "queue_wait: request waited {:.0} ms in the rate-limit/admission queue; robots.txt Crawl-delay applies with an effective {:.0} ms interval",
                    ms(queued),
                    ms(interval)
                )),
                None => warnings.push(format!(
                    "queue_wait: request waited {:.0} ms in the rate-limit/admission queue",
                    ms(queued)
                )),
            }
        }
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
        check_barrier(&r.doc, &r.html)?;
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
        Ok(rendered_document(
            r,
            url,
            opts,
            timings,
            warnings,
            started,
            self.store.as_ref(),
        ))
    }

    /// 保留页面上取到的结果 → 文档（`/interact`）：和渲染的结果走同一段组装。
    pub(super) async fn live_document(
        &self,
        page: RenderedPage,
        url: &str,
        opts: &ScrapeOptions,
        warnings: Vec<String>,
        started: Instant,
    ) -> Result<Document, ApiError> {
        let final_url = Url::parse(&page.final_url)
            .map_err(|e| ApiError::new(ErrorCode::UpstreamError, format!("page URL: {e}")))?;
        let t = Instant::now();
        let RenderedPage {
            html,
            status,
            content_type,
            timings: page_timings,
            ..
        } = page;
        let (doc, html) = process_html(html, &final_url, status, opts).await?;
        check_barrier(&doc, &html)?;
        let r = Rendered {
            index: 0,
            engine: pagesift_cdp::EngineKind::Chrome.name(),
            final_url,
            status,
            content_type,
            timings: page_timings,
            extract: t.elapsed(),
            doc,
            html,
            actions: None,
        };
        Ok(rendered_document(
            r,
            url,
            opts,
            BTreeMap::new(),
            warnings,
            started,
            self.store.as_ref(),
        ))
    }
}

/// 渲染结果组装成文档。
fn rendered_document(
    r: Rendered,
    url: &str,
    opts: &ScrapeOptions,
    timings: BTreeMap<String, f64>,
    mut warnings: Vec<String>,
    started: Instant,
    store: Option<&pagesift_store::Store>,
) -> Document {
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
            "video_metadata: no readable body text; used the page's embedded video metadata".into(),
        );
    }
    warnings.extend(r.doc.coverage.warnings());
    let raw_bytes = Some(thin_basis_bytes(&r.html, &r.doc.markdown));
    let video_fallback = r.doc.metadata.video_fallback;
    // 渲染结果的质量分：HTTP 结果按块合并时用（`fetch.rs`），内容层的覆盖率等特征到这里还在
    let quality = super::planner::quality(&r.doc, r.html.len());
    let graph_opts = crate::graph::effective_graph_options(opts);
    let graph_payload = graph_opts.as_ref().map(|g| {
        let dom = crate::graph::dom_links::extract_from_html_str(
            &r.html,
            &r.doc.metadata,
            r.doc.fingerprints.as_ref(),
            Some(r.final_url.as_str()),
        );
        let traces = if g.evidence || g.facts || g.conflicts {
            crate::graph::evidence::collect_structured_html_traces(
                &r.html,
                Some(r.final_url.as_str()),
            )
        } else {
            Vec::new()
        };
        let md_fallback = (!opts.wants("markdown")).then(|| r.doc.markdown.clone());
        (dom, traces, md_fallback)
    });
    let mut doc = document(r.doc, opts, r.html);
    doc.quality = quality;
    doc.actions = r.actions;
    let mut out = finish(
        doc,
        meta,
        ext,
        timings,
        warnings,
        raw_bytes,
        video_fallback,
        started,
    );
    if let (Some(gopts), Some((dom_data, traces, md_fallback))) = (graph_opts, graph_payload) {
        crate::graph::attach_html_graph(
            store,
            &gopts,
            &mut out,
            dom_data,
            traces,
            Vec::new(),
            false,
            &[],
            "browser",
            md_fallback,
        );
    }
    out
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

/// 调用方给了 `timeout` 时，整个请求的截止时间。
pub(super) fn request_deadline(opts: &ScrapeOptions, started: Instant) -> Option<Instant> {
    opts.timeout.map(|ms| started + Duration::from_millis(ms))
}

impl Scraper {
    /// 已经启动的各内核池当前的标签容量之和。
    fn tab_capacity(&self) -> usize {
        self.browsers
            .iter()
            .filter_map(|(_, cell)| cell.get().and_then(Option::as_ref))
            .map(|p| p.tab_capacity())
            .sum()
    }
}

/// 渲染失败归到哪类错误：调用方要据此决定重试、退避还是放弃。
/// 这个内核能不能让浏览器上下文走代理（只有 Chrome 支持 `createBrowserContext` 的 `proxyServer`）。
pub(super) fn proxy_capable(cfg: &pagesift_cdp::PoolConfig) -> bool {
    cfg.engine.unwrap_or(pagesift_cdp::EngineKind::Chrome) == pagesift_cdp::EngineKind::Chrome
}

impl Scraper {
    /// 出口 `proxy` 能不能用于渲染：内核链里有 Chrome，且浏览器能用这个代理（不是带认证的 SOCKS）。
    pub(super) fn can_render_via(&self, proxy: &str) -> bool {
        pagesift_cdp::BrowserProxy::parse(proxy).is_ok()
            && self.browsers.iter().any(|(c, _)| proxy_capable(c))
    }
}

pub(super) fn render_error_code(e: &pagesift_cdp::Error) -> ErrorCode {
    match e {
        pagesift_cdp::Error::Timeout => ErrorCode::Timeout,
        pagesift_cdp::Error::Busy => ErrorCode::Busy,
        pagesift_cdp::Error::Blocked { .. } => ErrorCode::SsrfBlocked,
        // 调用方写的动作在这个页面上做不到，重试也一样
        pagesift_cdp::Error::Action { .. } | pagesift_cdp::Error::InvalidOptions(_) => {
            ErrorCode::InvalidRequest
        }
        pagesift_cdp::Error::ExecuteJavascriptDisabled => ErrorCode::UnsupportedOption,
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

/// 把前一个内核结果里独有的块并进完整内核的结果：合并后的质量分不低于完整内核的才替换。
/// 质量分算不出来时不合并（0.81 的行为：直接用完整内核的结果）。
fn merge_engines(
    full: &mut pagesift_content::Document,
    full_html: usize,
    prev: &pagesift_content::Document,
    prev_html: usize,
) {
    let (Some(qf), Some(qp)) = (
        super::planner::quality(full, full_html),
        super::planner::quality(prev, prev_html),
    ) else {
        return;
    };
    let (md, from) = super::planner::merge_markdown(&full.markdown, qf, &prev.markdown, qp);
    // from[2]：只来自前一个内核的块数
    if from[2] == 0 {
        return;
    }
    let mut m = full.clone();
    m.markdown = md;
    m.plain_text = None;
    if super::planner::quality(&m, full_html).is_some_and(|qm| qm >= qf) {
        *full = m;
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
    /// `actions` 的产出（Firecrawl 的形状）；没有动作时为 `None`。
    actions: Option<serde_json::Value>,
}

/// `respondTiming`（Jina 的 `x-respond-timing`）→ 浏览器的就绪条件：
/// `visible-content` 到 DOMContentLoaded；`mutation-idle` 等 DOM 变化停下；`resource-idle`（默认）
/// 等脚本、XHR、文档类请求静默；`media-idle`、`network-idle` 先等 `load`（图片、字体、iframe），
/// 再等请求静默。`html` 不走浏览器（引擎直接用 HTTP），这里不会出现。
fn wait_until(t: Option<&str>) -> pagesift_cdp::WaitUntil {
    use pagesift_cdp::WaitUntil;
    match t {
        Some("visible-content") => WaitUntil::DomContentLoaded,
        Some("mutation-idle") => WaitUntil::MutationIdle,
        Some("media-idle" | "network-idle") => WaitUntil::LoadIdle,
        _ => WaitUntil::NetworkIdle,
    }
}
