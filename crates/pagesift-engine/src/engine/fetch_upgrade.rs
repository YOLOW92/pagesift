//! Opportunistic and fallback browser upgrade path for auto-engine HTML scrapes.

use super::*;

pub(super) type SpecRenderFut<'a> =
    std::pin::Pin<Box<dyn std::future::Future<Output = Result<Document, ApiError>> + Send + 'a>>;

pub(super) struct UpgradeInputs<'a> {
    pub url: &'a str,
    pub final_url: &'a Url,
    pub opts: &'a ScrapeOptions,
    pub render_opts: &'a ScrapeOptions,
    pub mode: RobotsMode,
    pub started: Instant,
    pub http_elapsed: Duration,
    pub http_status: u16,
    pub timings: &'a BTreeMap<String, f64>,
    pub warnings: &'a mut Vec<String>,
    pub proxy: &'a Option<String>,
    pub proxy_label: &'a str,
    pub render_blocked: bool,
    pub speculative: Option<(Instant, usize, SpecRenderFut<'a>)>,
    pub spec_done: Option<Result<Document, ApiError>>,
    pub spec_baseline: &'a Arc<std::sync::atomic::AtomicU64>,
    pub permit: &'a mut Option<pagesift_policy::Permit>,
    pub doc: &'a pagesift_content::Document,
    pub html_len: usize,
    pub http_chars: usize,
    pub hints: Option<RenderHints>,
    pub plan: &'a mut Vec<pagesift_api::types::PlanStep>,
    pub header_canonical: &'a Option<String>,
    pub header_alternates: &'a [(String, String)],
    pub graph_opts: &'a Option<pagesift_graph::GraphOptions>,
    pub graph_tenant: &'a Option<String>,
    pub redirect_hops: &'a [(String, String, u16, pagesift_graph::RedirectKind)],
}

impl Scraper {
    /// Attempt to upgrade a thin HTTP HTML result using speculative or newly started browser rendering.
    pub(super) async fn maybe_upgrade_to_browser<'a>(
        &self,
        in_: UpgradeInputs<'a>,
    ) -> Option<Document> {
        let mut speculative = in_.speculative;
        let mut spec_done = in_.spec_done;
        // [协议] HTTP 4xx/5xx（>= 400）在 ITNA 模式下交由 ITNA Attempts 状态机统一判定是否升级浏览器，
        // 避免薄页升级失败后再次触发第二次浏览器回退。
        if self.itna && in_.http_status >= 400 {
            drop(speculative.take());
            return None;
        }
        let Some(hints) = in_.hints.as_ref().filter(|h| h.worth) else {
            drop(speculative.take());
            return None;
        };
        let (go, step) = if in_.render_blocked {
            (true, Default::default())
        } else {
            self.plan_render(in_.url, in_.doc, in_.html_len, hints, in_.opts, in_.started)
        };
        if in_.opts.explain && !in_.render_blocked {
            in_.plan.push(step);
        }
        if in_.render_blocked {
            // 浏览器用不了这个出口（没有 Chrome，或带认证的 SOCKS 代理）：换个出口渲染，地址就不一致了
            in_.warnings.push("render_skipped_egress".into());
        } else if !go {
            in_.warnings.push("render_skipped_learned".into());
        } else if self.has_browser().await {
            // HTTP extraction no longer needs its admission slots. Rendering
            // takes a fresh slot below; retaining both can deadlock a batch
            // when every per-host slot is held by an auto-upgrade.
            in_.permit.take();
            let budget = self.render_stats.budget();
            let (render, spec) = match speculative.take() {
                // 推测出去的那一路接着用：已经跑了这么久，预算里扣掉
                Some((at, warned, fut)) => (fut, Some((at, warned))),
                None => {
                    in_.spec_baseline
                        .store(in_.http_chars as u64, std::sync::atomic::Ordering::Relaxed);
                    let early_stop = pagesift_cdp::EarlyStop {
                        baseline_chars: in_.http_chars as u64,
                        cap_navigation: hints.client_redirect,
                        http_elapsed: in_.http_elapsed,
                        after: self.render_stats.early_stop_after(),
                        probe: super::speculate::render_probe(
                            in_.opts,
                            in_.final_url.clone(),
                            in_.spec_baseline.clone(),
                        ),
                    };
                    let progress = Progress {
                        started: in_.started,
                        timings: in_.timings.clone(),
                        warnings: in_.warnings.clone(),
                    };
                    let fut: SpecRenderFut<'_> = Box::pin(self.render(
                        in_.url,
                        in_.render_opts,
                        in_.mode,
                        progress,
                        Some(in_.http_chars),
                        Some(early_stop),
                        in_.proxy.clone(),
                        in_.proxy_label.to_owned(),
                    ));
                    (fut, None)
                }
            };
            let budget = budget.map(|b| spec.map_or(b, |(at, _)| b.saturating_sub(at.elapsed())));
            let outcome = match spec_done.take() {
                Some(done) => Some(done),
                None => match budget {
                    Some(budget) => match tokio::time::timeout(budget, render).await {
                        Ok(result) => Some(result),
                        Err(_) => {
                            in_.warnings.push(format!(
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
                    for (k, v) in in_.timings {
                        ext.timings.entry(k.clone()).or_insert(*v);
                    }
                    let late = in_.warnings[warned.min(in_.warnings.len())..].to_vec();
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
                            in_.url,
                            in_.doc,
                            in_.html_len,
                            hints.client_code,
                            in_.http_chars,
                            rendered,
                        );
                        if in_.opts.explain {
                            in_.plan.push(step);
                        }
                        if let Some(mut better) = better {
                            if better.metadata.canonical_url.is_none() {
                                better.metadata.canonical_url = in_.header_canonical.clone();
                            }
                            crate::graph::merge_header_alternates(
                                &mut better,
                                in_.header_alternates,
                            );
                            if let Some(gopts) = in_.graph_opts {
                                let canon_from_header = in_.header_canonical.is_some()
                                    && better.metadata.canonical_url == *in_.header_canonical;
                                let provenance_opts = gopts.as_attempt();
                                crate::graph::apply_http_provenance(
                                    self.store.as_ref(),
                                    &provenance_opts,
                                    in_.graph_tenant.as_deref(),
                                    &mut better,
                                    in_.redirect_hops,
                                    canon_from_header,
                                    in_.header_alternates,
                                );
                                self.commit_fallback_render_graph(
                                    in_.url,
                                    in_.redirect_hops,
                                    in_.graph_tenant.as_deref(),
                                    gopts,
                                    &mut better,
                                );
                            }
                            if in_.opts.explain {
                                better
                                    .metadata
                                    .pagesift
                                    .get_or_insert_with(Default::default)
                                    .plan = Some(pagesift_api::types::PlanTrace {
                                    steps: std::mem::take(in_.plan),
                                });
                            }
                            return Some(better);
                        }
                        // 渲染没有补到正文：留 HTTP 的结果，占内存也少
                        in_.warnings.push("render_no_gain_http_fallback".into());
                    }
                    Err(e) => {
                        // A failed upgrade is not a failed scrape: keep the
                        // HTTP result and say why the browser was not used.
                        in_.warnings
                            .push(format!("render_failed_http_fallback: {}", e.message));
                    }
                }
            }
        } else {
            in_.warnings.push(format!(
                "render_unavailable: {}",
                self.browser_unavailable_reason()
            ));
        }

        // 推测出去、最后没用上的渲染：现在就取消，归还标签
        drop(speculative.take());
        None
    }

    fn commit_fallback_render_graph(
        &self,
        url: &str,
        redirect_hops: &[(String, String, u16, pagesift_graph::RedirectKind)],
        graph_tenant: Option<&str>,
        gopts: &pagesift_graph::GraphOptions,
        better: &mut Document,
    ) {
        if !(gopts.persist && !crate::graph::stage::is_stage_active()) {
            return;
        }
        let Some(st) = &self.store else {
            return;
        };
        let staged_fallback = better.graph.as_ref().map(|slice| {
            let clean_url = strip_url_userinfo_str(url);
            let ps = better.metadata.pagesift.as_ref();
            let fallback_node = pagesift_graph::DocNode {
                doc_id: slice.doc_id.clone(),
                resource_key: slice.resource_key.clone(),
                requested_url: clean_url.clone(),
                final_url: better.metadata.url.clone().unwrap_or(clean_url),
                canonical_url: better.metadata.canonical_url.clone(),
                source_kind: slice.source_kind.clone(),
                observed_at_ms: slice.observed_at_ms,
                status_code: better.metadata.status_code,
                engine: "browser".to_string(),
                content_hash: ps.and_then(|p| p.content_hash.clone()),
                raw_hash: ps.and_then(|p| p.raw_hash.clone()),
                text_hash: ps.and_then(|p| p.text_hash.clone()),
                etag: ps.and_then(|p| p.etag.clone()),
                last_modified: ps.and_then(|p| p.last_modified.clone()),
                title: better.metadata.title.clone(),
            };
            crate::graph::stage::StagedGraphCommit {
                doc_id: slice.doc_id.clone(),
                resource_key: slice.resource_key.clone(),
                node: fallback_node,
                alias_keys: crate::graph::trusted_permanent_redirect_keys(
                    redirect_hops,
                    better.metadata.url.as_deref().unwrap_or(url),
                )
                .into_iter()
                .filter(|k| k != &slice.resource_key)
                .collect(),
                markdown: better.markdown.clone(),
                matched_304_md: None,
                not_modified: false,
                not_modified_unavail_reason: None,
                previous_version: None,
                matched_304_version: None,
                chunk_spans: Vec::new(),
                had_initial_prev: false,
                seed_content_hash: None,
                seed_raw_hash: None,
            }
        });
        if let Some(staged_fallback) = staged_fallback {
            crate::graph::commit_staged_document(st, gopts, graph_tenant, better, staged_fallback);
        }
    }
}
