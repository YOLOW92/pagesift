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
            opts.proxy
                .as_deref()
                .filter(|p| !matches!(*p, "basic" | "auto"))
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
        // 网络层的解码提示（多层压缩、声明与实际不符、部分解压失败）原样带出去
        warnings.extend(resp.warnings.iter().cloned());
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
        let decoded = resp.take_text();
        if let Some(warning) = decoded.warning() {
            warnings.push(warning);
        }
        let html = decoded.text;
        let t = Instant::now();
        let parse_stage = self
            .policy
            .fetcher()
            .acquire_stage(pagesift_net::NetworkStage::Parse)
            .await;
        let (doc, html) = match process_html(html, &resp.final_url, Some(resp.status), opts).await {
            Ok(result) => {
                parse_stage.finish(pagesift_net::StageOutcome::Success);
                result
            }
            Err(error) => {
                parse_stage.finish(pagesift_net::StageOutcome::DeterministicFailure);
                return Err(error);
            }
        };
        timings.insert("extract".into(), ms(t.elapsed()));
        if doc.metadata.video_fallback {
            warnings.push(
                "video_metadata: no readable body text; used the page's embedded video metadata"
                    .into(),
            );
        }
        // 拦截页如实报告，不升级到浏览器：用真浏览器去过挑战页就是绕过
        check_barrier(&doc)?;

        let http_chars = text_weight(&doc.markdown);
        // 短页面不一定需要浏览器：没有脚本、HTML 本身也很小的静态页，渲染只会白抓一次
        // （crawl 时就是种子页被请求两次）。真正需要的是带客户端代码、或 HTML 很大却几乎
        // 没抽出文字的页面。
        let worth_rendering = html.len() >= 4096 || has_script_tag(&html);
        if choice == EngineChoice::Auto && needs_render(&doc) && worth_rendering {
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
                warnings.push(format!(
                    "render_unavailable: {}",
                    self.browser_unavailable_reason()
                ));
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
        fill_structured(&mut ext, &doc);
        fill_fingerprints(&mut ext, &doc);
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

/// 不解析 DOM，只扫一遍字节：判断"值不值得为短页面再开一次浏览器"够用了。
fn has_script_tag(html: &str) -> bool {
    html.as_bytes()
        .windows(7)
        .any(|w| w.eq_ignore_ascii_case(b"<script"))
}
