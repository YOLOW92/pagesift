//! 帧协议的承载：HTTP/2 长连接上的一个双向流。
//!
//! 工人主动连网关（工人在 NAT 或只出不进的网络里也能工作），发一个 `POST /pagesift-cluster`：
//! 请求体是工人 → 网关的帧流，响应体是网关 → 工人的帧流，两边都不结束，直到连接关闭。
//! 认证与版本协商在请求头里完成，失败时网关直接回 401 / 400，不进入帧流。
//!
//! - h2c（明文 HTTP/2，先验知识方式，RFC 9113 §3.3）只用于部署者显式声明可信的内网；
//! - TLS（rustls，ALPN `h2`）为默认，可选双向 TLS，见 [`tls`]。
//!
//! 复用 workspace 已有的 hyper 1.x、hyper-util、http-body，不新增依赖。

pub mod auth;
pub mod client;
pub mod server;
pub mod tls;

use std::convert::Infallible;
use std::pin::Pin;
use std::task::{Context, Poll};

use bytes::{Buf, Bytes, BytesMut};
use http_body::{Body, Frame};
use http_body_util::BodyExt;
use tokio::sync::mpsc;

use crate::frame::{self, Msg};
use crate::types::Error;

/// 请求路径。
pub const PATH: &str = "/pagesift-cluster";
/// 版本协商头：请求里是工人支持的 `最低-最高`，应答里是选定的版本。
pub const VERSION_HEADER: &str = "pagesift-cluster-version";
/// 工人 id 头（与 `Hello` 里的 id 必须一致）。
pub const WORKER_HEADER: &str = "pagesift-worker";

/// [协议] 本实现支持的协议版本区间；改帧格式或语义时加一，旧版本在一个发布周期内继续接受（滚动升级）
pub const PROTO_MIN: u32 = 1;
/// [协议] 同上
pub const PROTO_MAX: u32 = 1;

/// 版本协商：两边区间的交集里取最高；没有交集时为 `None`。
pub fn negotiate(offer: &str) -> Option<u32> {
    let (lo, hi) = offer.split_once('-')?;
    let (lo, hi): (u32, u32) = (lo.trim().parse().ok()?, hi.trim().parse().ok()?);
    let chosen = hi.min(PROTO_MAX);
    (chosen >= lo.max(PROTO_MIN) && lo <= hi).then_some(chosen)
}

/// 帧的默认上限：一个结果必须能和完成标记写进同一条存储记录，而记录不能跨段，
/// 所以取 store 的默认段大小（`pagesift_store::Config::max_segment_bytes`）。
pub fn default_max_frame() -> usize {
    pagesift_store::Config::new("").max_segment_bytes as usize
}

/// 由通道喂数据的流式消息体。
pub(crate) struct ChanBody {
    rx: mpsc::UnboundedReceiver<Bytes>,
}

impl Body for ChanBody {
    type Data = Bytes;
    type Error = Infallible;

    fn poll_frame(
        mut self: Pin<&mut Self>,
        cx: &mut Context<'_>,
    ) -> Poll<Option<Result<Frame<Bytes>, Infallible>>> {
        self.rx.poll_recv(cx).map(|b| b.map(|b| Ok(Frame::data(b))))
    }
}

/// 发送端：编码好的帧放进通道，由 hyper 按 HTTP/2 流控写出。
#[derive(Clone)]
pub struct Outbox {
    tx: mpsc::UnboundedSender<Bytes>,
}

impl Outbox {
    pub(crate) fn pair() -> (Self, ChanBody) {
        let (tx, rx) = mpsc::unbounded_channel();
        (Self { tx }, ChanBody { rx })
    }

    pub fn send(&self, corr: u64, m: &Msg) -> Result<(), Error> {
        self.tx
            .send(Bytes::from(frame::encode(corr, m)))
            .map_err(|_| Error::Closed)
    }

    pub fn is_closed(&self) -> bool {
        self.tx.is_closed()
    }
}

/// 接收端：从 HTTP/2 消息体里切出完整的帧。
pub struct FrameReader<B> {
    body: B,
    buf: BytesMut,
    max: usize,
}

impl<B> FrameReader<B>
where
    B: Body<Data = Bytes> + Unpin,
    B::Error: std::fmt::Display,
{
    pub(crate) fn new(body: B, max: usize) -> Self {
        Self {
            body,
            buf: BytesMut::new(),
            max,
        }
    }

    /// 下一帧；对方正常结束时为 `Ok(None)`。
    pub async fn next(&mut self) -> Result<Option<(u64, Msg)>, Error> {
        loop {
            if let Some(range) = frame::split(&self.buf, self.max)? {
                let end = range.end;
                let out = frame::decode(&self.buf[range])?;
                self.buf.advance(end);
                return Ok(Some(out));
            }
            match self.body.frame().await {
                None => {
                    return if self.buf.is_empty() {
                        Ok(None)
                    } else {
                        Err(Error::Transport("stream ended inside a frame".into()))
                    };
                }
                Some(Err(e)) => return Err(Error::Transport(e.to_string())),
                Some(Ok(f)) => {
                    if let Ok(data) = f.into_data() {
                        self.buf.extend_from_slice(&data);
                    }
                }
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn version_negotiation() {
        assert_eq!(negotiate("1-1"), Some(1));
        assert_eq!(negotiate("1-9"), Some(PROTO_MAX));
        assert_eq!(negotiate("0-0"), None);
        assert_eq!(
            negotiate(&format!("{}-{}", PROTO_MAX + 1, PROTO_MAX + 2)),
            None
        );
        assert_eq!(negotiate("x"), None);
        assert_eq!(negotiate("2-1"), None);
    }
}
