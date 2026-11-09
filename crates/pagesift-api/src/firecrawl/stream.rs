//! `GET /v2/crawl/{id}/stream` 和 `GET /v2/batch/scrape/{id}/stream`：任务完成一页推一页。
//!
//! - 默认 SSE：每页一个 `event: page`，`id:` 是这页的序号，`data:` 是这页文档的 JSON
//!   （和轮询接口 `data` 数组里的元素逐字一样）；
//! - `Accept: application/x-ndjson`：每行一个 `{"seq":…,"type":"page","data":…}`；
//! - 序号就是轮询接口里文档的下标（`skip`），从 0 开始；
//! - 续传：`?cursor=<seq>` 或 SSE 重连时的 `Last-Event-ID`，从该序号之后开始（两者都有时以 `cursor` 为准）；
//! - 任务结束（完成、失败、取消）时推 `event: done`（NDJSON 是 `{"type":"done",…}`），带最终状态，然后关闭；
//! - 页完成即推：任务每记下一页就通知等着的流，不轮询。鉴权和轮询接口一样（同一层中间件）；
//! - 心跳：[`KEEPALIVE`] 内没有新事件，SSE 发一行注释 `: keepalive`，NDJSON 发一行 `{"type":"keepalive"}`
//!   （合法的 JSON 行，按 `type` 分派的客户端忽略即可；空行会让逐行解析 JSON 的客户端报错）。

use std::time::Duration;

use axum::body::Bytes;
use axum::extract::{Path, Query, State};
use axum::http::{HeaderMap, header};
use axum::response::{IntoResponse, Response};
use serde::Deserialize;
use serde_json::json;
use tokio::sync::{mpsc, watch};

use crate::error::{ApiError, ErrorCode};
use crate::jobs::{JobState, Jobs, StatusPage};
use crate::server::{AppState, PAGE_SIZE};
use crate::types::Document;

/// 没有新事件时多久发一次心跳。
// [推导] 常见反向代理的默认读空闲超时取一半：nginx `proxy_read_timeout` 默认 60 s
// （ngx_http_proxy_module 文档），一半即 30 s，保证代理按读超时断开前至少收到一次数据。
pub(crate) const KEEPALIVE: Duration = Duration::from_secs(30);

#[derive(Deserialize)]
pub struct StreamQuery {
    #[serde(default)]
    pub cursor: Option<u64>,
    /// Provisional, non-replayable Markdown events in addition to final pages.
    #[serde(default)]
    pub previews: bool,
}

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
enum Format {
    Sse,
    Ndjson,
}

impl Format {
    fn preview(self, preview: &crate::types::MarkdownPreview) -> Bytes {
        let data = json!({"url": preview.url, "markdown": preview.markdown, "truncated": preview.truncated});
        Bytes::from(match self {
            Format::Sse => format!("event: markdown\ndata: {data}\n\n"),
            Format::Ndjson => format!("{{\"type\":\"markdown\",\"data\":{data}}}\n"),
        })
    }
    fn content_type(self) -> &'static str {
        match self {
            Format::Sse => "text/event-stream; charset=utf-8",
            Format::Ndjson => "application/x-ndjson",
        }
    }

    fn page(self, seq: usize, doc: &Document) -> Bytes {
        let data = serde_json::to_string(doc).unwrap_or_else(|_| "null".into());
        Bytes::from(match self {
            Format::Sse => format!("event: page\nid: {seq}\ndata: {data}\n\n"),
            Format::Ndjson => format!("{{\"seq\":{seq},\"type\":\"page\",\"data\":{data}}}\n"),
        })
    }

    fn keepalive(self) -> Bytes {
        Bytes::from_static(match self {
            Format::Sse => b": keepalive\n\n",
            Format::Ndjson => b"{\"type\":\"keepalive\"}\n",
        })
    }

    fn done(self, page: &StatusPage) -> Bytes {
        let status = json!({
            "status": page.state.as_str(),
            "completed": page.completed,
            "total": page.total,
        });
        Bytes::from(match self {
            Format::Sse => format!("event: done\ndata: {status}\n\n"),
            Format::Ndjson => {
                let mut v = json!({"type": "done"});
                if let (Some(o), Some(s)) = (v.as_object_mut(), status.as_object()) {
                    o.extend(s.clone());
                }
                format!("{v}\n")
            }
        })
    }
}

fn format(headers: &HeaderMap) -> Format {
    let accept = headers
        .get(header::ACCEPT)
        .and_then(|v| v.to_str().ok())
        .unwrap_or("");
    if accept.contains("application/x-ndjson") {
        Format::Ndjson
    } else {
        Format::Sse
    }
}

/// 第一个要推的序号：游标之后的那一个；没有游标从 0 开始。
fn start(q: &StreamQuery, headers: &HeaderMap) -> usize {
    let last_event = || {
        headers
            .get("last-event-id")
            .and_then(|v| v.to_str().ok())
            .and_then(|v| v.trim().parse::<u64>().ok())
    };
    q.cursor.or_else(last_event).map_or(0, |c| {
        usize::try_from(c).unwrap_or(usize::MAX).saturating_add(1)
    })
}

pub async fn job_stream(
    State(s): State<AppState>,
    caller: Option<axum::extract::Extension<crate::limits::Caller>>,
    headers: HeaderMap,
    Path(id): Path<String>,
    Query(q): Query<StreamQuery>,
) -> Result<Response, ApiError> {
    super::batch::authorize_job(
        &s,
        &id,
        caller.as_ref().map(|axum::extract::Extension(c)| c),
    )
    .await?;
    let changed = s
        .jobs
        .subscribe(&id)
        .await
        .ok_or_else(|| ApiError::new(ErrorCode::NotFound, format!("job {id} not found")))?;
    let fmt = format(&headers);
    let previews = if q.previews {
        s.jobs.subscribe_previews(&id).await
    } else {
        None
    };
    let (tx, body) = crate::stream_body::channel();
    tokio::spawn(pump(
        s.jobs.clone(),
        PumpState {
            id,
            next: start(&q, &headers),
            fmt,
            changed,
            tx,
            keepalive: KEEPALIVE,
            previews,
        },
    ));
    Ok((
        [
            (header::CONTENT_TYPE, fmt.content_type()),
            (header::CACHE_CONTROL, "no-cache"),
        ],
        body,
    )
        .into_response())
}

/// 从 `next` 开始把任务的文档逐页写进流；读到头了就等任务的通知，等了 `keepalive` 还没有就写一次心跳；
/// 任务结束时写 done 并返回。
struct PumpState {
    id: String,
    next: usize,
    fmt: Format,
    changed: watch::Receiver<()>,
    tx: mpsc::Sender<Bytes>,
    keepalive: Duration,
    previews: Option<tokio::sync::broadcast::Receiver<crate::types::MarkdownPreview>>,
}

async fn pump(jobs: Jobs, state: PumpState) {
    let PumpState {
        id,
        mut next,
        fmt,
        mut changed,
        tx,
        keepalive,
        mut previews,
    } = state;
    loop {
        if let Some(rx) = &mut previews {
            loop {
                match rx.try_recv() {
                    Ok(preview) => {
                        if tx.send(fmt.preview(&preview)).await.is_err() {
                            return;
                        }
                    }
                    Err(tokio::sync::broadcast::error::TryRecvError::Lagged(_)) => continue,
                    Err(_) => break,
                }
            }
        }
        // 任务过期被删了：没有最终状态可报，直接关闭
        let Some(page) = jobs.status(&id, next, PAGE_SIZE).await else {
            return;
        };
        if !page.data.is_empty() {
            for doc in &page.data {
                if tx.send(fmt.page(next, doc)).await.is_err() {
                    return;
                }
                next += 1;
            }
            continue;
        }
        if page.state != JobState::Scraping {
            let _ = tx.send(fmt.done(&page)).await;
            return;
        }
        // 发送端随任务一起删掉时 changed 返回错误，下一轮 status 返回 None 就结束
        tokio::select! {
            r = tokio::time::timeout(keepalive, changed.changed()) => {
                if r.is_err() && tx.send(fmt.keepalive()).await.is_err() {
                    return;
                }
            }
            () = tx.closed() => return,
            preview = async {
                match &mut previews {
                    Some(rx) => rx.recv().await.ok(),
                    None => std::future::pending().await,
                }
            } => {
                if let Some(preview) = preview && tx.send(fmt.preview(&preview)).await.is_err() { return; }
            }
        }
    }
}

#[cfg(test)]
#[path = "stream_tests.rs"]
mod tests;
