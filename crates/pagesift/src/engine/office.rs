//! Word、Excel、PowerPoint、EPUB、CSV → 文档：抓到的（`/v2/scrape`）和上传的（`/v2/parse`）
//! 走同一段代码。转换在 pagesift-office 里；EPUB 的 XHTML 章节交给和网页相同的 HTML 提取器
//! （保留整页，不做正文筛选：书的每一章都是正文）。

use pagesift_office::{Converted, Format};

use super::*;

/// 没给 `timeout` 时一份文档的转换时限。
const DEFAULT_OFFICE_TIMEOUT: Duration = Duration::from_secs(30);

/// 把一份 Office/EPUB/CSV 文档转成 Markdown，供引擎和评测共用。`deadline` 到了报超时。
/// CSV 的字符集按正文探测；知道 Content-Type 时用 [`office_markdown_from`]。
pub fn office_markdown(
    bytes: &[u8],
    format: Format,
    deadline: Option<Instant>,
) -> Result<Converted, pagesift_office::Error> {
    office_markdown_from(bytes, format, None, deadline)
}

/// 同 [`office_markdown`]，另给文件来源的 Content-Type（响应头或上传时的）：CSV 按它的
/// `charset` 解码，和网页一样；声明和正文对不上时纠正并给 `decode:` 警告。
pub fn office_markdown_from(
    bytes: &[u8],
    format: Format,
    content_type: Option<&str>,
    deadline: Option<Instant>,
) -> Result<Converted, pagesift_office::Error> {
    let html = |xhtml: &str, path: &str| {
        let base = Url::parse("file:///")
            .ok()
            .and_then(|b| b.join(&path.replace(' ', "%20")).ok());
        let opts = pagesift_content::Options {
            base_url: base,
            only_main_content: false,
            detect_barrier: false,
            ..Default::default()
        };
        pagesift_content::process(xhtml, &opts).markdown
    };
    let opts = pagesift_office::Options {
        deadline,
        html: Some(&html),
        ..Default::default()
    };
    if format == Format::Csv {
        let (text, warning) = csv_text::decode(bytes, content_type);
        let mut doc = pagesift_office::convert_csv_text(&text, &opts)?;
        doc.warnings.extend(warning);
        return Ok(doc);
    }
    pagesift_office::convert(bytes, format, &opts)
}

/// 文件名或网址路径的扩展名、Content-Type、内容一起判断是不是 Office/EPUB/CSV。
/// 服务器明说是 HTML 的不当 CSV（`/data.csv` 返回的也可能是网页）。
pub(super) fn office_format(body: &[u8], name: &str, content_type: Option<&str>) -> Option<Format> {
    let html = content_type.is_some_and(|c| c.to_ascii_lowercase().contains("html"));
    Format::detect(body, Some(name), content_type).filter(|f| !(html && *f == Format::Csv))
}

/// 一份待转换的文档：字节、格式，以及来源的 Content-Type（响应头或上传字段；CSV 按它的 `charset` 解码）。
pub(super) struct OfficeInput {
    pub(super) bytes: Vec<u8>,
    pub(super) format: Format,
    pub(super) content_type: Option<String>,
}

impl Scraper {
    pub(super) async fn office_document(
        &self,
        input: OfficeInput,
        opts: &ScrapeOptions,
        (mut meta, ext): (Metadata, PagesiftMeta),
        mut p: Progress,
        deadline: Option<Instant>,
    ) -> Result<Document, ApiError> {
        let stage = self
            .policy
            .fetcher()
            .acquire_stage(pagesift_net::NetworkStage::Parse)
            .await;
        let t = Instant::now();
        let format = input.format;
        let result = cpu_task(move || {
            let deadline = deadline.unwrap_or_else(|| Instant::now() + DEFAULT_OFFICE_TIMEOUT);
            office_markdown_from(
                &input.bytes,
                input.format,
                input.content_type.as_deref(),
                Some(deadline),
            )
        })
        .await?;
        let doc = match result {
            Ok(doc) => {
                stage.finish(pagesift_net::StageOutcome::Success);
                doc
            }
            Err(error) => {
                stage.finish(pagesift_net::StageOutcome::DeterministicFailure);
                return Err(office_error(format, error));
            }
        };
        p.timings.insert(format.extension().into(), ms(t.elapsed()));
        p.warnings.extend(doc.warnings);
        meta.title = doc.title;
        Ok(finish(
            Document {
                markdown: opts.wants("markdown").then_some(doc.markdown),
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

/// 文件本身的问题是 4xx：坏文件 400 INVALID_REQUEST，加密或用到不支持的东西 400 UNSUPPORTED_OPTION，
/// 超限 413，超时 408。
pub(super) fn office_error(format: Format, e: pagesift_office::Error) -> ApiError {
    use pagesift_office::Error as E;
    let code = match e {
        E::Encrypted | E::Unsupported(_) => ErrorCode::UnsupportedOption,
        E::TooLarge(_) => ErrorCode::TooLarge,
        E::Timeout => ErrorCode::Timeout,
        _ => ErrorCode::InvalidRequest,
    };
    ApiError::new(
        code,
        format!("could not read {}: {e}", format.extension().to_uppercase()),
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn office_errors_are_caller_errors() {
        use pagesift_office::Error as E;
        for (e, code) in [
            (E::Encrypted, ErrorCode::UnsupportedOption),
            (E::TooLarge("x"), ErrorCode::TooLarge),
            (E::Timeout, ErrorCode::Timeout),
            (E::Malformed("x".into()), ErrorCode::InvalidRequest),
        ] {
            assert_eq!(office_error(Format::Docx, e).code, code);
        }
    }

    #[test]
    fn html_responses_are_not_csv() {
        assert_eq!(
            office_format(b"a,b", "/data.csv", Some("text/plain")),
            Some(Format::Csv)
        );
        assert_eq!(office_format(b"<p>", "/data.csv", Some("text/html")), None);
        assert_eq!(office_format(b"<p>", "/page", None), None);
    }
}
