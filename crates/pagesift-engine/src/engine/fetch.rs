use super::*;

impl Scraper {
    /// 在一个出口上抓取并提取：`egress` 是命名出口或代理地址，`None` 为默认出口。
    /// 出口怎么选见 `egress_route.rs`。
    pub(super) async fn fetch_and_extract_via(
        &self,
        url: &str,
        opts: &ScrapeOptions,
        mode: RobotsMode,
        egress: Option<&str>,
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
            // 动作只能在浏览器里做（`engine: http` 在下面报错）
            _ if !opts.actions.is_empty() => EngineChoice::Browser,
            // respondTiming html：只要原始 HTML，不执行脚本
            _ if opts.respond_timing.as_deref() == Some("html") => EngineChoice::Http,
            _ => EngineChoice::Auto,
        };
        // 校验已经拦过；这里防的是规则（`[[rules]] engine`）把引擎改成 HTTP：动作不能被悄悄跳过
        if choice == EngineChoice::Http && !opts.actions.is_empty() {
            return Err(ApiError::invalid(
                "actions run in a browser, but this request was set to the HTTP engine",
            ));
        }
        let mut timings = BTreeMap::new();
        let mut warnings = Vec::new();

        // 出口的代理：浏览器上下文也走它（只有 Chrome 能做到；默认出口为 `None`）
        let proxy = egress.and_then(|e| self.policy.egress_proxy(e));
        if let Some(name) = egress
            && proxy.is_none()
            && !self.policy.has_egress(name)
        {
            return Err(ApiError::invalid(format!("unknown egress `{name}`")));
        }
        let render_blocked = proxy.as_deref().is_some_and(|p| !self.can_render_via(p));
        if choice == EngineChoice::Browser && render_blocked {
            return Err(ApiError::unsupported(
                "this egress cannot be used for browser rendering (needs Chrome and an HTTP proxy or a SOCKS proxy without credentials)",
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
                    None,
                    proxy,
                )
                .await;
        }

        // 并发名额一直拿到提取结束：已下载、待提取的页面数不超过全局并发，机器忙时内存不会堆积
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
        // 礼貌限速（请求间隔、Crawl-delay）让请求等得比抓取本身还久时才提醒；取代 0.81 的固定 10 ms。
        // 并发名额上的短暂排队记在 `timings.queue` 里，不是需要调用方处理的问题，不报
        if !permit.interval().is_zero() && queued > resp.timings.total {
            if permit.robots_crawl_delay() {
                warnings.push(format!(
                    "queue_wait: request waited {:.0} ms in the rate-limit/admission queue; robots.txt Crawl-delay applies with an effective {:.0} ms interval",
                    ms(queued),
                    ms(permit.interval())
                ));
            } else {
                warnings.push(format!(
                    "queue_wait: request waited {:.0} ms in the rate-limit/admission queue",
                    ms(queued)
                ));
            }
        }
        // Keep the admission permit optional: auto-render releases it before
        // handing the page to the browser, but if the browser upgrade times out
        // and we fall back to HTTP, the permit is already gone and the rest of
        // the HTTP path must not try to drop it twice.
        let mut permit = Some(permit);
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
            browser_fingerprint: resp.browser_fingerprint.map(|p| p.as_str().to_string()),
            ..Default::default()
        };
        // 网络层的解码提示（多层压缩、声明与实际不符、部分解压失败）原样带出去
        warnings.extend(resp.warnings.iter().cloned());
        if resp.truncated {
            warnings.push(
                "body_truncated: response body exceeded the size limit and was truncated".into(),
            );
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
            if opts.if_none_match.is_none() && opts.if_modified_since.is_none() {
                // 没发条件请求却拿到 304：服务器行为异常，这里也没有缓存可用。
                warnings.push(
                    "not_modified: HTTP 304 for an unconditional request; no content was returned"
                        .into(),
                );
            }
            ext.timings = timings;
            ext.warnings = warnings;
            meta.pagesift = Some(ext);
            return Ok(Document {
                metadata: meta,
                ..Default::default()
            });
        }

        // Word、Excel、PowerPoint、EPUB、CSV：内容、Content-Type、网址的扩展名一起判断
        if let Some(format) = office_format(&resp.body, resp.final_url.path(), resp.content_type())
        {
            let progress = Progress {
                started,
                timings,
                warnings,
            };
            let deadline = request_deadline(opts, started);
            let input = office::OfficeInput {
                content_type: resp.content_type().map(str::to_owned),
                bytes: resp.body,
                format,
            };
            return self
                .office_document(input, opts, (meta, ext), progress, deadline)
                .await;
        }

        if let Some(kind) = unsupported_content_type(resp.content_type(), &resp.body) {
            return Err(ApiError::unsupported(format!(
                "unsupported content type `{kind}`: pagesift extracts HTML/text; use `pagesift download` or `media-extract` for binary media"
            )));
        }

        if is_pdf(&resp.body, resp.content_type()) {
            let progress = Progress {
                started,
                timings,
                warnings,
            };
            return self
                .pdf_document(
                    resp.body,
                    opts,
                    (meta, ext),
                    progress,
                    request_deadline(opts, started),
                )
                .await;
        }

        if let Some(markdown) = json_markdown(resp.content_type(), &resp.body) {
            return Ok(finish(
                Document {
                    markdown: opts.wants("markdown").then_some(markdown),
                    ..Default::default()
                },
                meta,
                ext,
                timings,
                warnings,
                None,
                false,
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
        let t = Instant::now();
        let parse_stage = self
            .policy
            .fetcher()
            .acquire_stage(pagesift_net::NetworkStage::Parse)
            .await;
        let auto = choice == EngineChoice::Auto;
        // 推测执行：正文到手、还没提取时，按学到的"需要渲染"概率和浏览器影子价格决定是否同时开浏览器
        let body_bytes = resp.body.len();
        let mut spec_step = None;
        let mut speculative = None;
        let spec_baseline = Arc::new(std::sync::atomic::AtomicU64::new(
            super::speculate::BASELINE_UNKNOWN,
        ));
        if auto && !render_blocked {
            let (go, step) = self.plan_speculate(url, &resp.body);
            if go && self.has_browser().await {
                let early_stop = pagesift_cdp::EarlyStop {
                    // 还不知道 HTTP 正文有多少：按 HTTP 为空跑（不设导航上限、不早停），提取完再给探针基线
                    baseline_chars: 0,
                    cap_navigation: false,
                    http_elapsed: resp.timings.total,
                    after: None,
                    probe: super::speculate::render_probe(
                        opts,
                        resp.final_url.clone(),
                        spec_baseline.clone(),
                    ),
                };
                let progress = Progress {
                    started,
                    timings: timings.clone(),
                    warnings: warnings.clone(),
                };
                let fut = Box::pin(self.render(
                    url,
                    opts,
                    mode,
                    progress,
                    None,
                    Some(early_stop),
                    proxy.clone(),
                ));
                speculative = Some((Instant::now(), warnings.len(), fut));
            }
            spec_step = opts.explain.then_some(step);
        }
        let decode = resp.take_text_job();
        let extract = extract_fetched(decode, &resp.final_url, Some(resp.status), opts, auto);
        // 推测出去的渲染和提取一起推进；渲染先完成时把结果留着
        let mut spec_done = None;
        let extracted = match speculative.as_mut() {
            None => extract.await,
            Some((_, _, fut)) => {
                tokio::pin!(extract);
                loop {
                    tokio::select! {
                        r = &mut extract => break r,
                        r = fut.as_mut(), if spec_done.is_none() => spec_done = Some(r),
                    }
                }
            }
        };
        let fetched = match extracted {
            Ok(result) => {
                parse_stage.finish(pagesift_net::StageOutcome::Success);
                result
            }
            Err(error) => {
                parse_stage.finish(pagesift_net::StageOutcome::DeterministicFailure);
                return Err(error);
            }
        };
        let Fetched {
            doc,
            html,
            decode_warning,
            http_chars,
            render: hints,
            embedded,
        } = fetched;
        if let Some(warning) = decode_warning {
            warnings.push(warning);
        }
        timings.insert("extract".into(), ms(t.elapsed()));
        if auto {
            self.render_stats.note_extract(body_bytes, t.elapsed());
        }
        spec_baseline.store(http_chars as u64, std::sync::atomic::Ordering::Relaxed);
        if doc.metadata.video_fallback {
            warnings.push(
                "video_metadata: no readable body text; used the page's embedded video metadata"
                    .into(),
            );
        }
        // 拦截页如实报告，不升级到浏览器：用真浏览器去过挑战页就是绕过
        check_barrier(&doc)?;

        // 短页面不一定需要浏览器：没有脚本、HTML 本身也很小的静态页，渲染只会白抓一次
        // （crawl 时就是种子页被请求两次）。真正需要的是带客户端代码、或 HTML 很大却几乎
        // 没抽出文字的页面。这几项在提取的 CPU 任务里已经算好（`hints`）。
        // `explain`：每一步决策的依据，写进 `metadata.pagesift.plan`
        let (embedded_md, embedded_step) = embedded.unzip();
        let mut plan: Vec<pagesift_api::types::PlanStep> = spec_step
            .into_iter()
            .chain(embedded_step.filter(|_| opts.explain))
            .collect();
        if let Some(hints) = hints.filter(|h| h.worth) {
            let (go, step) = if render_blocked {
                (true, Default::default())
            } else {
                self.plan_render(url, &doc, html.len(), &hints, opts, started)
            };
            if opts.explain && !render_blocked {
                plan.push(step);
            }
            if render_blocked {
                // 浏览器用不了这个出口（没有 Chrome，或带认证的 SOCKS 代理）：换个出口渲染，地址就不一致了
                warnings.push("render_skipped_egress".into());
            } else if !go {
                warnings.push("render_skipped_learned".into());
            } else if self.has_browser().await {
                // HTTP extraction no longer needs its admission slots. Rendering
                // takes a fresh slot below; retaining both can deadlock a batch
                // when every per-host slot is held by an auto-upgrade.
                permit.take();
                // Auto-upgrade is opportunistic: the HTTP document is already a
                // valid result. The probe compares the *extracted* content of
                // the live DOM with what HTTP already produced, so navigation
                // bars and ad text cannot masquerade as a gain. The budget and
                // probe time come from observed render distributions, not from
                // fixed seconds.
                let budget = self.render_stats.budget();
                let (render, spec) = match speculative.take() {
                    // 推测出去的那一路接着用：已经跑了这么久，预算里扣掉
                    Some((at, warned, fut)) => (fut, Some((at, warned))),
                    None => {
                        spec_baseline
                            .store(http_chars as u64, std::sync::atomic::Ordering::Relaxed);
                        let early_stop = pagesift_cdp::EarlyStop {
                            baseline_chars: http_chars as u64,
                            cap_navigation: hints.client_redirect,
                            http_elapsed: resp.timings.total,
                            after: self.render_stats.early_stop_after(),
                            probe: super::speculate::render_probe(
                                opts,
                                resp.final_url.clone(),
                                spec_baseline.clone(),
                            ),
                        };
                        let progress = Progress {
                            started,
                            timings: timings.clone(),
                            warnings: warnings.clone(),
                        };
                        let fut = Box::pin(self.render(
                            url,
                            opts,
                            mode,
                            progress,
                            Some(http_chars),
                            Some(early_stop),
                            proxy.clone(),
                        ));
                        (fut, None)
                    }
                };
                let budget =
                    budget.map(|b| spec.map_or(b, |(at, _)| b.saturating_sub(at.elapsed())));
                let outcome = match spec_done.take() {
                    Some(done) => Some(done),
                    None => match budget {
                        Some(budget) => match tokio::time::timeout(budget, render).await {
                            Ok(result) => Some(result),
                            Err(_) => {
                                warnings.push(format!(
                                    "render_timeout_http_fallback: browser exceeded the learned {:.0} ms budget",
                                    budget.as_millis()
                                ));
                                None
                            }
                        },
                        None => Some(render.await),
                    },
                };
                // 推测的那一路是在提取之前出发的：补上之后才有的 HTTP 侧耗时和警告，与不推测时一致
                let outcome = match (outcome, spec) {
                    (Some(Ok(mut rendered)), Some((_, warned))) => {
                        let ext = rendered
                            .metadata
                            .pagesift
                            .get_or_insert_with(Default::default);
                        for (k, v) in &timings {
                            ext.timings.entry(k.clone()).or_insert(*v);
                        }
                        let late = warnings[warned.min(warnings.len())..].to_vec();
                        let at = warned.min(ext.warnings.len());
                        ext.warnings.splice(at..at, late);
                        Some(Ok(rendered))
                    }
                    (o, _) => o,
                };
                if let Some(outcome) = outcome {
                    match outcome {
                        Ok(rendered) => {
                            let (better, step) = self.adopt_render(
                                url,
                                &doc,
                                html.len(),
                                hints.client_code,
                                http_chars,
                                rendered,
                            );
                            if opts.explain {
                                plan.push(step);
                            }
                            if let Some(mut better) = better {
                                if opts.explain {
                                    better
                                        .metadata
                                        .pagesift
                                        .get_or_insert_with(Default::default)
                                        .plan =
                                        Some(pagesift_api::types::PlanTrace { steps: plan });
                                }
                                return Ok(better);
                            }
                            // 渲染没有补到正文：留 HTTP 的结果，占内存也少
                            warnings.push("render_no_gain_http_fallback".into());
                        }
                        Err(e) => {
                            // A failed upgrade is not a failed scrape: keep the
                            // HTTP result and say why the browser was not used.
                            warnings.push(format!("render_failed_http_fallback: {}", e.message));
                        }
                    }
                }
            } else {
                warnings.push(format!(
                    "render_unavailable: {}",
                    self.browser_unavailable_reason()
                ));
            }
        }

        // 推测出去、最后没用上的渲染：现在就取消，归还标签
        drop(speculative.take());
        let mut navigation = navigation;
        if opts.discover_scripts {
            // Release the document's admission permit before dependency fetches:
            // concurrent root pages must not occupy every per-host slot while
            // waiting on their own scripts.
            permit.take();
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
        let mut doc = doc;
        if let Some(md) = embedded_md.flatten() {
            doc.markdown = md;
            doc.plain_text = None;
        }
        fill_metadata(&mut meta, &doc.metadata);
        fill_structured(&mut ext, &doc);
        fill_fingerprints(&mut ext, &doc);
        if opts.explain {
            ext.plan = Some(pagesift_api::types::PlanTrace { steps: plan });
        }
        warnings.extend(doc.coverage.warnings());
        let raw_bytes = Some(thin_basis_bytes(&html, &doc.markdown));
        let video_fallback = doc.metadata.video_fallback;
        let mut result = document(doc, opts, html);
        if let Some(links) = result.links.as_mut() {
            for link in navigation {
                if !links.contains(&link) {
                    links.push(link);
                }
            }
        }
        Ok(finish(
            result,
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
