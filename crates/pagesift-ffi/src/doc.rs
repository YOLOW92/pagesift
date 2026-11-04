//! 结果 → JSON。字段和 Python 的 `Document.to_dict()` 一致，另加 `attachments`（`data` 是 base64）。

use pagesift_core::api::ApiError;
use pagesift_core::api::types::{self, Metadata};
use serde_json::{Map, Value, json};

/// 一次抓取或解析的结果。
pub struct Outcome {
    pub index: Option<usize>,
    /// 请求的地址（解析时是文件名）；`convert` 没有。
    pub url: Option<String>,
    pub result: Result<types::Document, ApiError>,
}

pub fn error_value(e: &ApiError) -> Value {
    json!({
        "code": e.code.as_str(),
        "message": e.message,
        "details": e.details.clone().unwrap_or_else(|| json!({})),
        "retry_after": e.retry_after,
    })
}

/// 单个文档的 JSON。失败时文档字段全部为 null，`error` 给出原因。
pub fn document_value(o: &Outcome) -> Value {
    let doc = o.result.as_ref().ok();
    let err = o.result.as_ref().err();
    let meta = doc.map(|d| &d.metadata);
    let ext = meta.and_then(|m| m.pagesift.as_ref());
    json!({
        "index": o.index,
        "url": o.url,
        "final_url": meta.and_then(|m| m.url.as_deref()),
        "success": err.is_none(),
        // 抓取和解析才有状态码；convert 没有请求地址
        "status_code": if o.url.is_some() { meta.map(|m| m.status_code) } else { None },
        "markdown": doc.and_then(|d| d.markdown.as_deref()),
        "html": doc.and_then(|d| d.html.as_deref()),
        "raw_html": doc.and_then(|d| d.raw_html.as_deref()),
        "links": doc.and_then(|d| d.links.as_ref()),
        "images": doc.and_then(|d| d.images.as_ref()),
        "extract": doc.and_then(|d| d.extract.as_ref()),
        "metadata": meta.map_or_else(|| Value::Object(Map::new()), metadata),
        "attachments": doc.map_or_else(Vec::new, attachments),
        "error_code": err.map(|e| e.code.as_str()),
        "error_message": err.map(|e| e.message.as_str()),
        "error": err.map(error_value),
        "engine": ext.and_then(|e| e.engine.as_deref()),
        "content_hash": ext.and_then(|e| e.content_hash.as_deref()),
        "raw_hash": ext.and_then(|e| e.raw_hash.as_deref()),
        "text_hash": ext.and_then(|e| e.text_hash.as_deref()),
        "elapsed_ms": ext.and_then(|e| e.timings.get("total").copied()),
        "timings": ext.map(|e| json!(e.timings)).unwrap_or_else(|| json!({})),
        "warnings": ext.map(|e| json!(e.warnings)).unwrap_or_else(|| json!([])),
        "ocr": ext.and_then(|e| e.ocr.as_ref()).map(|o| json!(o)),
        "redirect_chain": ext.map(|e| json!(e.redirect_chain)).unwrap_or_else(|| json!([])),
        "etag": ext.and_then(|e| e.etag.as_deref()),
        "last_modified": ext.and_then(|e| e.last_modified.as_deref()),
        "not_modified": ext.is_some_and(|e| e.not_modified),
    })
}

fn metadata(m: &Metadata) -> Value {
    let mut d = Map::new();
    for (k, v) in [
        ("title", &m.title),
        ("description", &m.description),
        ("language", &m.language),
        ("keywords", &m.keywords),
        ("robots", &m.robots),
        ("favicon", &m.favicon),
        ("published_time", &m.published_time),
        ("author", &m.author),
        ("modified_time", &m.modified_time),
        ("content_type", &m.content_type),
    ] {
        if let Some(v) = v {
            d.insert(k.into(), Value::String(v.clone()));
        }
    }
    for (k, v) in &m.extra {
        d.insert(pagesift_sink::meta::snake_case(k), v.clone());
    }
    Value::Object(d)
}

fn attachments(d: &types::Document) -> Vec<Value> {
    d.attachments
        .iter()
        .map(|a| {
            json!({
                "id": a.id,
                "format": a.format,
                "bytes": a.bytes,
                "hash": a.hash,
                "source": a.source,
                "data": a.data.as_deref().map(base64),
            })
        })
        .collect()
}

fn base64(data: &[u8]) -> String {
    const T: &[u8; 64] = b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789+/";
    let mut out = String::with_capacity(data.len().div_ceil(3) * 4);
    for c in data.chunks(3) {
        let n = (u32::from(c[0]) << 16)
            | (u32::from(*c.get(1).unwrap_or(&0)) << 8)
            | u32::from(*c.get(2).unwrap_or(&0));
        for (i, shift) in [18, 12, 6, 0].into_iter().enumerate() {
            if i <= c.len() {
                out.push(T[(n >> shift) as usize & 63] as char);
            } else {
                out.push('=');
            }
        }
    }
    out
}

/// 不是单个文档的调用（batch、map、crawl）失败时的外层结果。
pub fn failure_value(e: &ApiError) -> Value {
    json!({
        "success": false,
        "error_code": e.code.as_str(),
        "error_message": e.message,
        "error": error_value(e),
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn base64_matches_rfc4648_vectors() {
        for (raw, enc) in [
            ("", ""),
            ("f", "Zg=="),
            ("fo", "Zm8="),
            ("foo", "Zm9v"),
            ("foob", "Zm9vYg=="),
            ("fooba", "Zm9vYmE="),
            ("foobar", "Zm9vYmFy"),
        ] {
            assert_eq!(base64(raw.as_bytes()), enc);
        }
    }
}
