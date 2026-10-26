use super::*;

thread_local! {
    /// Tokio blocking workers are long-lived. Reuse content-pipeline capacity
    /// on the worker that first-touched it instead of allocating DOM/index
    /// vectors for every page and freeing them on a potentially remote arena.
    static CONTENT_WORKSPACE: std::cell::RefCell<pagesift_content::ProcessWorkspace> =
        std::cell::RefCell::new(pagesift_content::ProcessWorkspace::default());
}

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
pub(crate) fn chunk_markdown(
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
    let copts = pagesift_content::Options {
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
    };
    cpu_task(move || {
        let document = CONTENT_WORKSPACE.with(|workspace| {
            pagesift_content::process_with_workspace(&html, &copts, &mut workspace.borrow_mut())
        });
        (document, html)
    })
    .await
}

/// 同时进行的 CPU 密集任务（正文提取、PDF 解析）不超过核数。
/// 不加限制时，每个并发请求都会占一个阻塞线程，几十个线程各自持有一块 malloc arena，
/// 释放的内存分散在各处还不回系统，峰值内存成倍增加；超过核数的并发也换不来吞吐。
pub(super) static CPU_SLOTS: std::sync::LazyLock<tokio::sync::Semaphore> =
    std::sync::LazyLock::new(|| {
        tokio::sync::Semaphore::new(std::thread::available_parallelism().map_or(4, |n| n.get()))
    });

pub(super) async fn cpu_task<T: Send + 'static>(
    f: impl FnOnce() -> T + Send + 'static,
) -> Result<T, ApiError> {
    let _slot = CPU_SLOTS.acquire().await.expect("semaphore never closed");
    tokio::task::spawn_blocking(f)
        .await
        .map_err(|e| ApiError::new(ErrorCode::Internal, e.to_string()))
}

pub(super) fn check_barrier(doc: &pagesift_content::Document) -> Result<(), ApiError> {
    let Some(b) = &doc.barrier else { return Ok(()) };
    Err(ApiError::new(
        ErrorCode::AccessBarrier,
        format!(
            "the site returned an access barrier ({} {}); pagesift does not bypass it",
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

pub(super) fn fill_metadata(meta: &mut Metadata, m: &pagesift_content::Metadata) {
    meta.title = m.title.clone();
    meta.description = m.description.clone();
    meta.language = m.language.clone();
    meta.keywords = m.keywords.clone();
    meta.robots = m.robots.clone();
    meta.favicon = m.favicon.clone();
    meta.published_time = m.published_time.clone();
    meta.author = m.author.clone();
    meta.modified_time = m.modified_time.clone();
    for (k, v) in &m.extra {
        meta.extra
            .entry(firecrawl_meta_key(k))
            .or_insert_with(|| v.clone().into());
    }
}

/// `og:image` → `ogImage`、`article:published_time` → `articlePublishedTime`，和 Firecrawl 的键名风格一致。
pub(super) fn firecrawl_meta_key(raw: &str) -> String {
    let mut out = String::with_capacity(raw.len());
    let mut upper = false;
    for c in raw.chars() {
        if matches!(c, ':' | '_' | '-' | '.') {
            upper = true;
        } else if upper {
            out.extend(c.to_uppercase());
            upper = false;
        } else {
            out.push(c);
        }
    }
    out
}

pub(super) fn finish(
    mut doc: Document,
    mut meta: Metadata,
    mut ext: PagesiftMeta,
    mut timings: BTreeMap<String, f64>,
    mut warnings: Vec<String>,
    started: Instant,
) -> Document {
    timings.insert("total".into(), ms(started.elapsed()));
    if meta.status_code >= 400 {
        warnings.push(format!("http_error_status: HTTP {}", meta.status_code));
    }
    if meta.status_code < 400
        && doc
            .markdown
            .as_deref()
            .is_some_and(|markdown| markdown.trim().is_empty())
    {
        warnings.push(format!(
            "empty_content: no readable text extracted (HTTP {})",
            meta.status_code
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

pub(super) fn is_pdf(body: &[u8], content_type: Option<&str>) -> bool {
    body.starts_with(b"%PDF-")
        || content_type.is_some_and(|c| c.to_ascii_lowercase().contains("application/pdf"))
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
    if kind == "application/pdf" {
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

pub(crate) fn policy_error(e: pagesift_policy::Error) -> ApiError {
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
    use super::unsupported_content_type;

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
    }
}
