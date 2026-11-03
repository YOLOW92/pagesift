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

pub(super) async fn process_html(
    html: String,
    base: &Url,
    status: Option<u16>,
    opts: &ScrapeOptions,
) -> Result<(pagesift_content::Document, String), ApiError> {
    let copts = content_options(base, status, opts);
    let tags = tag_filter(opts)?;
    cpu_task(move || {
        // RSS / Atom / RDF feed 不是网页：按条目解析，别让 HTML 提取器把标签剥成一锅粥。
        if let Some(document) = super::feed::parse(&html, copts.links) {
            return (document, html);
        }
        // includeTags / excludeTags 只影响提取；rawHtml 仍是原始页面
        let filtered = tags.map(|t| t.apply(&html));
        let source = filtered.as_deref().unwrap_or(&html);
        let document = CONTENT_WORKSPACE.with(|workspace| {
            pagesift_content::process_with_workspace(source, &copts, &mut workspace.borrow_mut())
        });
        (document, html)
    })
    .await
}

/// 提取参数：只按请求要的格式算（链接、图片、清洗后的 HTML、变化检测的指纹）。
pub(super) fn content_options(
    base: &Url,
    status: Option<u16>,
    opts: &ScrapeOptions,
) -> pagesift_content::Options {
    pagesift_content::Options {
        base_url: Some(base.clone()),
        only_main_content: opts.only_main_content,
        status,
        detect_barrier: true,
        clean_html: opts.wants("html"),
        links: opts.wants("links"),
        images: opts.wants("images"),
        // 完整指纹（原始 HTML 的哈希、再渲染一遍纯文本做规范化哈希）约占提取 CPU 的一成，只在变化检测要用时算
        fingerprints: opts.wants("changeTracking"),
        ..Default::default()
    }
}

/// `includeTags` / `excludeTags` 的筛选规则；都没给时为 `None`，选择器写错时返回 `INVALID_REQUEST`。
pub fn tag_filter(opts: &ScrapeOptions) -> Result<Option<pagesift_content::TagFilter>, ApiError> {
    pagesift_content::TagFilter::new(&opts.include_tags, &opts.exclude_tags)
        .map_err(ApiError::invalid)
}

pub(super) fn check_barrier(doc: &pagesift_content::Document) -> Result<(), ApiError> {
    let Some(b) = &doc.barrier else { return Ok(()) };
    Err(ApiError::new(
        ErrorCode::AccessBarrier,
        format!(
            "the site returned an access barrier ({} {}); pagesift reports it and does not retry to get past it",
            b.vendor,
            b.kind.as_str()
        ),
    )
    .with_details(serde_json::json!({
        "vendor": b.vendor,
        "kind": b.kind.as_str(),
        "confidence": b.confidence,
        "ruleId": b.rule_id,
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
/// 正文薄时扣掉 `<style>` 的内容和 `data:` 内嵌资源：一张 8 MB 的 base64 图片
/// 不代表页面“有大量内容却没抽出来”。`<script>` 不扣：内联脚本大而正文薄正是 JS 壳页的特征。
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
    let lower = html.to_ascii_lowercase();
    let mut removed = 0usize;
    for tag in ["style"] {
        let (open, close) = (format!("<{tag}"), format!("</{tag}"));
        let mut at = 0;
        while let Some(i) = lower[at..].find(&open) {
            let start = at + i;
            let Some(j) = lower[start..].find(&close) else {
                removed += lower.len() - start;
                break;
            };
            removed += j;
            at = start + j + close.len();
        }
    }
    let mut at = 0;
    while let Some(i) = lower[at..].find("data:") {
        let start = at + i;
        let len = lower[start..]
            .find(['"', '\'', ')', ' ', '>'])
            .unwrap_or(lower.len() - start);
        removed += len;
        at = start + len.max(1);
    }
    html.len().saturating_sub(removed)
}

/// JSON 响应（`application/json`、`*+json`）不是网页：当 HTML 解析只会剩下碎片。
/// 合法 JSON 格式化后放进代码块；太大的不格式化，原样放进去。不是合法 JSON 时返回 `None`，按文本继续。
pub(super) fn json_markdown(content_type: Option<&str>, body: &[u8]) -> Option<String> {
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
    Some(format!("{fence}json\n{rendered}\n{fence}\n"))
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
mod tests {
    use super::{is_pdf, json_markdown, thin_basis_bytes, unsupported_content_type};

    #[test]
    fn thin_basis_ignores_inline_payloads() {
        let image = format!(
            "<p>short</p><img src=\"data:image/png;base64,{}\">",
            "A".repeat(100_000)
        );
        assert!(thin_basis_bytes(&image, "short") < 1_000);
        let style = format!("<STYLE>{}</STYLE><p>x</p>", "y".repeat(50_000));
        assert!(thin_basis_bytes(&style, "x") < 1_000);
        let next = format!(
            "<script id=\"__NEXT_DATA__\">{}</script><div id=\"app\"></div>",
            "x".repeat(50_000)
        );
        assert_eq!(thin_basis_bytes(&next, ""), next.len());
        let shell = format!("<div>{}</div>", "<span></span>".repeat(5_000));
        assert_eq!(thin_basis_bytes(&shell, ""), shell.len());
        let long = "word ".repeat(200);
        assert_eq!(thin_basis_bytes(&image, &long), image.len());
    }

    #[test]
    fn json_responses_become_code_blocks() {
        let md = json_markdown(
            Some("application/json; charset=utf-8"),
            br#"{"html":"<h1>x</h1>","n":[1,2]}"#,
        )
        .unwrap();
        assert!(md.starts_with("```json\n{"), "{md}");
        assert!(md.contains("\"html\": \"<h1>x</h1>\""), "{md}");
        assert!(md.ends_with("\n```\n"), "{md}");
        let md = json_markdown(Some("application/ld+json"), br#"{"a":"```"}"#).unwrap();
        assert!(md.starts_with("````json"), "{md}");
        assert_eq!(json_markdown(Some("text/html"), b"{}"), None);
        assert_eq!(json_markdown(Some("application/json"), b"<html>"), None);
    }

    #[test]
    fn rejects_binary_content_types() {
        for kind in [
            "image/png",
            "image/jpeg",
            "image/svg+xml",
            "audio/mpeg",
            "video/mp4",
            "font/woff2",
            "application/zip",
            "application/octet-stream",
        ] {
            assert_eq!(
                unsupported_content_type(Some(kind), b"\x89PNG\r\n"),
                Some(kind.to_string()),
                "{kind}"
            );
        }
    }

    #[test]
    fn accepts_text_and_mislabeled_markup() {
        for kind in [
            "text/html; charset=utf-8",
            "text/plain",
            "application/xhtml+xml",
            "application/json",
            "application/rss+xml",
            "application/pdf",
        ] {
            assert_eq!(
                unsupported_content_type(Some(kind), b"anything"),
                None,
                "{kind}"
            );
        }
        assert_eq!(
            unsupported_content_type(Some("application/octet-stream"), b"<!doctype html><p>x"),
            None
        );
        assert_eq!(
            unsupported_content_type(Some("application/octet-stream"), b"  <html><p>x"),
            None
        );
        assert_eq!(unsupported_content_type(None, b"\x89PNG"), None);
        for kind in ["application/octet-stream", "binary/octet-stream"] {
            assert_eq!(
                unsupported_content_type(Some(kind), b"%PDF-1.7\n1 0 obj"),
                None
            );
            assert!(is_pdf(b"%PDF-1.7\n1 0 obj", Some(kind)));
        }
        // 头说是 PDF：正文是网页（拦截页、登录页）时按网页处理；不是网页的照样交给 PDF 解析
        let pdf = Some("application/pdf");
        assert!(!is_pdf(
            b"<!DOCTYPE html><html><body>Access denied</body></html>",
            pdf
        ));
        assert!(!is_pdf(b"\n  <html><body>x</body></html>", pdf));
        assert!(is_pdf(b"junk before the header %PDF-1.4", pdf));
        assert!(is_pdf(b"%PDF-1.4", Some("text/html")));
        assert_eq!(
            unsupported_content_type(Some("application/octet-stream"), b"\x00\x01binary"),
            Some("application/octet-stream".into())
        );
    }
}
