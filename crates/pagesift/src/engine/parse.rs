//! `/v2/parse`：解析调用方上传的文件。PDF 走和抓取相同的 pagesift-pdf，HTML 走相同的正文提取，
//! 组装成和 scrape 相同的文档。不联网，所以没有缓存、robots、浏览器渲染这些环节；
//! CPU 照样经过解析阶段的闸门和 [`cpu_task`] 的核数限制。

use pagesift_api::types::{PageMarkdown, PdfParser, Upload, UploadKind};

use super::*;

/// 选项里没给 `timeout` 时的处理时限，和 Firecrawl `/parse` 的默认值一样。
const DEFAULT_PARSE_TIMEOUT: Duration = Duration::from_secs(30);

impl Scraper {
    /// 解析上传的文件并写审计记录（`kind: parse`）。不存快照：上传的文件往往不公开。
    pub async fn parse(&self, upload: Upload, opts: &ScrapeOptions) -> Result<Document, ApiError> {
        let Some(audit) = &self.audit else {
            return self.scrape_inner(Source::Upload(upload), opts).await;
        };
        let name = format!("upload:{}", upload.filename);
        let size = upload.bytes.len();
        let started = Instant::now();
        let result = self.scrape_inner(Source::Upload(upload), opts).await;
        audit.record_parse(&name, size, opts, &result, started.elapsed());
        result
    }

    /// 上传文件的基础文档（正文、元数据），相当于抓取时的 `scrape_cached`。
    pub(super) async fn parse_upload(
        &self,
        upload: Upload,
        opts: &ScrapeOptions,
    ) -> Result<Document, ApiError> {
        let started = Instant::now();
        let deadline = started
            + opts
                .timeout
                .map_or(DEFAULT_PARSE_TIMEOUT, Duration::from_millis);
        let content_type = match upload.kind {
            UploadKind::Html => upload
                .content_type
                .filter(|c| c.to_ascii_lowercase().contains("html")),
            UploadKind::Pdf => None,
        };
        let meta = Metadata {
            source_url: Some(upload.filename.clone()),
            status_code: 200,
            content_type: Some(
                content_type
                    .clone()
                    .unwrap_or_else(|| upload.kind.mime().into()),
            ),
            proxy_used: "basic".into(),
            ..Default::default()
        };
        let ext = PagesiftMeta {
            engine: Some("upload".into()),
            ..Default::default()
        };
        let progress = Work {
            started,
            timings: BTreeMap::new(),
            warnings: Vec::new(),
        };
        match upload.kind {
            UploadKind::Pdf => {
                self.parse_pdf(upload.bytes, opts, meta, ext, progress, deadline)
                    .await
            }
            UploadKind::Html => {
                let html = pagesift_net::decode_owned(upload.bytes, content_type.as_deref(), None);
                self.parse_html(
                    html,
                    &upload.filename,
                    opts,
                    (meta, ext),
                    progress,
                    deadline,
                )
                .await
            }
        }
    }

    async fn parse_pdf(
        &self,
        bytes: Vec<u8>,
        opts: &ScrapeOptions,
        mut meta: Metadata,
        ext: PagesiftMeta,
        mut p: Work,
        deadline: Instant,
    ) -> Result<Document, ApiError> {
        let parser = PdfParser::from_parsers(opts.parsers.as_ref())?;
        let stage = self
            .policy
            .fetcher()
            .acquire_stage(pagesift_net::NetworkStage::Parse)
            .await;
        let t = Instant::now();
        // 字节移进解析任务，解析完就释放；时限从拿到 CPU 名额时算剩下多少
        let result = cpu_task(move || {
            let popts = pagesift_pdf::Options {
                timeout: Some(deadline.saturating_duration_since(Instant::now())),
                ..Default::default()
            };
            pagesift_pdf::extract_with(&bytes, &popts)
        })
        .await?;
        let pdf = match result {
            Ok(pdf) => {
                stage.finish(pagesift_net::StageOutcome::Success);
                pdf
            }
            Err(error) => {
                stage.finish(pagesift_net::StageOutcome::DeterministicFailure);
                return Err(pdf_error(error));
            }
        };
        p.timings.insert("pdf".into(), ms(t.elapsed()));
        if pdf.scanned {
            p.warnings
                .push("scanned PDF: pages have no text layer and OCR is not supported".into());
        }
        meta.title = pdf.metadata.title.clone();
        meta.extra
            .insert("numPages".into(), pdf.metadata.page_count.into());
        let total = pdf.pages.len();
        let keep = parser.max_pages.filter(|&n| n < total);
        let markdown = match keep {
            Some(n) => {
                p.warnings.push(format!(
                    "max_pages: converted the first {n} of {total} pages"
                ));
                let kept: Vec<&str> = pdf.pages[..n]
                    .iter()
                    .map(|page| page.markdown.trim())
                    .filter(|md| !md.is_empty())
                    .collect();
                kept.join("\n\n")
            }
            None => pdf.markdown,
        };
        let pages = parser.pages.then(|| {
            pdf.pages
                .into_iter()
                .take(keep.unwrap_or(total))
                .map(|page| PageMarkdown {
                    page_number: page.number,
                    markdown: page.markdown,
                })
                .collect()
        });
        Ok(finish(
            Document {
                markdown: opts.wants("markdown").then_some(markdown),
                pages,
                ..Default::default()
            },
            meta,
            ext,
            p.timings,
            p.warnings,
            None,
            false,
            p.started,
        ))
    }

    async fn parse_html(
        &self,
        decoded: pagesift_net::Decoded,
        filename: &str,
        opts: &ScrapeOptions,
        (mut meta, mut ext): (Metadata, PagesiftMeta),
        mut p: Work,
        deadline: Instant,
    ) -> Result<Document, ApiError> {
        if let Some(warning) = decoded.warning() {
            p.warnings.push(warning);
        }
        let base = upload_base(filename);
        let stage = self
            .policy
            .fetcher()
            .acquire_stage(pagesift_net::NetworkStage::Parse)
            .await;
        let t = Instant::now();
        let work = process_html(decoded.text, &base, Some(200), opts);
        let (doc, html) = match tokio::time::timeout_at(deadline.into(), work).await {
            Ok(Ok(result)) => {
                stage.finish(pagesift_net::StageOutcome::Success);
                result
            }
            Ok(Err(error)) => {
                stage.finish(pagesift_net::StageOutcome::DeterministicFailure);
                return Err(error);
            }
            Err(_) => {
                stage.finish(pagesift_net::StageOutcome::DeterministicFailure);
                return Err(ApiError::new(
                    ErrorCode::Timeout,
                    "HTML extraction did not finish within the timeout",
                ));
            }
        };
        p.timings.insert("extract".into(), ms(t.elapsed()));
        if doc.metadata.video_fallback {
            p.warnings.push(
                "video_metadata: no readable body text; used the page's embedded video metadata"
                    .into(),
            );
        }
        // 抓取时拦截页要报错；上传的文件是调用方自己给的，照样提取，只提醒一句
        if doc.barrier.is_some() {
            p.warnings.push(
                "access_barrier: the uploaded HTML looks like a bot-check or access-barrier page, not the content"
                    .into(),
            );
        }
        fill_metadata(&mut meta, &doc.metadata);
        // 页面没写图标时提取器按惯例猜 `/favicon.ico`，相对于 file:/// 毫无意义
        if meta
            .favicon
            .as_deref()
            .is_some_and(|f| f.starts_with("file:"))
        {
            meta.favicon = None;
        }
        fill_structured(&mut ext, &doc);
        fill_fingerprints(&mut ext, &doc);
        let raw_bytes = Some(thin_basis_bytes(&html, &doc.markdown));
        let video_fallback = doc.metadata.video_fallback;
        Ok(finish(
            document(doc, opts, html),
            meta,
            ext,
            p.timings,
            p.warnings,
            raw_bytes,
            video_fallback,
            p.started,
        ))
    }
}

/// 一次解析的起始时间和沿途记下的耗时、警告。
struct Work {
    started: Instant,
    timings: BTreeMap<String, f64>,
    warnings: Vec<String>,
}

/// 上传 HTML 里相对链接的基准：没有 `<base href>` 时按 `file:///<文件名>` 解析，
/// 不冒充任何真实网址。
fn upload_base(filename: &str) -> Url {
    let mut base = Url::parse("file:///").expect("static URL");
    base.set_path(filename);
    base
}

/// 上传的 PDF 读不了是调用方的文件问题（4xx），不是上游错误。
fn pdf_error(e: pagesift_pdf::Error) -> ApiError {
    use pagesift_pdf::Error as E;
    let code = match e {
        E::Encrypted => ErrorCode::UnsupportedOption,
        E::TooLarge(_) => ErrorCode::TooLarge,
        E::Timeout => ErrorCode::Timeout,
        _ => ErrorCode::InvalidRequest,
    };
    ApiError::new(code, format!("could not read PDF: {e}"))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn upload_base_resolves_relative_links_locally() {
        let base = upload_base("my report.html");
        assert_eq!(base.as_str(), "file:///my%20report.html");
        assert_eq!(
            base.join("img/a.png").unwrap().as_str(),
            "file:///img/a.png"
        );
    }

    #[test]
    fn pdf_errors_are_caller_errors() {
        use pagesift_pdf::Error as E;
        for (e, code) in [
            (E::Encrypted, ErrorCode::UnsupportedOption),
            (E::TooLarge("page count"), ErrorCode::TooLarge),
            (E::Timeout, ErrorCode::Timeout),
            (E::Malformed("x".into()), ErrorCode::InvalidRequest),
        ] {
            assert_eq!(pdf_error(e).code, code);
        }
    }
}
