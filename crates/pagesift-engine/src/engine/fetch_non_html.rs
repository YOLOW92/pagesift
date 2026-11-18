//! Handling for conditional 304 and non-HTML HTTP responses (Office, image OCR, PDF, JSON).

use pagesift_net::FetchResponse;

use super::*;

pub(super) struct FetchHeadState {
    pub meta: Metadata,
    pub ext: PagesiftMeta,
    pub timings: BTreeMap<String, f64>,
    pub warnings: Vec<String>,
    pub header_canonical: Option<String>,
    pub header_alternates: Vec<(String, String)>,
    pub redirect_hops: Vec<(String, String, u16, pagesift_graph::RedirectKind)>,
    pub graph_opts: Option<pagesift_graph::GraphOptions>,
    pub graph_tenant: Option<String>,
}

pub(super) enum NonHtmlStep {
    Done(Box<Document>),
    Continue(Box<(FetchResponse, FetchHeadState)>),
}

impl Scraper {
    /// Handle HTTP 304 and non-HTML payloads before falling back to HTML extraction.
    pub(super) async fn try_extract_non_html(
        &self,
        resp: FetchResponse,
        opts: &ScrapeOptions,
        started: Instant,
        mut state: FetchHeadState,
    ) -> Result<NonHtmlStep, ApiError> {
        if resp.not_modified {
            let req_inm = opts.if_none_match.clone().or_else(|| {
                opts.headers
                    .iter()
                    .find(|(k, _)| k.eq_ignore_ascii_case("if-none-match"))
                    .map(|(_, v)| v.clone())
            });
            let req_ims = opts.if_modified_since.clone().or_else(|| {
                opts.headers
                    .iter()
                    .find(|(k, _)| k.eq_ignore_ascii_case("if-modified-since"))
                    .map(|(_, v)| v.clone())
            });
            let unconditional = req_inm.is_none() && req_ims.is_none();
            if unconditional {
                // 没发条件请求却拿到 304：服务器行为异常，这里也没有缓存可用。
                state.warnings.push(
                    "not_modified: HTTP 304 for an unconditional request; no content was returned"
                        .into(),
                );
            }
            state.ext.timings = state.timings;
            state.ext.warnings = state.warnings;
            state.meta.pagesift = Some(state.ext.clone());
            let mut out = Document {
                metadata: state.meta,
                alternates: state.header_alternates,
                ..Default::default()
            };
            if let Some(gopts) = &state.graph_opts {
                crate::graph::build_and_attach_page_graph(
                    self.store.as_ref(),
                    gopts,
                    &mut out,
                    crate::graph::PageGraphSeed {
                        canonical_from_header: state.header_canonical.is_some(),
                        etag: state.ext.etag,
                        last_modified: state.ext.last_modified,
                        if_none_match: req_inm,
                        if_modified_since: req_ims,
                        unconditional_not_modified: unconditional,
                        redirect_chain: state.redirect_hops,
                        tenant: state.graph_tenant,
                        ..Default::default()
                    },
                );
            }
            return Ok(NonHtmlStep::Done(Box::new(out)));
        }

        // Word、Excel、PowerPoint、EPUB、CSV：内容、Content-Type、网址的扩展名一起判断
        if let Some(format) = office_format(&resp.body, resp.final_url.path(), resp.content_type())
        {
            let progress = Progress {
                started,
                timings: state.timings,
                warnings: state.warnings,
            };
            let deadline = request_deadline(opts, started);
            let input = office::OfficeInput {
                content_type: resp.content_type().map(str::to_owned),
                bytes: resp.body,
                format,
            };
            let mut out = self
                .office_document(input, opts, (state.meta, state.ext), progress, deadline)
                .await?;
            self.finish_binary_non_html(
                &mut out,
                state.header_canonical.as_ref(),
                &state.header_alternates,
                state.graph_opts.as_ref(),
                state.graph_tenant.as_deref(),
                &state.redirect_hops,
            );
            return Ok(NonHtmlStep::Done(Box::new(out)));
        }

        // 图片：打开了 OCR 才转换；没开时说明怎么开（其他媒体类型仍按原来的提示）
        if pagesift_pdf::image_mime(&resp.body).is_some() {
            let progress = Progress {
                started,
                timings: state.timings,
                warnings: state.warnings,
            };
            let mut out = self
                .image_document(
                    resp.body,
                    opts,
                    (state.meta, state.ext),
                    progress,
                    request_deadline(opts, started),
                )
                .await?;
            self.finish_binary_non_html(
                &mut out,
                state.header_canonical.as_ref(),
                &state.header_alternates,
                state.graph_opts.as_ref(),
                state.graph_tenant.as_deref(),
                &state.redirect_hops,
            );
            return Ok(NonHtmlStep::Done(Box::new(out)));
        }

        if is_pdf(&resp.body, resp.content_type()) {
            let progress = Progress {
                started,
                timings: state.timings,
                warnings: state.warnings,
            };
            let mut out = self
                .pdf_document(
                    resp.body,
                    opts,
                    (state.meta, state.ext),
                    progress,
                    request_deadline(opts, started),
                )
                .await?;
            self.finish_binary_non_html(
                &mut out,
                state.header_canonical.as_ref(),
                &state.header_alternates,
                state.graph_opts.as_ref(),
                state.graph_tenant.as_deref(),
                &state.redirect_hops,
            );
            return Ok(NonHtmlStep::Done(Box::new(out)));
        }

        if let Some(err) =
            corrupt_declared_binary_error(resp.final_url.path(), resp.content_type(), &resp.body)
        {
            return Err(err);
        }

        if let Some(kind) = unsupported_content_type(resp.content_type(), &resp.body) {
            return Err(ApiError::unsupported(format!(
                "unsupported content type `{kind}`: pagesift extracts HTML/text; use `pagesift download` or `media-extract` for binary media"
            )));
        }

        if let Some((markdown, json_val)) = parse_json_markdown(resp.content_type(), &resp.body) {
            let raw_json_traces = if state
                .graph_opts
                .as_ref()
                .is_some_and(|g| g.evidence || g.facts || g.conflicts)
            {
                let mut traces = Vec::new();
                let method = pagesift_graph::ExtractionMethod::RegexPattern {
                    pattern: "json".to_string(),
                    label: Some("json".to_string()),
                };
                pagesift_graph::collect_json_traces(
                    &json_val,
                    "",
                    &method,
                    true,
                    state.meta.url.as_deref(),
                    &mut traces,
                );
                traces
            } else {
                Vec::new()
            };
            let mut out = finish(
                Document {
                    markdown: Some(markdown),
                    alternates: state.header_alternates,
                    ..Default::default()
                },
                state.meta,
                state.ext,
                state.timings,
                state.warnings,
                None,
                false,
                started,
            );
            if let Some(gopts) = &state.graph_opts {
                let ps = out.metadata.pagesift.as_ref();
                let etag = ps.and_then(|p| p.etag.clone());
                let last_modified = ps.and_then(|p| p.last_modified.clone());
                crate::graph::build_and_attach_page_graph(
                    self.store.as_ref(),
                    gopts,
                    &mut out,
                    crate::graph::PageGraphSeed {
                        source_kind: pagesift_graph::SourceKind::Api,
                        engine: "http".to_string(),
                        canonical_from_header: state.header_canonical.is_some(),
                        etag,
                        last_modified,
                        redirect_chain: state.redirect_hops,
                        raw_traces: raw_json_traces,
                        tenant: state.graph_tenant,
                        ..Default::default()
                    },
                );
            }
            if !opts.wants("markdown") {
                out.markdown = None;
            }
            return Ok(NonHtmlStep::Done(Box::new(out)));
        }

        Ok(NonHtmlStep::Continue(Box::new((resp, state))))
    }

    fn finish_binary_non_html(
        &self,
        out: &mut Document,
        header_canonical: Option<&String>,
        header_alternates: &[(String, String)],
        graph_opts: Option<&pagesift_graph::GraphOptions>,
        graph_tenant: Option<&str>,
        redirect_hops: &[(String, String, u16, pagesift_graph::RedirectKind)],
    ) {
        crate::graph::merge_header_alternates(out, header_alternates);
        if let Some(gopts) = graph_opts {
            let canon_from_header = header_canonical.is_some()
                && out.metadata.canonical_url.as_ref() == header_canonical;
            crate::graph::apply_http_provenance(
                self.store.as_ref(),
                gopts,
                graph_tenant,
                out,
                redirect_hops,
                canon_from_header,
                header_alternates,
            );
        }
    }
}
