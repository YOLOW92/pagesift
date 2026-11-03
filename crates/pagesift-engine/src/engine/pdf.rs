//! PDF → 文档：抓到的 PDF（`/v2/scrape`）和上传的 PDF（`/v2/parse`）走同一段代码，
//! `parsers` 的含义、错误码、时限都一样。选项照 Firecrawl 的 `parsers: [{type: "pdf", ...}]`。

use pagesift_api::types::{PageMarkdown, PdfParser};

use super::*;

impl Scraper {
    /// 解析 PDF 字节，组装成和 scrape 相同的文档。`deadline` 是整个请求的截止时间（没有时解析时限全由 pagesift-pdf 按文件大小估计）。
    /// `parsers` 里没有 `pdf` 时不解析，和 Firecrawl 一样把原文件按 base64 放进 `markdown`/`html`。
    pub(super) async fn pdf_document(
        &self,
        bytes: Vec<u8>,
        opts: &ScrapeOptions,
        (mut meta, mut ext): (Metadata, PagesiftMeta),
        mut p: Progress,
        deadline: Option<Instant>,
    ) -> Result<Document, ApiError> {
        let parser = PdfParser::from_parsers(opts.parsers.as_ref())?;
        if parser.raw {
            let encoded = base64(&bytes);
            drop(bytes);
            let doc = Document {
                html: opts.wants("html").then(|| encoded.clone()),
                markdown: opts.wants("markdown").then_some(encoded),
                ..Default::default()
            };
            return Ok(finish(
                doc, meta, ext, p.timings, p.warnings, None, false, p.started,
            ));
        }
        // Firecrawl 的三种模式：fast 只读文字层；auto（默认）扫描页才 OCR；ocr 强制 OCR。
        // OCR 要服务端先打开（`[ocr] enabled`，打开才会下载模型），按请求只能选用不能开启
        let force_ocr = parser.mode.as_deref() == Some("ocr");
        if force_ocr && self.ocr.is_none() {
            return Err(ApiError::unsupported(
                "parsers[].mode \"ocr\" needs OCR turned on on this server: [ocr] enabled = true or --ocr",
            ));
        }
        let ocr = match parser.mode.as_deref() {
            Some("fast") => None,
            _ => self.ocr.clone(),
        };
        let engine = ocr.clone();
        let stage = self
            .policy
            .fetcher()
            .acquire_stage(pagesift_net::NetworkStage::Parse)
            .await;
        let t = Instant::now();
        let page_limit = parser.max_pages;
        // `exportImages`：识别全部图片、Markdown 按阅读顺序引用 `images/<id>`、按原编码导出
        let export = opts.export_images;
        // 字节移进解析任务，解析完就释放；时限从拿到 CPU 名额时算剩下多少
        // 解析时限由 pagesift-pdf 按"字节数 → 耗时"的回归给出（0.88），请求的截止时间只作硬上限
        let result = cpu_task(move || {
            let popts = pagesift_pdf::Options {
                timeout: deadline.map(|d| d.saturating_duration_since(Instant::now())),
                // 按字节数估的时限没算 OCR 的耗时：开着 OCR 时只看请求的截止时间
                adaptive_timeout: ocr.is_none(),
                force_ocr,
                ocr,
                page_limit,
                images: pagesift_pdf::ImageOptions {
                    list: export,
                    markdown: export,
                    export,
                },
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
        if pdf.scanned && parser.mode.as_deref() == Some("fast") {
            // Firecrawl 在 fast 模式遇到扫描件也是报错（SCRAPE_PDF_OCR_REQUIRED），不返回空正文
            return Err(ApiError::unsupported(
                "this PDF is scanned and needs OCR; parsers mode \"fast\" reads the text layer only",
            ));
        }
        let scanned_pages = pdf.pages.iter().filter(|p| p.scanned).count();
        let ocr_pages = pdf.pages.iter().filter(|p| p.ocr).count();
        match engine {
            Some(engine) => {
                let info = engine.info();
                if let Some(error) = &info.error {
                    p.warnings.push(format!("ocr_failed: {error}"));
                }
                if let Some(reason) = &info.fallback {
                    p.warnings.push(format!("gpu_fallback: {reason}"));
                }
                if ocr_pages > 0 {
                    ext.ocr = Some(pagesift_api::types::OcrMeta {
                        engine: info.engine,
                        model: info.model,
                        device: info.device,
                        pages: ocr_pages,
                        fallback: info.fallback.is_some(),
                    });
                }
            }
            None if scanned_pages > 0 => p.warnings.push(format!(
                "scanned_pages_without_ocr: {scanned_pages} of {} pages are scanned images with no text layer; turn on OCR with [ocr] enabled = true or --ocr",
                pdf.pages.len()
            )),
            None => {}
        }
        let total = pdf.metadata.page_count;
        let converted = pdf.pages.len();
        if converted < total {
            p.warnings.push(format!(
                "max_pages: converted the first {converted} of {total} pages"
            ));
        }
        // 损坏的文件部分恢复：pdf_repaired、pages_missing
        p.warnings.extend(pdf.warnings);
        meta.title = pdf.metadata.title.clone();
        // /Info 没有标题时用版面推断的标题（第一页的一级标题），并标明来源
        if meta.title.is_none()
            && let Some(title) = pdf.inferred_title
        {
            meta.title = Some(title);
            meta.extra.insert("titleSource".into(), "inferred".into());
        }
        // 和 Firecrawl 一样：numPages 是实际转换的页数，totalPages 是整个文件的页数
        meta.extra.insert("numPages".into(), converted.into());
        meta.extra.insert("totalPages".into(), total.into());
        let mut attachments = Vec::new();
        for img in pdf.images {
            match img.data {
                Some(data) => attachments.push(pagesift_api::types::Attachment::with_hash(
                    img.hash,
                    img.extension,
                    data,
                    None,
                )),
                None if export => p.warnings.push(format!(
                    "image_not_exported: {}: {}",
                    img.hash.get(..32).unwrap_or(&img.hash),
                    img.export_error
                        .as_deref()
                        .unwrap_or("unsupported encoding")
                )),
                None => {}
            }
        }
        let pages = parser.pages.then(|| {
            pdf.pages
                .into_iter()
                .map(|page| PageMarkdown {
                    page_number: page.number,
                    markdown: page.markdown,
                })
                .collect()
        });
        Ok(finish(
            Document {
                markdown: opts.wants("markdown").then_some(pdf.markdown),
                pages,
                attachments,
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
}

/// PDF 读不了是文件本身的问题（4xx），不是上游故障：坏文件 400 INVALID_REQUEST，
/// 加密 400 UNSUPPORTED_OPTION，超限 413，解析超时 408。
pub(super) fn pdf_error(e: pagesift_pdf::Error) -> ApiError {
    use pagesift_pdf::Error as E;
    let code = match e {
        E::Encrypted => ErrorCode::UnsupportedOption,
        E::TooLarge(_) => ErrorCode::TooLarge,
        E::Timeout => ErrorCode::Timeout,
        _ => ErrorCode::InvalidRequest,
    };
    ApiError::new(code, format!("could not read PDF: {e}"))
}

/// 标准 base64（RFC 4648，带补位）。只在 `parsers: []` 时用一次，不值得为它加依赖。
fn base64(bytes: &[u8]) -> String {
    const T: &[u8; 64] = b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789+/";
    let mut out = String::with_capacity(bytes.len().div_ceil(3) * 4);
    for c in bytes.chunks(3) {
        let n = (u32::from(c[0]) << 16)
            | (u32::from(*c.get(1).unwrap_or(&0)) << 8)
            | u32::from(*c.get(2).unwrap_or(&0));
        for i in 0..4 {
            if i <= c.len() {
                out.push(T[(n >> (18 - 6 * i) & 63) as usize] as char);
            } else {
                out.push('=');
            }
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn base64_matches_rfc4648_vectors() {
        for (input, want) in [
            ("", ""),
            ("f", "Zg=="),
            ("fo", "Zm8="),
            ("foo", "Zm9v"),
            ("foob", "Zm9vYg=="),
            ("fooba", "Zm9vYmE="),
            ("foobar", "Zm9vYmFy"),
        ] {
            assert_eq!(base64(input.as_bytes()), want);
        }
        assert_eq!(base64(&[0xff, 0xfe, 0x00]), "//4A");
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
