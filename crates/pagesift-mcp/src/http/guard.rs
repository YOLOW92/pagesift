//! 安全检查：校验 `Host` 和 `Origin`，防止 DNS 重绑定。

use axum::http::{HeaderMap, StatusCode, header};
use axum::response::Response;
use serde_json::json;

use crate::jsonrpc;

use super::{HttpConfig, json_response};

pub(super) fn check_host_and_origin(
    cfg: &HttpConfig,
    headers: &HeaderMap,
) -> Result<(), Box<Response>> {
    if let Some(allowed) = &cfg.allowed_hosts {
        let host = headers
            .get(header::HOST)
            .and_then(|h| h.to_str().ok())
            .map(host_without_port);
        let ok = host.is_some_and(|h| allowed.iter().any(|a| a.eq_ignore_ascii_case(h)));
        if !ok {
            return Err(Box::new(forbidden("Host header is not allowed")));
        }
    }
    if let Some(origin) = headers.get(header::ORIGIN) {
        let ok = origin.to_str().ok().is_some_and(|o| origin_allowed(cfg, o));
        if !ok {
            return Err(Box::new(forbidden("Origin is not allowed")));
        }
    }
    Ok(())
}

/// `example.com:80` → `example.com`，`[::1]:3003` → `[::1]`。
pub(super) fn host_without_port(h: &str) -> &str {
    if h.starts_with('[') {
        return h.find(']').map_or(h, |i| &h[..=i]);
    }
    h.rsplit_once(':').map_or(h, |(host, _)| host)
}

pub(super) fn origin_allowed(cfg: &HttpConfig, origin: &str) -> bool {
    if cfg.allowed_origins.iter().any(|o| o == origin) {
        return true;
    }
    if !cfg.allow_loopback_origins {
        return false;
    }
    let Some(rest) = origin
        .strip_prefix("http://")
        .or_else(|| origin.strip_prefix("https://"))
    else {
        return false;
    };
    if rest.contains('/') || rest.contains('@') {
        return false;
    }
    let host = host_without_port(rest);
    host.eq_ignore_ascii_case("localhost") || host == "127.0.0.1" || host == "[::1]"
}

pub(super) fn forbidden(why: &str) -> Response {
    // 规范允许附上不带 id 的 JSON-RPC 错误。
    let body = json!({ "jsonrpc": "2.0", "error": { "code": jsonrpc::INVALID_REQUEST, "message": format!("Forbidden: {why}") } });
    json_response(StatusCode::FORBIDDEN, body)
}
