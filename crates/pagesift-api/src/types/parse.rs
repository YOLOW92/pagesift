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
}

impl UploadKind {
    /// 判断上传的文件类型：内容以 `%PDF-` 开头就是 PDF（不管文件名怎么写）；否则看扩展名，
    /// 再看 Content-Type。和 Firecrawl 一样，认不出的类型不猜，直接拒绝。
    pub fn detect(bytes: &[u8], filename: &str, content_type: Option<&str>) -> Option<Self> {
        if bytes.starts_with(b"%PDF-") {
            return Some(UploadKind::Pdf);
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
}

impl PdfParser {
    /// 从 `parsers` 取 PDF 选项。接受 Firecrawl 的两种写法：`["pdf"]` 和 `[{type: "pdf", ...}]`。
    /// 做不到的选项（OCR、版面块、分页标记）明确报 UNSUPPORTED_OPTION，不悄悄忽略。
    pub fn from_parsers(parsers: Option<&Value>) -> Result<Self, ApiError> {
        let items = match parsers {
            None | Some(Value::Null) => return Ok(Self::default()),
            Some(Value::Array(items)) => items,
            Some(_) => return Err(ApiError::invalid("parsers must be an array")),
        };
        let mut out = Self::default();
        for item in items {
            let obj = match item {
                Value::String(t) if t == "pdf" => continue,
                Value::Object(o) if o.get("type").and_then(Value::as_str) == Some("pdf") => o,
                _ => {
                    return Err(ApiError::invalid(
                        "parsers items must be \"pdf\" or {\"type\": \"pdf\", ...}",
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
        assert_eq!(d(b"PK", "a.docx", None), None);
        assert_eq!(d(b"x", "upload", Some("application/octet-stream")), None);
    }

    #[test]
    fn pdf_parser_options() {
        let p = |v| PdfParser::from_parsers(Some(&v));
        assert_eq!(p(json!(["pdf"])).unwrap(), PdfParser::default());
        assert_eq!(
            p(json!([{"type": "pdf", "mode": "fast", "maxPages": 3, "pages": true}])).unwrap(),
            PdfParser {
                mode: Some("fast".into()),
                max_pages: Some(3),
                pages: true
            }
        );
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
