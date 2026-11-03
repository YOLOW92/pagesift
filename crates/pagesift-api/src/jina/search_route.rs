//! Jina 的 `/s/<query>`：搜索，再把前几条结果逐个按 `/r/` 的规则抓取转换。
//!
//! 条数照 Jina README（“fetches the top 5 results”）；`?site=a.com&site=b.com` 限定站点
//! （转成搜索后端的 `site:` 语法）。请求头和 `/r/` 一样映射（`x-respond-with`、保留方式、截断……）；
//! README 写明 `x-token-budget` 对搜索不起作用，这里也不用它。

use axum::Json;
use axum::extract::{Extension, Path, RawQuery, State};
use axum::http::{HeaderMap, header};
use axum::response::{IntoResponse, Response};

use crate::limits::Caller;
use crate::server::AppState;
use crate::types::SearchRequest;

/// [协议] Jina README：`s.jina.ai` 抓取前 5 条结果。
const JINA_TOP_RESULTS: usize = 5;

pub(crate) async fn handle_jina_search(
    State(s): State<AppState>,
    caller: Option<Extension<Caller>>,
    headers: HeaderMap,
    Path(raw): Path<String>,
    RawQuery(query): RawQuery,
) -> Response {
    let mut q = raw.trim().to_string();
    let sites: Vec<String> = query
        .as_deref()
        .unwrap_or_default()
        .split('&')
        .filter_map(|kv| kv.strip_prefix("site="))
        .map(url_decode)
        .filter(|v| !v.is_empty())
        .collect();
    if q.is_empty() {
        return crate::error::ApiError::invalid("the search query is empty").into_response();
    }
    if !sites.is_empty() {
        let any: Vec<String> = sites.iter().map(|d| format!("site:{d}")).collect();
        q = format!("{q} ({})", any.join(" OR "));
    }
    let (opts, mut params) = match super::headers::parse(&headers) {
        Ok(v) => v,
        Err(e) => return e.into_response(),
    };
    params.token_budget = None;
    if let Err(e) = crate::validate::check_scrape_options(&opts, s.engine.as_ref()) {
        return e.into_response();
    }
    if let Some(Extension(c)) = &caller
        && let Err(e) = s.limiter.check_quota(c)
    {
        return e.into_response();
    }
    let req: SearchRequest = match serde_json::from_value(serde_json::json!({
        "query": q, "limit": JINA_TOP_RESULTS,
    })) {
        Ok(r) => r,
        Err(e) => return crate::error::ApiError::invalid(e.to_string()).into_response(),
    };
    let hits = match s.engine.search(&req).await {
        Ok(v) => v["web"].as_array().cloned().unwrap_or_default(),
        Err(e) => return e.into_response(),
    };
    // 几条结果同时抓（各自仍受引擎的按主机限速约束），按搜索结果的顺序收回
    let handles: Vec<_> = hits
        .iter()
        .take(JINA_TOP_RESULTS)
        .map(|h| {
            let url = h["url"].as_str().unwrap_or_default().to_string();
            let (engine, opts) = (s.engine.clone(), opts.clone());
            tokio::spawn(async move {
                let r = engine.scrape(&url, &opts).await;
                (url, r)
            })
        })
        .collect();
    let mut pages = Vec::with_capacity(handles.len());
    for h in handles {
        match h.await {
            Ok(p) => pages.push(p),
            Err(e) => pages.push((
                String::new(),
                Err(crate::error::ApiError::new(
                    crate::error::ErrorCode::Internal,
                    e.to_string(),
                )),
            )),
        }
    }
    let mut out = Vec::new();
    for ((url, r), hit) in pages.into_iter().zip(&hits) {
        let title = hit["title"].as_str().unwrap_or_default().to_string();
        let description = hit["description"].as_str().unwrap_or_default().to_string();
        let (content, warnings) =
            match r.and_then(|doc| super::respond::render(&doc, &url, &params).map(|x| (x, doc))) {
                Ok((x, doc)) => {
                    if let Some(Extension(c)) = &caller {
                        s.limiter.consume_pages(c, 1).await;
                    }
                    let mut w = doc
                        .metadata
                        .pagesift
                        .map(|m| m.warnings)
                        .unwrap_or_default();
                    w.extend(x.warnings);
                    (x.body, w)
                }
                // 单条失败不影响其他结果：如实写出错误
                Err(e) => (
                    String::new(),
                    vec![format!("{}: {}", e.code.as_str(), e.message)],
                ),
            };
        out.push((title, url, description, content, warnings));
    }
    let accept = headers
        .get(header::ACCEPT)
        .and_then(|v| v.to_str().ok())
        .unwrap_or("");
    if accept.contains("application/json") {
        let data: Vec<serde_json::Value> = out
            .iter()
            .map(|(t, u, d, c, w)| {
                serde_json::json!({ "title": t, "url": u, "description": d, "content": c, "warnings": w })
            })
            .collect();
        // [协议] Jina Reader JSON 响应的固定外壳：code 200、status 20000
        return Json(serde_json::json!({ "code": 200, "status": 20000, "data": data }))
            .into_response();
    }
    let mut text = String::new();
    for (i, (t, u, d, c, w)) in out.iter().enumerate() {
        let n = i + 1;
        text.push_str(&format!(
            "[{n}] Title: {t}\n[{n}] URL Source: {u}\n[{n}] Description: {d}\n"
        ));
        if !w.is_empty() {
            text.push_str(&format!("[{n}] Warnings: {}\n", w.join("; ")));
        }
        text.push_str(&format!("[{n}] Markdown Content:\n{c}\n\n"));
    }
    ([(header::CONTENT_TYPE, "text/plain; charset=utf-8")], text).into_response()
}

fn url_decode(s: &str) -> String {
    let bytes = s.replace('+', " ").into_bytes();
    let mut out = Vec::with_capacity(bytes.len());
    let mut i = 0;
    while i < bytes.len() {
        // [协议] 百分号编码 %XX：两位十六进制（RFC 3986 §2.1）
        if bytes[i] == b'%'
            && let Some(b) = std::str::from_utf8(&bytes[i + 1..(i + 3).min(bytes.len())])
                .ok()
                .and_then(|h| u8::from_str_radix(h, 16).ok())
        {
            out.push(b);
            i += 3; // [协议] 百分号编码 %XX 占 3 个字节（RFC 3986 §2.1）
            continue;
        }
        out.push(bytes[i]);
        i += 1;
    }
    String::from_utf8_lossy(&out).into_owned()
}
