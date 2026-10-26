//! 新版（无状态）请求的 `MCP-Protocol-Version`、`Mcp-Method`、`Mcp-Name` 头与请求体核对。

use axum::http::HeaderMap;
use base64::Engine as _;
use serde_json::Value;

use super::{METHOD_HEADER, NAME_HEADER, VERSION_HEADER, header_str};
use crate::jsonrpc::{ErrorObject, Request};
use crate::protocol;

pub(super) fn check_modern_headers(headers: &HeaderMap, req: &Request) -> Result<(), ErrorObject> {
    let mismatch = |msg: String| ErrorObject::new(protocol::HEADER_MISMATCH, msg);
    for name in [VERSION_HEADER, METHOD_HEADER, NAME_HEADER] {
        if headers.get_all(name).iter().count() > 1 {
            return Err(mismatch(format!(
                "Header mismatch: {name} header appears more than once"
            )));
        }
    }
    let body_version = req
        .params
        .as_ref()
        .and_then(|p| p.get("_meta"))
        .and_then(|m| m.get(protocol::META_PROTOCOL_VERSION))
        .and_then(Value::as_str)
        .unwrap_or("");
    match header_str(headers, VERSION_HEADER) {
        None => {
            return Err(mismatch(
                "Header mismatch: missing MCP-Protocol-Version header".into(),
            ));
        }
        Some(h) if h != body_version => {
            return Err(mismatch(format!(
                "Header mismatch: MCP-Protocol-Version header value '{h}' does not match body value '{body_version}'"
            )));
        }
        Some(_) => {}
    }
    match header_str(headers, METHOD_HEADER) {
        None => {
            return Err(mismatch(
                "Header mismatch: missing Mcp-Method header".into(),
            ));
        }
        Some(h) if h != req.method => {
            return Err(mismatch(format!(
                "Header mismatch: Mcp-Method header value '{h}' does not match body value '{}'",
                req.method
            )));
        }
        Some(_) => {}
    }
    if matches!(
        req.method.as_str(),
        "tools/call" | "prompts/get" | "resources/read"
    ) {
        let key = if req.method == "resources/read" {
            "uri"
        } else {
            "name"
        };
        let body_name = req
            .params
            .as_ref()
            .and_then(|p| p.get(key))
            .and_then(Value::as_str);
        let Some(raw) = header_str(headers, NAME_HEADER) else {
            return Err(mismatch("Header mismatch: missing Mcp-Name header".into()));
        };
        let decoded = decode_header_value(raw)
            .ok_or_else(|| mismatch("Header mismatch: Mcp-Name header is malformed".into()))?;
        if Some(decoded.as_str()) != body_name {
            return Err(mismatch(format!(
                "Header mismatch: Mcp-Name header value '{decoded}' does not match body value '{}'",
                body_name.unwrap_or("")
            )));
        }
    }
    Ok(())
}

/// 解码 `=?base64?...?=` 形式的头值；普通值原样返回。
pub(super) fn decode_header_value(raw: &str) -> Option<String> {
    match raw
        .strip_prefix("=?base64?")
        .and_then(|r| r.strip_suffix("?="))
    {
        Some(b64) => {
            let bytes = base64::engine::general_purpose::STANDARD.decode(b64).ok()?;
            String::from_utf8(bytes).ok()
        }
        None => Some(raw.to_owned()),
    }
}
