//! Office、邮件、压缩包、文本类 → 文档：抓到的（`/v2/scrape`）和上传的（`/v2/parse`）走同一段代码。
//! 转换在 pagesift-office 里；EPUB 的 XHTML 章节和邮件的 HTML 正文交给和网页相同的 HTML 提取器
//! （保留整页，不做正文筛选）。压缩包成员和邮件附件回到这里递归转换，内层 PDF 只读文字层。

use pagesift_office::{Converted, Format};

use super::*;

/// [策略] 先验包 v0：0.81 的现值。没给 `timeout`、这个工人上转换的样本还不够时一份文档的转换时限；
/// 样本够了以后按"字节数 → 耗时"的预测上界（[`pagesift_stat::size_time`]）。
const DEFAULT_OFFICE_TIMEOUT: Duration = Duration::from_secs(30);

thread_local! {
    /// 这个 CPU 工人上 Office 转换的耗时样本（工人本地，不加锁）。
    static OFFICE_TIME: std::cell::RefCell<pagesift_stat::size_time::SizeTime> =
        std::cell::RefCell::default();
}

/// 转换的截止时间：学到的预测上界和请求的截止时间取早的；还没学到时和 0.81 一样
/// （有请求截止时间就用它，没有就用先验）。
fn office_deadline(bytes: usize, started: Instant, request: Option<Instant>) -> Instant {
    let learned = OFFICE_TIME
        .with_borrow(|m| m.timeout(bytes, pagesift_stat::Delta::DEFAULT))
        .map(|t| started + t);
    match (request, learned) {
        (Some(r), Some(l)) => r.min(l),
        (Some(r), None) => r,
        (None, Some(l)) => l,
        (None, None) => started + DEFAULT_OFFICE_TIMEOUT,
    }
}

/// 把一份 Office/EPUB/CSV 文档转成 Markdown，供引擎和评测共用。`deadline` 到了报超时。
/// CSV 的字符集按正文探测；知道 Content-Type 时用 [`office_markdown_from`]。
pub fn office_markdown(
    bytes: &[u8],
    format: Format,
    deadline: Option<Instant>,
) -> Result<Converted, pagesift_office::Error> {
    office_markdown_from(bytes, format, None, deadline)
}

/// HTML 转 Markdown：保留整页，相对路径按包里的位置解析。
fn html_to_markdown(xhtml: &str, path: &str) -> String {
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
}

/// 压缩包成员或邮件附件：按内容和文件名认类型，转不了的返回 `None`（只留在清单里）。
fn member_markdown(
    bytes: &[u8],
    name: &str,
    room: usize,
    deadline: Option<Instant>,
) -> Result<Option<Converted>, pagesift_office::Error> {
    use pagesift_office::Error as E;
    if bytes.starts_with(b"%PDF-") {
        let popts = pagesift_pdf::Options {
            timeout: deadline.map(|d| d.saturating_duration_since(Instant::now())),
            ..Default::default()
        };
        return match pagesift_pdf::extract_with(bytes, &popts) {
            Ok(pdf) => Ok(Some(Converted {
                parts: pdf.pages.len(),
                markdown: pdf.markdown,
                title: pdf.metadata.title,
                ..Default::default()
            })),
            Err(pagesift_pdf::Error::Timeout) => Err(E::Timeout),
            Err(e) => Err(E::Malformed(e.to_string())),
        };
    }
    let ext = name.rsplit_once('.').map(|(_, e)| e.to_ascii_lowercase());
    if matches!(ext.as_deref(), Some("html" | "htm" | "xhtml")) {
        let decoded = pagesift_net::decode(bytes, None, None);
        return Ok(Some(Converted {
            markdown: html_to_markdown(&decoded.text, name),
            ..Default::default()
        }));
    }
    let Some(format) = Format::detect(bytes, Some(name), None) else {
        return Ok(None);
    };
    convert_with_room(bytes, format, None, deadline, Some(room)).map(Some)
}

fn convert_with_room(
    bytes: &[u8],
    format: Format,
    content_type: Option<&str>,
    deadline: Option<Instant>,
    room: Option<usize>,
) -> Result<Converted, pagesift_office::Error> {
    let member = |data: &[u8], name: &str, room: usize| member_markdown(data, name, room, deadline);
    let mut opts = pagesift_office::Options {
        deadline,
        html: Some(&html_to_markdown),
        member: Some(&member),
        ..Default::default()
    };
    if let Some(room) = room {
        opts.limits.max_output_bytes = room;
    }
    if format.decodes_text() {
        let (text, warning) = csv_text::decode(bytes, content_type);
        let mut doc = pagesift_office::convert_text(&text, format, &opts)?;
        doc.warnings.extend(warning);
        return Ok(doc);
    }
    pagesift_office::convert(bytes, format, &opts)
}

/// 同 [`office_markdown`]，另给文件来源的 Content-Type（响应头或上传时的）：CSV 按它的
/// `charset` 解码，和网页一样；声明和正文对不上时纠正并给 `decode:` 警告。
pub fn office_markdown_from(
    bytes: &[u8],
    format: Format,
    content_type: Option<&str>,
    deadline: Option<Instant>,
) -> Result<Converted, pagesift_office::Error> {
    convert_with_room(bytes, format, content_type, deadline, None)
}

/// Office/EPUB 里的媒体文件作为附件：来源是部件路径（与 Markdown 里的图片引用一致，空格写成 `%20`）。
fn office_media(
    bytes: &[u8],
    format: Format,
) -> (
    Vec<pagesift_api::types::Attachment>,
    Vec<String>,
    Vec<pagesift_api::types::ImageAssetResult>,
) {
    let opts = pagesift_office::Options::default();
    let mut out = Vec::new();
    let mut warnings = Vec::new();
    let mut failed = Vec::new();
    let list = match pagesift_office::media(bytes, format, &opts) {
        Ok(list) => list,
        Err(e) => return (out, vec![format!("images_not_exported: {e}")], failed),
    };
    for m in list {
        match pagesift_office::media_bytes(bytes, &m.part, &opts) {
            Ok(Some(data)) => match pagesift_api::types::attachment::sniff_image_format(&data) {
                Some(format) => out.push(pagesift_api::types::Attachment::new(
                    data,
                    format,
                    Some(m.part.replace(' ', "%20")),
                )),
                None => {
                    warnings.push(format!(
                        "image_not_exported: {}: unrecognized image bytes",
                        m.part
                    ));
                    failed.push(pagesift_api::types::ImageAssetResult {
                        source: m.part.clone(),
                        status: pagesift_api::types::ImageAssetStatus::Failed,
                        id: None,
                        mime: None,
                        bytes: None,
                        reason: Some("unrecognized_image_bytes".into()),
                    });
                }
            },
            Ok(None) => {
                warnings.push(format!("image_not_exported: {}: missing part", m.part));
                failed.push(pagesift_api::types::ImageAssetResult {
                    source: m.part.clone(),
                    status: pagesift_api::types::ImageAssetStatus::Failed,
                    id: None,
                    mime: None,
                    bytes: None,
                    reason: Some("missing_part".into()),
                });
            }
            Err(e) => {
                warnings.push(format!("image_not_exported: {}: {e}", m.part));
                failed.push(pagesift_api::types::ImageAssetResult {
                    source: m.part.clone(),
                    status: pagesift_api::types::ImageAssetStatus::Failed,
                    id: None,
                    mime: None,
                    bytes: None,
                    reason: Some(e.to_string()),
                });
            }
        }
    }
    (out, warnings, failed)
}

/// 文件名或网址路径的扩展名、Content-Type、内容一起判断是不是要转换的文档。
/// 文本类不走这里（网上的 text/plain、XML、JSON 还是网页路径）；
/// 服务器明说是 HTML 的不信扩展名（`/data.csv` 返回的也可能是网页）。
pub(super) fn office_format(body: &[u8], name: &str, content_type: Option<&str>) -> Option<Format> {
    let html = content_type.is_some_and(|c| c.to_ascii_lowercase().contains("html"));
    Format::detect(body, Some(name), content_type)
        .filter(|f| f.fetchable() && !(html && f.is_text_based()))
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
        let export = opts.export_images;
        let (result, media) = cpu_task(move || {
            let started = Instant::now();
            let bytes = input.bytes.len();
            let deadline = office_deadline(bytes, started, deadline);
            let result = office_markdown_from(
                &input.bytes,
                input.format,
                input.content_type.as_deref(),
                Some(deadline),
            );
            if result.is_ok() {
                OFFICE_TIME.with_borrow_mut(|m| m.observe(bytes, started.elapsed()));
            }
            // `exportImages`：文档里的媒体原样从 ZIP 取出
            let media =
                (export && result.is_ok()).then(|| office_media(&input.bytes, input.format));
            (result, media)
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
        let (attachments, media_warnings, mut image_assets) = media.unwrap_or_default();
        if opts.image_assets.is_none() {
            image_assets.clear();
        }
        p.warnings.extend(media_warnings);
        meta.title = doc.title;
        Ok(finish(
            Document {
                markdown: opts.wants("markdown").then_some(doc.markdown),
                attachments,
                image_assets,
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
        // 文本类不抢网页路径；邮件和 RTF 这种内容明确的才接
        assert_eq!(office_format(b"x", "/a.txt", Some("text/plain")), None);
        assert_eq!(
            office_format(b"{}", "/a.json", Some("application/json")),
            None
        );
        assert_eq!(office_format(b"<p>", "/m.eml", Some("text/html")), None);
        assert_eq!(
            office_format(b"Subject: x\n\ny", "/m.eml", None),
            Some(Format::Eml)
        );
        assert_eq!(office_format(b"{\\rtf1 x}", "/f", None), Some(Format::Rtf));
    }
}
