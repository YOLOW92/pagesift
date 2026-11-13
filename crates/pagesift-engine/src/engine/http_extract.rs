//! HTTP 抓到的页面在一个 CPU 任务里做完：转码、正文提取，以及 `engine: auto` 判断要不要渲染的几项。
//! 都读同一份文本、同一棵 DOM，也都不占异步线程。

use super::*;

/// `engine: auto` 时决定要不要渲染用到的页面特征。
pub(super) struct RenderHints {
    /// 值不值得为它开浏览器：HTML 不小，或带脚本。没有脚本的小静态页渲染只会白抓一次。
    pub worth: bool,
    /// 有客户端代码（可执行脚本或内联事件）。
    pub client_code: bool,
    /// 几乎没正文、只是用脚本跳转的小页面。
    pub client_redirect: bool,
}

pub(super) struct Fetched {
    pub doc: pagesift_content::Document,
    pub html: String,
    pub decode_warning: Option<String>,
    /// 正文的加权字数。
    pub http_chars: usize,
    /// 只有 `auto` 且正文不够、可能要渲染时才算。
    pub render: Option<RenderHints>,
    /// 试过 `embedded-json` 候选时：采用时的合并 Markdown（只在最后返回 HTTP 结果时用），以及依据
    /// （`metadata.pagesift.plan`）。
    pub embedded: Option<(Option<String>, pagesift_api::types::PlanStep)>,
    pub graph_dom: Option<crate::graph::dom_links::DomGraphData>,
    pub graph_traces: Vec<pagesift_graph::RawFieldTrace>,
}

/// [策略] 先验包 v0：0.81 的现值。"小页面"的 HTML 字节数，只用作候选门：没有脚本的小页面浏览器没东西可执行，
/// 只有脚本跳转的小页面 Lightpanda 不会跟随；过了门的页由 `planner.rs` 按学到的后验决定
const SMALL_HTML_BYTES: usize = 4096;

pub(super) async fn extract_fetched(
    decode: impl FnOnce() -> pagesift_net::Decoded + Send + 'static,
    base: &Url,
    status: Option<u16>,
    opts: &ScrapeOptions,
    auto: bool,
) -> Result<Fetched, ApiError> {
    let copts = content_options(base, status, opts);
    let tags = tag_filter(opts)?;
    let tagged = tags.is_some();
    let graph_opts = crate::graph::effective_graph_options(opts);
    let want_dom = graph_opts.is_some();
    let want_traces = graph_opts
        .as_ref()
        .is_some_and(|g| g.evidence || g.facts || g.conflicts);
    let base_str = want_dom.then(|| base.to_string());
    cpu_task(move || {
        let decoded = decode();
        let decode_warning = decoded.warning();
        let html = decoded.text;
        let mut dom_links_captured: Option<(
            Vec<pagesift_graph::RawLinkObservation>,
            Option<String>,
        )> = None;
        let mut is_feed = false;
        // feed 和标签筛选的提取不用原页面的 DOM，要判断客户端代码时再解析一次原页面
        let (doc, client_code) = if let Some(doc) = super::feed::parse(&html, copts.links) {
            is_feed = true;
            (doc, None)
        } else if let Some(t) = tags {
            let filtered = t.apply(&html);
            let doc = CONTENT_WORKSPACE.with(|ws| {
                pagesift_content::process_with_workspace(&filtered, &copts, &mut ws.borrow_mut())
            });
            (doc, None)
        } else {
            CONTENT_WORKSPACE.with(|ws| {
                pagesift_content::process_with_workspace_then(
                    &html,
                    &copts,
                    &mut ws.borrow_mut(),
                    |dom, doc| {
                        if want_dom && let Some(b) = base_str.as_deref() {
                            dom_links_captured =
                                Some(crate::graph::dom_links::collect_from_dom(dom, Some(b)));
                        }
                        (auto && needs_render(doc)).then(|| client_code_in(dom))
                    },
                )
            })
        };
        let graph_dom = if let Some(b) = base_str.as_deref() {
            if let Some((links, dom_canon)) = dom_links_captured {
                Some(crate::graph::dom_links::DomGraphData {
                    links,
                    canonical_url: doc.metadata.canonical_url.clone().or(dom_canon),
                    alternates: doc.metadata.alternates.clone(),
                    json_ld: doc.metadata.json_ld.clone(),
                    raw_html_hash: doc.fingerprints.as_ref().map(|f| f.raw_html.clone()),
                    markdown_hash: doc.fingerprints.as_ref().map(|f| f.markdown.clone()),
                    text_hash: doc.fingerprints.as_ref().map(|f| f.text.clone()),
                })
            } else if tagged {
                Some(crate::graph::dom_links::extract_from_html_str(
                    &html,
                    &doc.metadata,
                    doc.fingerprints.as_ref(),
                    Some(b),
                ))
            } else {
                Some(crate::graph::dom_links::DomGraphData {
                    canonical_url: doc.metadata.canonical_url.clone(),
                    alternates: doc.metadata.alternates.clone(),
                    json_ld: doc.metadata.json_ld.clone(),
                    raw_html_hash: doc.fingerprints.as_ref().map(|f| f.raw_html.clone()),
                    markdown_hash: doc.fingerprints.as_ref().map(|f| f.markdown.clone()),
                    text_hash: doc.fingerprints.as_ref().map(|f| f.text.clone()),
                    ..Default::default()
                })
            }
        } else {
            None
        };
        let graph_traces = if want_traces && !is_feed {
            crate::graph::evidence::collect_structured_html_traces(&html, base_str.as_deref())
        } else {
            Vec::new()
        };
        // 正文不够时看页面数据岛里有没有正文（不用浏览器）。合并结果只用在最后返回 HTTP 结果的时候：
        // 要不要渲染、渲染有没有增益都按 HTTP 本身的结果判断——质量分不看篇幅，补进一个标题和一段简介
        // 就会接近满分，拿它去和渲染出的整页比会把整页丢掉
        let embedded = (auto && !tagged && needs_render(&doc))
            .then(|| {
                let mut merged = doc.clone();
                let step = super::planner::embedded_json(&mut merged, &html)?;
                Some((step.chosen.then_some(merged.markdown), step))
            })
            .flatten();
        let http_chars = text_weight(&doc.markdown);
        let render = (auto && needs_render(&doc)).then(|| RenderHints {
            worth: html.len() >= SMALL_HTML_BYTES || pagesift_flow::sniff::has_script_tag(&html),
            client_code: client_code.unwrap_or_else(|| has_client_code(&html)),
            client_redirect: http_chars == 0
                && html.len() < SMALL_HTML_BYTES
                && pagesift_flow::sniff::has_client_redirect(&html),
        });
        Fetched {
            doc,
            html,
            decode_warning,
            http_chars,
            render,
            embedded,
            graph_dom,
            graph_traces,
        }
    })
    .await
}
