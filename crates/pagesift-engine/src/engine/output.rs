use super::*;

pub(super) fn fetch_request(url: &str, opts: &ScrapeOptions) -> Result<FetchRequest, ApiError> {
    let mut req = FetchRequest::new(url);
    for (k, v) in &opts.headers {
        req = req.header(k.clone(), v.clone());
    }
    if let Some(etag) = &opts.if_none_match {
        req = req.if_none_match(etag.clone());
    }
    if let Some(t) = &opts.if_modified_since {
        let t = httpdate::parse_http_date(t).map_err(|_| {
            ApiError::invalid(format!("ifModifiedSince is not an HTTP date: {t:?}"))
        })?;
        req = req.if_modified_since(t);
    }
    req.timeout = opts.timeout.map(Duration::from_millis);
    Ok(req)
}

/// `chunking`：正文 Markdown 分块。参数格式见 [`pagesift_extract::ChunkStrategy`]（`maxLevel` 这类驼峰写法也认）。
pub fn chunk_markdown(
    md: &str,
    spec: &serde_json::Value,
) -> Result<Vec<pagesift_api::types::DocChunk>, ApiError> {
    // API 用驼峰，提取库用下划线：只改键名
    let snake = match spec {
        serde_json::Value::Object(m) => serde_json::Value::Object(
            m.iter()
                .map(|(k, v)| {
                    let mut out = String::new();
                    for c in k.chars() {
                        if c.is_ascii_uppercase() {
                            out.push('_');
                            out.push(c.to_ascii_lowercase());
                        } else {
                            out.push(c);
                        }
                    }
                    (out, v.clone())
                })
                .collect(),
        ),
        other => other.clone(),
    };
    let strategy: pagesift_extract::ChunkStrategy = serde_json::from_value(snake)
        .map_err(|e| ApiError::invalid(format!("invalid chunking: {e}")))?;
    let chunks = pagesift_extract::chunk(md, &strategy)
        .map_err(|e| ApiError::invalid(format!("invalid chunking: {e}")))?;
    Ok(chunks
        .into_iter()
        .map(|c| pagesift_api::types::DocChunk {
            text: c.text,
            heading_path: c.heading_path,
            start: c.start,
            end: c.end,
            tokens: c.tokens,
        })
        .collect())
}

/// `queryFilter`：只留下和查询相关的内容，统计写进 `metadata.pagesift.queryFilter`。
pub(super) async fn apply_query_filter(
    doc: &mut Document,
    q: &pagesift_api::types::QueryFilter,
) -> Result<(), ApiError> {
    let Some(md) = doc.markdown.take() else {
        return Ok(());
    };
    let fo = pagesift_extract::QueryFilterOptions {
        threshold: q.threshold,
        relative: q.relative,
        top_k: q.top_k,
        max_tokens: q.max_tokens,
        min_tokens: q.min_tokens,
        headings: q.headings,
    };
    let query = q.query.clone();
    let (f, before) = cpu_task(move || {
        let f = pagesift_extract::filter_markdown_by_query(&md, &query, &fo);
        (f, pagesift_content::estimate_tokens(&md))
    })
    .await?;
    let ext = doc.metadata.pagesift.get_or_insert_with(Default::default);
    if f.kept == 0 && f.total > 0 {
        ext.warnings.push("query_filter_no_match".into());
    }
    ext.query_filter = Some(pagesift_api::types::QueryFilterStats {
        kept_blocks: f.kept,
        total_blocks: f.total,
        tokens_before: before,
        tokens_after: pagesift_content::estimate_tokens(&f.markdown),
    });
    doc.markdown = Some(f.markdown);
    Ok(())
}

pub(super) fn strip_url_userinfo(url: &Url) -> Url {
    if url.username().is_empty() && url.password().is_none() {
        return url.clone();
    }
    let mut clean = url.clone();
    let _ = clean.set_username("");
    let _ = clean.set_password(None);
    clean
}

pub(super) fn strip_url_userinfo_str(raw: &str) -> String {
    let Ok(parsed) = Url::parse(raw) else {
        return raw.to_string();
    };
    if parsed.username().is_empty() && parsed.password().is_none() {
        raw.to_string()
    } else {
        strip_url_userinfo(&parsed).to_string()
    }
}

pub(super) fn collect_html_text_warnings(
    raw_html: &str,
    dom: Option<&pagesift_content::yolowhtml_dom::Html<'_>>,
) -> Vec<String> {
    if let Some(doc) = dom {
        return collect_dom_warnings_and_traces(raw_html, doc, false, None).0;
    }
    let mut warnings = Vec::new();
    let nul_bytes = raw_html.as_bytes().iter().filter(|&&b| b == 0).count();
    if nul_bytes > 0 {
        warnings.push(format!(
            "nul_bytes_stripped: input contained {nul_bytes} NUL byte(s) stripped during HTML parsing"
        ));
    }
    const LD_JSON: &[u8] = b"ld+json";
    if raw_html
        .as_bytes()
        .windows(LD_JSON.len())
        .any(|w| w.eq_ignore_ascii_case(LD_JSON))
    {
        let malformed = pagesift_extract::structured::count_malformed_json_ld_str(raw_html);
        if malformed > 0 {
            warnings.push(format!(
                "malformed_json_ld: {malformed} JSON-LD script block(s) failed to parse"
            ));
        }
    }
    warnings
}

/// 在同一棵已解析的 `dom` 上一次完成 HTML 文本诊断告警（NUL 字节、损坏的 JSON-LD）与
/// 结构化证据轨迹（JSON-LD、Microdata、OpenGraph、`__NEXT_DATA__`/`__NUXT_DATA__`）提取，
/// 消除额外的整页 DOM 构建与二次 JSON-LD 遍历。
pub(super) fn collect_dom_warnings_and_traces(
    raw_html: &str,
    dom: &pagesift_content::yolowhtml_dom::Html<'_>,
    want_traces: bool,
    source_url: Option<&str>,
) -> (Vec<String>, Vec<pagesift_graph::RawFieldTrace>) {
    let mut warnings = Vec::new();
    let nul_bytes = raw_html.as_bytes().iter().filter(|&&b| b == 0).count();
    if nul_bytes > 0 {
        warnings.push(format!(
            "nul_bytes_stripped: input contained {nul_bytes} NUL byte(s) stripped during HTML parsing"
        ));
    }
    if !want_traces || raw_html.is_empty() {
        const LD_JSON: &[u8] = b"ld+json";
        if raw_html
            .as_bytes()
            .windows(LD_JSON.len())
            .any(|w| w.eq_ignore_ascii_case(LD_JSON))
        {
            let malformed = pagesift_extract::structured::count_malformed_json_ld(dom);
            if malformed > 0 {
                warnings.push(format!(
                    "malformed_json_ld: {malformed} JSON-LD script block(s) failed to parse"
                ));
            }
        }
        return (warnings, Vec::new());
    }
    let diag = pagesift_extract::structured::extract_json_ld_with_diagnostics(dom);
    if diag.malformed > 0 {
        warnings.push(format!(
            "malformed_json_ld: {} JSON-LD script block(s) failed to parse",
            diag.malformed
        ));
    }
    let structured = pagesift_extract::structured::StructuredData {
        json_ld: diag.items,
        microdata: pagesift_extract::structured::extract_microdata(dom),
        opengraph: pagesift_extract::structured::extract_opengraph(dom),
    };
    let mut traces = pagesift_extract::evidence::extract_structured_traces(&structured, source_url);
    if raw_html.contains("__NEXT_DATA__") || raw_html.contains("__NUXT_DATA__") {
        for node in dom.root().descendants() {
            let Some(el) = node.as_element() else {
                continue;
            };
            if el.name() != "script" {
                continue;
            }
            let Some(id) = el.attr("id") else {
                continue;
            };
            if id != "__NEXT_DATA__" && id != "__NUXT_DATA__" {
                continue;
            }
            let raw = node.text();
            let trimmed = raw.trim();
            if !trimmed.is_empty()
                && let Ok(val) = serde_json::from_str::<serde_json::Value>(trimmed)
            {
                let method = pagesift_graph::ExtractionMethod::HydrationJson {
                    script_id: id.to_string(),
                };
                pagesift_graph::collect_json_traces(
                    &val,
                    "",
                    &method,
                    false,
                    source_url,
                    &mut traces,
                );
            }
        }
    }
    (warnings, traces)
}

pub(super) struct ProcessedHtml {
    pub doc: pagesift_content::Document,
    pub html: String,
    pub warnings: Vec<String>,
    pub graph_dom: Option<crate::graph::dom_links::DomGraphData>,
    pub graph_traces: Vec<pagesift_graph::RawFieldTrace>,
}

pub(super) async fn process_html(
    html: String,
    base: &Url,
    status: Option<u16>,
    opts: &ScrapeOptions,
) -> Result<(pagesift_content::Document, String, Vec<String>), ApiError> {
    let out = process_html_with_graph(html, base, status, opts, false, false).await?;
    Ok((out.doc, out.html, out.warnings))
}

pub(super) async fn process_html_with_graph(
    html: String,
    base: &Url,
    status: Option<u16>,
    opts: &ScrapeOptions,
    want_dom: bool,
    want_traces: bool,
) -> Result<ProcessedHtml, ApiError> {
    let copts = content_options(base, status, opts);
    let tags = dom_tag_filter(opts)?;
    let base_str = (want_dom || want_traces).then(|| base.to_string());
    cpu_task(move || {
        // RSS / Atom / RDF feed 不是网页：按条目解析，别让 HTML 提取器把标签剥成一锅粥。
        if let Some(document) = super::feed::parse(&html, copts.links) {
            if want_dom || want_traces {
                let (warnings, graph_traces, graph_dom) = super::cpu::with_dom(&html, |dom| {
                    let (warnings, traces) = collect_dom_warnings_and_traces(
                        &html,
                        dom,
                        want_traces,
                        base_str.as_deref(),
                    );
                    let dom_data = want_dom.then(|| {
                        crate::graph::dom_links::collect_dom_graph_data(
                            dom,
                            &document.metadata,
                            document.fingerprints.as_ref(),
                            base_str.as_deref(),
                        )
                    });
                    (warnings, traces, dom_data)
                });
                return ProcessedHtml {
                    doc: document,
                    html,
                    warnings,
                    graph_dom,
                    graph_traces,
                };
            }
            let warnings = collect_html_text_warnings(&html, None);
            return ProcessedHtml {
                doc: document,
                html,
                warnings,
                graph_dom: None,
                graph_traces: Vec::new(),
            };
        }
        // includeTags / excludeTags 只影响提取；rawHtml 与图谱链接/证据仍读原始页面单棵 DOM
        if let Some(t) = tags {
            let (filtered, warnings, graph_traces, raw_links) =
                super::cpu::with_dom(&html, |raw_dom| {
                    let filtered = t.apply_dom(raw_dom);
                    let (warnings, traces) = collect_dom_warnings_and_traces(
                        &html,
                        raw_dom,
                        want_traces,
                        base_str.as_deref(),
                    );
                    let links = want_dom.then(|| {
                        crate::graph::dom_links::collect_from_dom(raw_dom, base_str.as_deref())
                    });
                    (filtered, warnings, traces, links)
                });
            let document = CONTENT_WORKSPACE.with(|workspace| {
                pagesift_content::process_with_workspace(
                    &filtered,
                    &copts,
                    &mut workspace.borrow_mut(),
                )
            });
            let graph_dom =
                raw_links.map(|(links, dom_canon)| crate::graph::dom_links::DomGraphData {
                    links,
                    canonical_url: document.metadata.canonical_url.clone().or(dom_canon),
                    alternates: document.metadata.alternates.clone(),
                    json_ld: document.metadata.json_ld.clone(),
                    raw_html_hash: document.fingerprints.as_ref().map(|f| f.raw_html.clone()),
                    markdown_hash: document.fingerprints.as_ref().map(|f| f.markdown.clone()),
                    text_hash: document.fingerprints.as_ref().map(|f| f.text.clone()),
                });
            return ProcessedHtml {
                doc: document,
                html,
                warnings,
                graph_dom,
                graph_traces,
            };
        }
        let (document, (warnings, graph_traces, graph_dom)) = CONTENT_WORKSPACE.with(|workspace| {
            pagesift_content::process_with_workspace_then(
                &html,
                &copts,
                &mut workspace.borrow_mut(),
                |dom, doc| {
                    let (warnings, traces) = collect_dom_warnings_and_traces(
                        &html,
                        dom,
                        want_traces,
                        base_str.as_deref(),
                    );
                    let dom_data = want_dom.then(|| {
                        crate::graph::dom_links::collect_dom_graph_data(
                            dom,
                            &doc.metadata,
                            doc.fingerprints.as_ref(),
                            base_str.as_deref(),
                        )
                    });
                    (warnings, traces, dom_data)
                },
            )
        });
        ProcessedHtml {
            doc: document,
            html,
            warnings,
            graph_dom,
            graph_traces,
        }
    })
    .await
}

/// 提取参数：只按请求要的格式算（链接、图片、清洗后的 HTML、变化检测的指纹）。
pub(super) fn content_options(
    base: &Url,
    status: Option<u16>,
    opts: &ScrapeOptions,
) -> pagesift_content::Options {
    let graph_versions = crate::graph::effective_graph_options(opts).is_some_and(|g| g.versions);
    pagesift_content::Options {
        base_url: Some(strip_url_userinfo(base)),
        only_main_content: opts.only_main_content,
        keep_data_images: !opts.remove_base64_images,
        status,
        detect_barrier: true,
        clean_html: opts.wants("html"),
        links: opts.wants("links"),
        images: opts.wants("images"),
        // 完整指纹（原始 HTML 的哈希、再渲染一遍纯文本做规范化哈希）约占提取 CPU 的一成，只在变化检测或 DataGraph 版本追踪要用时算
        fingerprints: opts.wants("changeTracking") || graph_versions,
        ..Default::default()
    }
}

/// `includeTags` / `excludeTags` 的筛选规则；都没给时为 `None`，选择器写错时返回 `INVALID_REQUEST`。
pub fn tag_filter(opts: &ScrapeOptions) -> Result<Option<pagesift_content::TagFilter>, ApiError> {
    pagesift_content::TagFilter::new(&opts.include_tags, &opts.exclude_tags)
        .map_err(ApiError::invalid)
}

pub(super) fn dom_tag_filter(
    opts: &ScrapeOptions,
) -> Result<Option<pagesift_content::DomTagFilter>, ApiError> {
    pagesift_content::DomTagFilter::new(&opts.include_tags, &opts.exclude_tags)
        .map_err(ApiError::invalid)
}

fn contains_ascii_ci(haystack: &[u8], needle_lower: &[u8]) -> bool {
    if needle_lower.is_empty() {
        return true;
    }
    if haystack.len() < needle_lower.len() {
        return false;
    }
    let first = needle_lower[0];
    let first_up = first.to_ascii_uppercase();
    let end = haystack.len() - needle_lower.len();
    let mut i = 0;
    while i <= end {
        let b = haystack[i];
        if (b == first || b == first_up)
            && haystack[i + 1..i + needle_lower.len()].eq_ignore_ascii_case(&needle_lower[1..])
        {
            return true;
        }
        i += 1;
    }
    false
}

pub(super) fn check_barrier(
    doc: &pagesift_content::Document,
    raw_html: &str,
) -> Result<(), ApiError> {
    let Some(b) = &doc.barrier else { return Ok(()) };
    // A vendor challenge page can also embed an interactive widget. The
    // primary rule may classify the page as a challenge, so inspect the raw
    // document for widgets that need a human before saying it is automatic.
    let raw_bytes = raw_html.as_bytes();
    let interactive = [
        &b"g-recaptcha"[..],
        &b"h-captcha"[..],
        &b"cf-turnstile"[..],
        &b"px-captcha"[..],
        &b"captcha-delivery"[..],
        &b"geetest"[..],
    ]
    .iter()
    .any(|marker| contains_ascii_ci(raw_bytes, marker));
    Err(ApiError::new(
        ErrorCode::AccessBarrier,
        format!(
            "the site returned an access barrier ({} {})",
            b.vendor,
            b.kind.as_str()
        ),
    )
    .with_details(serde_json::json!({
        "vendor": b.vendor,
        "kind": b.kind.as_str(),
        "confidence": b.confidence,
        "ruleId": b.rule_id,
        "needsHuman": interactive || matches!(b.kind, pagesift_content::BarrierKind::Captcha),
    })))
}

pub(super) fn document(
    doc: pagesift_content::Document,
    opts: &ScrapeOptions,
    raw_html: String,
) -> Document {
    Document {
        markdown: opts.wants("markdown").then_some(doc.markdown),
        html: doc.html,
        raw_html: opts.wants("rawHtml").then_some(raw_html),
        link_text: doc
            .links
            .as_ref()
            .map(|l| l.iter().map(|l| l.text.clone()).collect()),
        alternates: doc.metadata.alternates.clone(),
        links: doc.links.map(|l| l.into_iter().map(|l| l.url).collect()),
        images: doc.images.map(|i| i.into_iter().map(|i| i.src).collect()),
        ..Default::default()
    }
}

/// 正文 Markdown 的哈希总是给（Markdown 只有几 KB，几乎不花时间）；原始 HTML 和规范化纯文本的
/// 哈希只在请求变化检测时算（见 `process_html`），它们要整页再过一遍。
pub(super) fn fill_fingerprints(ext: &mut PagesiftMeta, doc: &pagesift_content::Document) {
    match &doc.fingerprints {
        Some(f) => {
            ext.content_hash = Some(f.markdown.clone());
            ext.raw_hash = Some(f.raw_html.clone());
            ext.text_hash = Some(f.text.clone());
        }
        None => ext.content_hash = Some(pagesift_content::hash_hex(doc.markdown.as_bytes())),
    }
}

/// 页面自带的 JSON-LD 原样透传到 `metadata.pagesift.jsonLd`，供下游直接消费。
pub(super) fn fill_structured(ext: &mut PagesiftMeta, doc: &pagesift_content::Document) {
    ext.json_ld = doc.metadata.json_ld().to_vec();
}

pub(super) use pagesift_sink::meta::fill as fill_metadata;

#[allow(clippy::too_many_arguments)] // 把各阶段的结果汇成文档，只在抓取流程末尾调用一处
pub(super) fn finish(
    mut doc: Document,
    mut meta: Metadata,
    mut ext: PagesiftMeta,
    mut timings: BTreeMap<String, f64>,
    mut warnings: Vec<String>,
    raw_bytes: Option<usize>,
    video_fallback: bool,
    started: Instant,
) -> Document {
    for slot in [
        &mut meta.url,
        &mut meta.source_url,
        &mut meta.canonical_url,
        &mut meta.favicon,
    ]
    .into_iter()
    .flatten()
    {
        *slot = strip_url_userinfo_str(slot);
    }
    for hop in &mut ext.redirect_chain {
        *hop = strip_url_userinfo_str(hop);
    }
    timings.insert("total".into(), ms(started.elapsed()));
    if meta.status_code >= 400 {
        warnings.push(format!("http_error_status: HTTP {}", meta.status_code));
    } else if (300..400).contains(&meta.status_code) {
        // 跟随重定向之后仍是 3xx：没有 Location、Location 不可用，或 300 这类需要人工选择的响应。
        warnings.push(format!(
            "http_redirect_status: HTTP {} was not followed (missing or unusable Location)",
            meta.status_code
        ));
    }
    // 只数到阈值为止：4.49 MB 的正文没必要为了一个警告全量扫一遍。
    // [策略] 警告门槛（不改结果，只提示 JS 壳页）：先验包 v0：0.81 的现值，约 80 个英文词
    const THIN_MAX_CHARS: usize = 500;
    let markdown_chars = doc.markdown.as_deref().map(|markdown| {
        let mut count = 0;
        for c in markdown.chars() {
            if !c.is_whitespace() {
                count += 1;
                if count > THIN_MAX_CHARS {
                    break;
                }
            }
        }
        count
    });
    if meta.status_code < 400 && markdown_chars == Some(0) {
        warnings.push(format!(
            "empty_content: no readable text extracted (HTTP {})",
            meta.status_code
        ));
    }
    // "过薄"与"全空"是两回事：页面有实质内容（HTML 不小）却只抽出几百字，
    // 多半是 JS 壳，或者正文选择把主体丢了。只警告，不改结果。
    // [策略] 警告门槛：先验包 v0：0.81 的现值
    const THIN_MIN_HTML_BYTES: usize = 20_000;
    if meta.status_code < 400
        && !video_fallback
        && let (Some(chars @ 1..THIN_MAX_CHARS), Some(bytes)) = (markdown_chars, raw_bytes)
        && bytes >= THIN_MIN_HTML_BYTES
    {
        warnings.push(format!(
            "thin_content: extracted {chars} characters from {} KB of HTML; the page may be a JavaScript shell or the main content was not selected",
            bytes / 1024
        ));
    }
    ext.timings = timings;
    ext.warnings = warnings;
    meta.pagesift = Some(ext);
    doc.metadata = meta;
    doc
}

pub(super) fn ms(d: Duration) -> f64 {
    (d.as_secs_f64() * 1000.0 * 100.0).round() / 100.0
}

/// 判断 `thin_content` 用的页面体积。正文不薄时直接用 HTML 长度，不多扫一遍；
/// 正文薄时单遍零分配扣掉 `<style>` 的内容和不在 `<style>` 内的 `data:` 内嵌资源：
/// 一张 8 MB 的 base64 图片不代表页面“有大量内容却没抽出来”。`<script>` 不扣：
/// 内联脚本大而正文薄正是 JS 壳页的特征。
pub(super) fn thin_basis_bytes(html: &str, markdown: &str) -> usize {
    // [策略] 与 THIN_MAX_CHARS 同值：正文不薄时不必扣减
    const ENOUGH: usize = 500;
    if markdown
        .chars()
        .filter(|c| !c.is_whitespace())
        .take(ENOUGH)
        .count()
        >= ENOUGH
    {
        return html.len();
    }
    const OPEN_STYLE: &[u8] = b"<style";
    const CLOSE_STYLE: &[u8] = b"</style";
    const DATA_URI: &[u8] = b"data:";
    let bytes = html.as_bytes();
    let mut removed = 0usize;
    let mut i = 0usize;
    while i < bytes.len() {
        let b = bytes[i];
        if b == b'<'
            && i + OPEN_STYLE.len() <= bytes.len()
            && bytes[i + 1..i + OPEN_STYLE.len()].eq_ignore_ascii_case(&OPEN_STYLE[1..])
            && matches!(
                bytes.get(i + OPEN_STYLE.len()),
                Some(b' ' | b'\t' | b'\n' | b'\r' | b'\x0c' | b'/' | b'>')
            )
        {
            let mut close_pos = i + OPEN_STYLE.len();
            let end = loop {
                let Some(off) = bytes[close_pos..].windows(CLOSE_STYLE.len()).position(|w| {
                    w[0] == b'<' && w[1] == b'/' && w[2..].eq_ignore_ascii_case(&CLOSE_STYLE[2..])
                }) else {
                    break bytes.len();
                };
                let cand = close_pos + off;
                let after_close = cand + CLOSE_STYLE.len();
                if matches!(
                    bytes.get(after_close),
                    Some(b' ' | b'\t' | b'\n' | b'\r' | b'\x0c' | b'/' | b'>') | None
                ) {
                    break bytes[after_close..]
                        .iter()
                        .position(|&c| c == b'>')
                        .map_or(bytes.len(), |q| after_close + q + 1);
                }
                close_pos = cand + 2;
            };
            removed += end - i;
            i = end;
            continue;
        }
        if (b == b'd' || b == b'D')
            && i + DATA_URI.len() <= bytes.len()
            && bytes[i + 1..i + DATA_URI.len()].eq_ignore_ascii_case(&DATA_URI[1..])
        {
            let after_prefix = i + DATA_URI.len();
            let len = bytes[after_prefix..]
                .iter()
                .position(|&c| {
                    matches!(c, b'"' | b'\'' | b')' | b' ' | b'\t' | b'\n' | b'\r' | b'>')
                })
                .map_or(bytes.len() - i, |p| DATA_URI.len() + p);
            removed += len;
            i += len;
            continue;
        }
        i += 1;
    }
    bytes.len().saturating_sub(removed)
}

/// JSON 响应（`application/json`、`*+json`）不是网页：当 HTML 解析只会剩下碎片。
/// 合法 JSON 格式化后放进代码块并返回已解析的 `Value` 供图谱证据直接复用；
/// 太大的不格式化，原样放进去。不是合法 JSON 时返回 `None`，按文本继续。
pub(super) fn parse_json_markdown(
    content_type: Option<&str>,
    body: &[u8],
) -> Option<(String, serde_json::Value)> {
    // [策略] 安全上限：格式化会把 JSON 放大数倍（缩进、换行），超过它原样输出，防大响应撑爆内存
    const PRETTY_MAX_BYTES: usize = 5 * 1024 * 1024;
    let kind = content_type?.split(';').next()?.trim().to_ascii_lowercase();
    if kind != "application/json" && kind != "text/json" && !kind.ends_with("+json") {
        return None;
    }
    let text = std::str::from_utf8(body.strip_prefix(b"\xef\xbb\xbf").unwrap_or(body)).ok()?;
    let value: serde_json::Value = serde_json::from_str(text).ok()?;
    let rendered = if text.len() <= PRETTY_MAX_BYTES {
        serde_json::to_string_pretty(&value).ok()?
    } else {
        text.trim().to_string()
    };
    // 内容里有三个反引号时加长围栏，保证代码块不被提前闭合。
    let mut fence = String::from("```");
    while rendered.contains(fence.as_str()) {
        fence.push('`');
    }
    Some((format!("{fence}json\n{rendered}\n{fence}\n"), value))
}

#[cfg(test)]
pub(super) fn json_markdown(content_type: Option<&str>, body: &[u8]) -> Option<String> {
    parse_json_markdown(content_type, body).map(|(md, _)| md)
}

/// 魔数优先：`%PDF-` 开头就是 PDF（不管头怎么写）。只有头说是 PDF 时，正文明显是 HTML/XML
/// 的不算——CDN、WAF 把拦截页、登录页标成 `application/pdf` 返回时按网页处理，拦截页识别照常生效。
pub(super) fn is_pdf(body: &[u8], content_type: Option<&str>) -> bool {
    body.starts_with(b"%PDF-")
        || (content_type.is_some_and(|c| c.to_ascii_lowercase().contains("application/pdf"))
            && !looks_like_markup(body))
}

/// 非 HTML/文本内容（图片、音视频、字体、压缩包等）不应该被当作文本抽取。
/// 返回不支持的类型名；`None` 表示可以按文本继续处理。
///
/// Content-Type 可能标错（例如 `application/octet-stream` 实际是 HTML），所以明显的
/// HTML/XML 开头仍然放行；图片/音视频/字体等即使正文碰巧以 `<` 开头也拒绝。
pub(super) fn unsupported_content_type(content_type: Option<&str>, body: &[u8]) -> Option<String> {
    let raw = content_type?;
    let kind = raw.split(';').next()?.trim().to_ascii_lowercase();
    if kind.is_empty() {
        return None;
    }
    if kind.starts_with("image/")
        || kind.starts_with("audio/")
        || kind.starts_with("video/")
        || kind.starts_with("font/")
    {
        return Some(kind);
    }
    // `application/octet-stream`、`binary/octet-stream` 返回的 PDF 很常见：认魔数，交给 PDF 解析。
    if kind == "application/pdf" || body.starts_with(b"%PDF-") {
        return None;
    }
    if kind.starts_with("text/")
        || kind.contains("html")
        || kind.contains("xml")
        || kind.contains("json")
        || matches!(
            kind.as_str(),
            "application/javascript" | "application/x-javascript" | "application/ecmascript"
        )
    {
        return None;
    }
    if looks_like_markup(body) {
        return None;
    }
    Some(kind)
}

/// 声明了受支持的二进制/容器格式但内容嗅探失败时，返回“内容损坏或与声明类型不符”错误，
/// 避免误报 `UNSUPPORTED_OPTION "unsupported content type"`。
pub(super) fn corrupt_declared_binary_error(
    name: &str,
    content_type: Option<&str>,
    body: &[u8],
) -> Option<ApiError> {
    let is_html_ct = content_type.is_some_and(|c| c.to_ascii_lowercase().contains("html"));
    if is_html_ct || looks_like_markup(body) {
        return None;
    }
    pagesift_api::types::UploadKind::corrupt_declared_error(name, content_type)
}

fn looks_like_markup(body: &[u8]) -> bool {
    let head = &body[..body.len().min(1024)];
    let text = String::from_utf8_lossy(head);
    let text = text.trim_start_matches('\u{feff}').trim_start();
    let lower = text.to_ascii_lowercase();
    lower.starts_with("<!doctype html")
        || lower.starts_with("<html")
        || lower.starts_with("<head")
        || lower.starts_with("<body")
        || lower.starts_with("<?xml")
}

pub fn policy_error(e: pagesift_policy::Error) -> ApiError {
    use pagesift_policy::ErrorCode as P;
    let code = match e.code() {
        P::RobotsDisallowed => ErrorCode::RobotsDisallowed,
        P::AccessBarrier => ErrorCode::AccessBarrier,
        P::RateLimited => ErrorCode::RateLimited,
        P::HostCooldown => ErrorCode::HostCooldown,
        P::Timeout => ErrorCode::Timeout,
        P::SsrfBlocked => ErrorCode::SsrfBlocked,
        P::TooLarge => ErrorCode::TooLarge,
        P::UpstreamError => ErrorCode::UpstreamError,
        P::UnsupportedOption => ErrorCode::UnsupportedOption,
        P::InvalidRequest => ErrorCode::InvalidRequest,
        P::DnsFailure => ErrorCode::DnsFailure,
        P::NetworkError => ErrorCode::NetworkError,
        P::TlsCertExpired => ErrorCode::TlsCertExpired,
        P::TlsHostnameMismatch => ErrorCode::TlsHostnameMismatch,
        P::TlsUntrustedRoot => ErrorCode::TlsUntrustedRoot,
        P::TlsError => ErrorCode::TlsError,
        _ => ErrorCode::Internal,
    };
    let retry = e.retry_after().map(|d| d.as_secs().max(1));
    let mut err = ApiError::new(code, e.to_string());
    if let Some(s) = retry {
        err = err.with_retry_after(s);
    }
    err
}

#[cfg(test)]
mod tests;
