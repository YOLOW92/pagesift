//! PDF → 文档：抓到的 PDF（`/v2/scrape`）和上传的 PDF（`/v2/parse`）走同一段代码，
//! `parsers` 的含义、错误码、时限都一样。选项照 Firecrawl 的 `parsers: [{type: "pdf", ...}]`。

use pagesift_api::types::{PageMarkdown, PdfParser};

use super::*;

/// 没给 `timeout` 时一份 PDF 的解析时限（和 pagesift-pdf 的默认值相同）。
const DEFAULT_PDF_TIMEOUT: Duration = Duration::from_secs(30);

impl Scraper {
    /// 解析 PDF 字节，组装成和 scrape 相同的文档。`deadline` 是整个请求的截止时间（没有就用默认时限）。
    /// `parsers` 里没有 `pdf` 时不解析，和 Firecrawl 一样把原文件按 base64 放进 `markdown`/`html`。
    pub(super) async fn pdf_document(
        &self,
        bytes: Vec<u8>,
        opts: &ScrapeOptions,
        (mut meta, ext): (Metadata, PagesiftMeta),
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
        let stage = self
            .policy
            .fetcher()
            .acquire_stage(pagesift_net::NetworkStage::Parse)
            .await;
        let t = Instant::now();
        let page_limit = parser.max_pages;
        // 字节移进解析任务，解析完就释放；时限从拿到 CPU 名额时算剩下多少
        let result = cpu_task(move || {
            let timeout = match deadline {
                Some(d) => d.saturating_duration_since(Instant::now()),
                None => DEFAULT_PDF_TIMEOUT,
            };
            let popts = pagesift_pdf::Options {
                timeout: Some(timeout),
                page_limit,
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
            if parser.mode.as_deref() == Some("fast") {
                // Firecrawl 在 fast 模式遇到扫描件也是报错（SCRAPE_PDF_OCR_REQUIRED），不返回空正文
                return Err(ApiError::unsupported(
                    "this PDF is scanned and needs OCR, which pagesift does not support; parsers mode \"fast\" reads the text layer only",
                ));
            }
            p.warnings
                .push("scanned PDF: pages have no text layer and OCR is not supported".into());
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
