//! Firecrawl v2 `/parse`：调用方直接上传文件（multipart/form-data 的 `file` 字段，可选的 `options`
//! JSON 字符串），返回和 `/scrape` 相同的响应。接口照 Firecrawl 文档
//! <https://docs.firecrawl.dev/api-reference/endpoint/parse> 和源码 `apps/api/src/controllers/v2/parse.ts`。
//!
//! 内存：文件边收边追加到一块按 Content-Length 一次分配好的缓冲区，超限立刻停；之后这块缓冲区
//! 原样交给引擎，不再复制。

use axum::Json;
use axum::extract::multipart::{Field, MultipartError, MultipartRejection};
use axum::extract::{Extension, Multipart, State};
use axum::http::{HeaderMap, StatusCode, header};
use serde_json::Value;

use crate::error::{ApiError, ErrorCode};
use crate::limits::Caller;
use crate::metrics::PageSource;
use crate::server::AppState;
use crate::types::{ScrapeOptions, ScrapeResponse, Upload, UploadKind};
use crate::validate::{MAX_PARSE_FILE_BYTES, check_parse_options};

/// `options` 字段的上限：JSON 选项再大也用不了 1 MB（json 格式的 schema 也够）。
const MAX_OPTIONS_BYTES: usize = 1024 * 1024;
/// 整个请求体的上限：文件加上 `options` 和 multipart 的分隔行、字段头。
pub const MAX_PARSE_BODY_BYTES: usize = MAX_PARSE_FILE_BYTES + MAX_OPTIONS_BYTES + 64 * 1024;

pub async fn parse(
    State(s): State<AppState>,
    caller: Option<Extension<Caller>>,
    headers: HeaderMap,
    form: Result<Multipart, MultipartRejection>,
) -> Result<Json<ScrapeResponse>, ApiError> {
    // 声明的大小已经超限就不读请求体
    let declared = headers
        .get(header::CONTENT_LENGTH)
        .and_then(|v| v.to_str().ok())
        .and_then(|v| v.parse::<usize>().ok());
    if declared.is_some_and(|n| n > MAX_PARSE_BODY_BYTES) {
        return Err(too_large());
    }
    let mut form = form.map_err(|e| {
        ApiError::invalid(format!(
            "invalid multipart/form-data request: {}; send a 'file' field and an optional 'options' JSON string",
            e.body_text()
        ))
    })?;
    if let Some(Extension(c)) = &caller {
        s.limiter.check_quota(c)?;
    }

    let mut file: Option<(Vec<u8>, String, Option<String>)> = None;
    let mut options: Option<ScrapeOptions> = None;
    while let Some(field) = form.next_field().await.map_err(multipart_error)? {
        match field.name() {
            Some("file") => {
                if file.is_some() {
                    return Err(ApiError::invalid("only one 'file' field is allowed"));
                }
                let filename = field
                    .file_name()
                    .map(|n| n.rsplit(['/', '\\']).next().unwrap_or(n).to_string())
                    .filter(|n| !n.is_empty())
                    .unwrap_or_else(|| "upload".into());
                let content_type = field.content_type().map(str::to_string);
                let capacity = declared.unwrap_or(0).min(MAX_PARSE_FILE_BYTES);
                let bytes = read_field(field, MAX_PARSE_FILE_BYTES, capacity).await?;
                file = Some((bytes, filename, content_type));
            }
            Some("options") => {
                let raw = read_field(field, MAX_OPTIONS_BYTES, 0).await?;
                options = Some(parse_options(&raw)?);
            }
            // 其他字段读过去不用，和 Firecrawl 一样
            _ => {}
        }
    }
    let Some((bytes, filename, content_type)) = file else {
        return Err(ApiError::invalid(
            "missing file upload: send multipart/form-data with a 'file' field and an optional 'options' JSON string",
        ));
    };
    if bytes.is_empty() {
        return Err(ApiError::invalid("the uploaded file is empty"));
    }
    let opts = options.unwrap_or_else(default_options);
    check_parse_options(&opts, s.engine.as_ref())?;
    let Some(kind) = UploadKind::detect(&bytes, &filename, content_type.as_deref()) else {
        return Err(ApiError::unsupported(
            "unsupported upload type: pagesift parses PDF (.pdf), HTML (.html, .htm, .xhtml), Word (.docx), Excel (.xlsx), PowerPoint (.pptx), EPUB (.epub) and CSV (.csv, .tsv) files",
        ));
    };
    let upload = Upload {
        bytes,
        filename,
        content_type,
        kind,
    };
    let data = s.engine.parse(upload, &opts).await?;
    s.metrics.page(PageSource::Parse);
    if let Some(Extension(c)) = &caller {
        s.limiter.consume_pages(c, 1).await;
    }
    Ok(Json(ScrapeResponse {
        success: true,
        warning: data.warning.clone(),
        data,
    }))
}

/// 读完一个字段，超过 `limit` 就停下报 413。`capacity` 是预先分配的大小，免得边读边翻倍扩容
/// （扩容的一瞬间新旧两块缓冲区同时在内存里）。
async fn read_field(
    mut field: Field<'_>,
    limit: usize,
    capacity: usize,
) -> Result<Vec<u8>, ApiError> {
    let mut buf = Vec::with_capacity(capacity);
    while let Some(chunk) = field.chunk().await.map_err(multipart_error)? {
        if buf.len() + chunk.len() > limit {
            return Err(too_large());
        }
        buf.extend_from_slice(&chunk);
    }
    Ok(buf)
}

fn parse_options(raw: &[u8]) -> Result<ScrapeOptions, ApiError> {
    let v: Value = serde_json::from_slice(raw).map_err(|e| {
        ApiError::invalid(format!(
            "invalid JSON in the 'options' field: {e}; provide a JSON object string"
        ))
    })?;
    if !v.is_object() {
        return Err(ApiError::invalid(
            "the 'options' field must parse to a JSON object",
        ));
    }
    serde_json::from_value(v).map_err(|e| ApiError::invalid(format!("invalid options: {e}")))
}

fn default_options() -> ScrapeOptions {
    serde_json::from_value(Value::Object(Default::default())).expect("defaults deserialize")
}

fn too_large() -> ApiError {
    ApiError::new(
        ErrorCode::TooLarge,
        format!(
            "uploaded file exceeds the {} MB limit",
            MAX_PARSE_FILE_BYTES / (1024 * 1024)
        ),
    )
}

fn multipart_error(e: MultipartError) -> ApiError {
    if e.status() == StatusCode::PAYLOAD_TOO_LARGE {
        return too_large();
    }
    ApiError::invalid(format!(
        "invalid multipart/form-data request: {}",
        e.body_text()
    ))
}
