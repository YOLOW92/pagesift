//! `/v2/parse`（上传文件解析）的输入类型：上传的文件、文件类型判断、`parsers` 里的 PDF 选项。
//! 字段和取值照 Firecrawl 文档 <https://docs.firecrawl.dev/api-reference/endpoint/parse>
//! 与源码 `apps/api/src/controllers/v2/parse.ts`。

use serde_json::Value;

use crate::error::ApiError;

/// 调用方上传的文件。字节只有这一份，交给引擎后不再复制。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Upload {
    pub bytes: Vec<u8>,
    /// multipart 里的文件名（没有时为 `upload`），作为文档的 `metadata.sourceURL`。
    pub filename: String,
    /// multipart 里这个文件的 Content-Type。
    pub content_type: Option<String>,
    pub kind: UploadKind,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum UploadKind {
    Pdf,
    Html,
    /// Word、Excel、PowerPoint、EPUB、CSV（见 pagesift-office）。
    Office(pagesift_office::Format),
}

impl UploadKind {
    /// 判断上传的文件类型：内容以 `%PDF-` 开头就是 PDF；ZIP 容器按里面的部件认 Word、Excel、
    /// PowerPoint、EPUB（不管文件名怎么写）；否则看扩展名，再看 Content-Type。
    /// 和 Firecrawl 一样，认不出的类型不猜，直接拒绝。
    pub fn detect(bytes: &[u8], filename: &str, content_type: Option<&str>) -> Option<Self> {
        if bytes.starts_with(b"%PDF-") {
            return Some(UploadKind::Pdf);
        }
        if let Some(f) = pagesift_office::Format::detect(bytes, Some(filename), content_type) {
            return Some(UploadKind::Office(f));
        }
        let ext = filename
            .rsplit_once('.')
            .map(|(_, e)| e.to_ascii_lowercase())
            .unwrap_or_default();
        let mime = content_type
            .and_then(|c| c.split(';').next())
            .map(|c| c.trim().to_ascii_lowercase())
            .unwrap_or_default();
        if ext == "pdf" || matches!(mime.as_str(), "application/pdf" | "application/x-pdf") {
            return Some(UploadKind::Pdf);
        }
        if matches!(ext.as_str(), "html" | "htm" | "xhtml")
            || matches!(mime.as_str(), "text/html" | "application/xhtml+xml")
        {
            return Some(UploadKind::Html);
        }
        None
    }

    pub fn mime(self) -> &'static str {
        match self {
            UploadKind::Pdf => "application/pdf",
            UploadKind::Html => "text/html",
            UploadKind::Office(f) => f.mime(),
        }
    }
}

/// `parsers` 里 `{type: "pdf"}` 的选项。
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct PdfParser {
    /// `fast`、`auto`（默认）或 `ocr`。
    pub mode: Option<String>,
    /// 只输出前这么多页（1–10000）。
    pub max_pages: Option<usize>,
    /// 另给每页的 Markdown（`data.pages`）。
    pub pages: bool,
    /// 给了 `parsers` 却没有 `pdf`（比如 `[]`）：和 Firecrawl 一样不解析，把原文件按 base64
    /// 放进 `markdown`。
    pub raw: bool,
}

impl PdfParser {
    /// 从 `parsers` 取 PDF 选项。接受 Firecrawl 的写法：`["pdf"]`、`[{type: "pdf", ...}]`，
    /// 以及 `"image"`（图片 OCR，这里本来就不支持图片，照收不报错，免得 Firecrawl SDK 的默认值被拒）。
    /// 列表里没有 `pdf` 时 PDF 不解析（[`Self::raw`]）。
    /// 做不到的选项（OCR、版面块、分页标记）明确报 UNSUPPORTED_OPTION，不悄悄忽略。
    pub fn from_parsers(parsers: Option<&Value>) -> Result<Self, ApiError> {
        let items = match parsers {
            None | Some(Value::Null) => return Ok(Self::default()),
            Some(Value::Array(items)) => items,
            Some(_) => return Err(ApiError::invalid("parsers must be an array")),
        };
        let mut out = Self {
            raw: true,
            ..Self::default()
        };
        for item in items {
            let kind = match item {
                Value::String(t) => t.as_str(),
                Value::Object(o) => o.get("type").and_then(Value::as_str).unwrap_or_default(),
                _ => "",
            };
            let obj = match (kind, item) {
                ("image", _) => continue,
                ("pdf", Value::String(_)) => {
                    out.raw = false;
                    continue;
                }
                ("pdf", Value::Object(o)) => {
                    out.raw = false;
                    o
                }
                _ => {
                    return Err(ApiError::invalid(
                        "parsers items must be \"pdf\", \"image\" or {\"type\": \"pdf\" | \"image\", ...}",
                    ));
                }
            };
            for (key, v) in obj {
                match key.as_str() {
                    "type" => {}
                    "mode" => match v.as_str() {
                        Some(m @ ("fast" | "auto")) => out.mode = Some(m.into()),
                        Some("ocr") => {
                            return Err(ApiError::unsupported(
                                "parsers[].mode \"ocr\" is not supported: pagesift reads the PDF text layer only",
                            ));
                        }
                        _ => {
                            return Err(ApiError::invalid(
                                "parsers[].mode must be fast, auto or ocr",
                            ));
                        }
                    },
                    "maxPages" => match v.as_u64() {
                        Some(n @ 1..=10_000) => out.max_pages = Some(n as usize),
                        _ => {
                            return Err(ApiError::invalid(
                                "parsers[].maxPages must be an integer from 1 to 10000",
                            ));
                        }
                    },
                    "pages" => out.pages = flag(key, v)?,
                    "blocks" | "pageMarkers" => {
                        if flag(key, v)? {
                            return Err(ApiError::unsupported(format!(
                                "parsers[].{key} is not supported by this build"
                            )));
                        }
                    }
                    _ => {
                        return Err(ApiError::invalid(format!(
                            "unknown PDF parser option {key:?}"
                        )));
                    }
                }
            }
        }
        Ok(out)
    }
}

fn flag(key: &str, v: &Value) -> Result<bool, ApiError> {
    v.as_bool()
        .ok_or_else(|| ApiError::invalid(format!("parsers[].{key} must be a boolean")))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::error::ErrorCode;
    use serde_json::json;

    #[test]
    fn kind_prefers_content_then_name_then_mime() {
        let d = UploadKind::detect;
        assert_eq!(
            d(b"%PDF-1.7", "a.html", Some("text/html")),
            Some(UploadKind::Pdf)
        );
        assert_eq!(d(b"x", "Report.PDF", None), Some(UploadKind::Pdf));
        assert_eq!(
            d(b"x", "upload", Some("application/pdf; q=1")),
            Some(UploadKind::Pdf)
        );
        assert_eq!(d(b"<p>", "a.htm", None), Some(UploadKind::Html));
        assert_eq!(
            d(b"<p>", "blob", Some("Text/HTML; charset=utf-8")),
            Some(UploadKind::Html)
        );
        // 内容不是 ZIP 的 .docx 认不出；ZIP 按里面的部件认
        assert_eq!(d(b"PK", "a.docx", None), None);
        assert_eq!(
            d(b"a,b\n1,2", "t.csv", None),
            Some(UploadKind::Office(pagesift_office::Format::Csv))
        );
        assert_eq!(
            d(b"a,b", "upload", Some("text/csv")),
            Some(UploadKind::Office(pagesift_office::Format::Csv))
        );
        assert_eq!(d(b"x", "upload", Some("application/octet-stream")), None);
    }

    #[test]
    fn pdf_parser_options() {
        let p = |v| PdfParser::from_parsers(Some(&v));
        assert_eq!(p(json!(["pdf"])).unwrap(), PdfParser::default());
        assert_eq!(p(json!(["pdf", "image"])).unwrap(), PdfParser::default());
        assert_eq!(
            p(json!([{"type": "pdf", "mode": "fast", "maxPages": 3, "pages": true}])).unwrap(),
            PdfParser {
                mode: Some("fast".into()),
                max_pages: Some(3),
                pages: true,
                raw: false,
            }
        );
        // 列表里没有 pdf：不解析，原样返回
        for v in [json!([]), json!(["image"]), json!([{"type": "image"}])] {
            assert!(p(v.clone()).unwrap().raw, "{v}");
        }
        assert!(!PdfParser::from_parsers(None).unwrap().raw);
        // 关着的 blocks / pageMarkers 不算请求
        assert!(p(json!([{"type": "pdf", "blocks": false, "pageMarkers": false}])).is_ok());
        for (v, code) in [
            (
                json!([{"type": "pdf", "mode": "ocr"}]),
                ErrorCode::UnsupportedOption,
            ),
            (
                json!([{"type": "pdf", "blocks": true}]),
                ErrorCode::UnsupportedOption,
            ),
            (
                json!([{"type": "pdf", "maxPages": 0}]),
                ErrorCode::InvalidRequest,
            ),
            (
                json!([{"type": "pdf", "maxPages": 10001}]),
                ErrorCode::InvalidRequest,
            ),
            (
                json!([{"type": "pdf", "mode": "x"}]),
                ErrorCode::InvalidRequest,
            ),
            (json!([{"type": "docx"}]), ErrorCode::InvalidRequest),
            (json!({"type": "pdf"}), ErrorCode::InvalidRequest),
        ] {
            assert_eq!(p(v.clone()).unwrap_err().code, code, "{v}");
        }
    }
}
