//! 边产出边写出的响应体：生产方往通道里写一段段字节，连接上每次写出一段，不先拼成整块。
//!
//! 通道只缓冲一段：客户端读得慢，生产方就等着（背压）；客户端断开，响应体随之释放，
//! 生产方的下一次发送失败，`Sender::closed` 也会就绪，生产方据此停下。

use std::convert::Infallible;
use std::pin::Pin;
use std::task::{Context, Poll};

use axum::body::{Body, Bytes, HttpBody};
use http_body::Frame;
use tokio::sync::mpsc;

/// 新建一个流式响应体和往里写的发送端。
pub(crate) fn channel() -> (mpsc::Sender<Bytes>, Body) {
    let (tx, rx) = mpsc::channel(1);
    (tx, Body::new(ChannelBody { rx }))
}

struct ChannelBody {
    rx: mpsc::Receiver<Bytes>,
}

impl HttpBody for ChannelBody {
    type Data = Bytes;
    type Error = Infallible;

    fn poll_frame(
        self: Pin<&mut Self>,
        cx: &mut Context<'_>,
    ) -> Poll<Option<Result<Frame<Bytes>, Infallible>>> {
        self.get_mut()
            .rx
            .poll_recv(cx)
            .map(|b| b.map(|b| Ok(Frame::data(b))))
    }
}
