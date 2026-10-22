//! Jina Reader protocol adapter for PageSift.
//! Implements `GET /r/{*url}` and `POST /r` for direct markdown and SSE event streaming.

pub mod reader_route;
pub mod stream_sse;

use axum::routing::{get, post};
use axum::Router;
use std::sync::Arc;

use crate::engine::Engine;
pub use reader_route::{handle_jina_get, handle_jina_post};
pub use stream_sse::stream_markdown_sse;

/// Mount Jina Reader protocol routes onto the router.
pub fn jina_router(engine: Arc<dyn Engine>) -> Router {
    Router::new()
        .route("/r/{*url}", get(handle_jina_get))
        .route("/r", post(handle_jina_post))
        .route("/r/", post(handle_jina_post))
        .with_state(engine)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_sse_event_formatting() {
        let resp = stream_markdown_sse("https://example.com", Some("Test Title"), "# Hello\n\nWorld paragraph");
        assert_eq!(resp.status(), axum::http::StatusCode::OK);
        assert_eq!(
            resp.headers().get("content-type").unwrap(),
            "text/event-stream; charset=utf-8"
        );
    }
}
