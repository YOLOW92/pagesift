//! Jina Reader protocol adapter for PageSift.
//! Implements `GET /r/{*url}` and `POST /r` for direct markdown and SSE event streaming.

pub mod reader_route;
pub mod stream_sse;

use axum::Router;
use axum::routing::{get, post};

use crate::server::AppState;
pub(crate) use reader_route::{handle_jina_get, handle_jina_post};
pub use stream_sse::stream_markdown_sse;

/// Jina Reader 路由。和 Firecrawl 路由挂在同一层鉴权、限流和配额之后。
pub(crate) fn jina_routes() -> Router<AppState> {
    Router::new()
        .route("/r/{*url}", get(handle_jina_get))
        .route("/r", post(handle_jina_post))
        .route("/r/", post(handle_jina_post))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_sse_event_formatting() {
        let resp = stream_markdown_sse(
            "https://example.com",
            Some("Test Title"),
            "# Hello\n\nWorld paragraph",
        );
        assert_eq!(resp.status(), axum::http::StatusCode::OK);
        assert_eq!(
            resp.headers().get("content-type").unwrap(),
            "text/event-stream; charset=utf-8"
        );
    }

    /// 改成流式之前的实现，原样保留做字节对照。
    fn old_sse_text(url: &str, title: Option<&str>, markdown: &str) -> String {
        let mut sse_text = String::new();
        let progress_payload = serde_json::json!({
            "status": "processing",
            "url": url,
            "title": title.unwrap_or(""),
        });
        sse_text.push_str(&format!("event: progress\ndata: {}\n\n", progress_payload));
        let paragraphs: Vec<&str> = markdown.split("\n\n").collect();
        for p in paragraphs {
            if !p.trim().is_empty() {
                let data_payload = serde_json::json!({
                    "chunk": p,
                });
                sse_text.push_str(&format!("event: content\ndata: {}\n\n", data_payload));
            }
        }
        let done_payload = serde_json::json!({
            "status": "completed",
            "url": url,
        });
        sse_text.push_str(&format!("event: done\ndata: {}\n\n", done_payload));
        sse_text
    }

    #[tokio::test]
    async fn streamed_sse_bytes_match_the_old_whole_body() {
        use crate::test_engine::{FakeEngine, frames};
        let markdown =
            "# Hello \"q\"\n\n\n\nWorld paragraph\nline two\n\n  \n\n- 列表\n- \u{1F600}";
        let engine = FakeEngine {
            title: Some("Test <Title>".into()),
            markdown: Some(markdown.into()),
            ..FakeEngine::open()
        };
        let app = crate::router(std::sync::Arc::new(engine), crate::Config::default());
        let url = "https://site.example/x";
        let req = axum::http::Request::get(format!("/r/{url}"))
            .header("accept", "text/event-stream")
            .body(axum::body::Body::empty())
            .unwrap();
        let (status, headers, f) = frames(&app, req).await;
        assert_eq!(status, axum::http::StatusCode::OK);
        assert_eq!(headers["content-type"], "text/event-stream; charset=utf-8");
        let want = old_sse_text(url, Some("Test <Title>"), markdown);
        assert_eq!(String::from_utf8(f.concat()).unwrap(), want);
        // 一段一帧：progress、3 个 content、done
        assert_eq!(f.len(), 5);
        // 整块接口字节也不变
        let whole = stream_markdown_sse(url, Some("Test <Title>"), markdown);
        let body = axum::body::to_bytes(whole.into_body(), usize::MAX)
            .await
            .unwrap();
        assert_eq!(body, want.as_bytes());
    }

    /// 抓取很慢时（渲染、慢站点）流里每个心跳间隔有一行注释保活；去掉注释后事件字节和以前一样
    /// （0.90 抓取期间一个字节都不写，代理可能按空闲超时断开）。暂停的时钟上跑，不读真实时间。
    #[tokio::test(start_paused = true)]
    async fn slow_scrapes_send_keepalive_comments_without_changing_events() {
        use crate::test_engine::{FakeEngine, frames};
        let gate = std::sync::Arc::new(tokio::sync::Semaphore::new(0));
        let engine = FakeEngine {
            title: Some("T".into()),
            markdown: Some("# A\n\nB".into()),
            gate: Some(gate.clone()),
        };
        let app = crate::router(std::sync::Arc::new(engine), crate::Config::default());
        let open = gate.clone();
        tokio::spawn(async move {
            // 两个心跳间隔多一点之后放行
            tokio::time::sleep(crate::firecrawl::stream::KEEPALIVE * 2 + KEEPALIVE_SLACK).await;
            open.add_permits(1);
        });
        let url = "https://site.example/slow";
        let req = axum::http::Request::get(format!("/r/{url}"))
            .header("accept", "text/event-stream")
            .body(axum::body::Body::empty())
            .unwrap();
        let (_, _, f) = frames(&app, req).await;
        let keepalive: Vec<_> = f.iter().filter(|x| x.starts_with(b": keepalive")).collect();
        assert_eq!(keepalive.len(), 2, "{f:?}");
        let events: Vec<u8> = f
            .iter()
            .filter(|x| !x.starts_with(b":"))
            .flatten()
            .copied()
            .collect();
        assert_eq!(
            String::from_utf8(events).unwrap(),
            old_sse_text(url, Some("T"), "# A\n\nB")
        );
    }

    /// 放行时刻比第二个心跳晚一点（不在心跳整点上）。
    const KEEPALIVE_SLACK: std::time::Duration = std::time::Duration::from_secs(1);
}
