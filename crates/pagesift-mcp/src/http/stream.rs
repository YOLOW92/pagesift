//! 响应体的流：SSE 转发通知直到最终响应，JSON 只等最终响应；连接断开时取消任务。

use std::convert::Infallible;
use std::pin::Pin;
use std::task::{Context, Poll};

use axum::body::Bytes;
use axum::http::{HeaderValue, header};
use axum::response::Response;
use axum::response::sse::Event;
use futures_util::Stream;
use serde_json::Value;
use tokio::sync::mpsc;
use tokio::task::JoinHandle;

pub(super) struct AbortOnDrop(pub(super) JoinHandle<()>);

impl Drop for AbortOnDrop {
    fn drop(&mut self) {
        self.0.abort();
    }
}

/// SSE 流：先转发通知，收到最终响应后结束。
pub(super) struct ResponseStream {
    pub(super) rx: mpsc::UnboundedReceiver<Value>,
    pub(super) _guard: AbortOnDrop,
    pub(super) done: bool,
}

impl Stream for ResponseStream {
    type Item = Result<Event, Infallible>;

    fn poll_next(mut self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<Option<Self::Item>> {
        if self.done {
            return Poll::Ready(None);
        }
        match self.rx.poll_recv(cx) {
            Poll::Ready(Some(v)) => {
                // 响应带 id 且没有 method。
                if v.get("method").is_none() {
                    self.done = true;
                }
                let data = serde_json::to_string(&v).unwrap_or_default();
                Poll::Ready(Some(Ok(Event::default().event("message").data(data))))
            }
            Poll::Ready(None) => Poll::Ready(None),
            Poll::Pending => Poll::Pending,
        }
    }
}

pub(super) struct JsonReply {
    pub(super) rx: mpsc::UnboundedReceiver<Value>,
    pub(super) _guard: AbortOnDrop,
}

impl JsonReply {
    /// 返回一个 body 是"等待最终响应"的流式 JSON 响应，这样连接断开能被感知并取消任务。
    pub(super) fn into_response_future(self) -> Response {
        let body = axum::body::Body::from_stream(JsonBody { reply: Some(self) });
        let mut resp = Response::new(body);
        resp.headers_mut().insert(
            header::CONTENT_TYPE,
            HeaderValue::from_static("application/json"),
        );
        resp
    }
}

pub(super) struct JsonBody {
    reply: Option<JsonReply>,
}

impl Stream for JsonBody {
    type Item = Result<Bytes, Infallible>;

    fn poll_next(mut self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<Option<Self::Item>> {
        let Some(reply) = self.reply.as_mut() else {
            return Poll::Ready(None);
        };
        loop {
            match reply.rx.poll_recv(cx) {
                // JSON 模式下不转发通知。
                Poll::Ready(Some(v)) if v.get("method").is_some() => continue,
                Poll::Ready(Some(v)) => {
                    self.reply = None;
                    let bytes = serde_json::to_vec(&v).unwrap_or_default();
                    return Poll::Ready(Some(Ok(Bytes::from(bytes))));
                }
                Poll::Ready(None) => {
                    self.reply = None;
                    return Poll::Ready(None);
                }
                Poll::Pending => return Poll::Pending,
            }
        }
    }
}

/// 不产生事件的 SSE 流，内部流结束（会话关闭）时结束。
pub(super) struct NoEvents<S>(pub(super) S);

impl<S: Stream<Item = ()> + Unpin> Stream for NoEvents<S> {
    type Item = Result<Event, Infallible>;

    fn poll_next(mut self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<Option<Self::Item>> {
        match Pin::new(&mut self.0).poll_next(cx) {
            Poll::Ready(_) => Poll::Ready(None),
            Poll::Pending => Poll::Pending,
        }
    }
}
