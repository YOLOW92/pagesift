//! 工人一侧：在一条已建立的连接（TCP、TLS 或测试用的内存管道）上开 HTTP/2 双向流。

use hyper::body::Incoming;
use hyper::header::{AUTHORIZATION, HeaderValue};
use hyper::{Method, Request, StatusCode};
use hyper_util::rt::{TokioExecutor, TokioIo};
use tokio::io::{AsyncRead, AsyncWrite};

use super::{FrameReader, Outbox, PATH, PROTO_MAX, PROTO_MIN, VERSION_HEADER, WORKER_HEADER};
use crate::types::{Error, WorkerId};

/// 连到网关所需的身份与参数。
#[derive(Debug, Clone)]
pub struct Dial {
    pub worker: WorkerId,
    pub token: String,
    /// `:authority`（网关的主机名和端口）。
    pub authority: String,
    /// 连接是否已经过 TLS（决定 `:scheme`）。
    pub tls: bool,
    pub max_frame: usize,
}

/// 握手成功后的双向流。
pub struct Stream {
    pub out: Outbox,
    pub reader: FrameReader<Incoming>,
    /// 网关选定的协议版本。
    pub version: u32,
}

fn err(e: impl std::fmt::Display) -> Error {
    Error::Transport(e.to_string())
}

/// 在 `io` 上做 HTTP/2 握手（h2c 时是先验知识方式），发起帧流请求并检查认证与版本。
pub async fn open<IO>(io: IO, d: &Dial) -> Result<Stream, Error>
where
    IO: AsyncRead + AsyncWrite + Unpin + Send + 'static,
{
    let (mut send, conn) =
        hyper::client::conn::http2::handshake(TokioExecutor::new(), TokioIo::new(io))
            .await
            .map_err(err)?;
    tokio::spawn(async move {
        // 连接结束（对方关闭或出错）时两端的帧流都会结束，由读帧的一方处理
        let _ = conn.await;
    });
    let (out, body) = Outbox::pair();
    let scheme = if d.tls { "https" } else { "http" };
    let req = Request::builder()
        .method(Method::POST)
        .uri(format!("{scheme}://{}{PATH}", d.authority))
        .header(
            AUTHORIZATION,
            HeaderValue::from_str(&format!("Bearer {}", d.token)).map_err(err)?,
        )
        .header(
            WORKER_HEADER,
            HeaderValue::from_str(d.worker.as_str()).map_err(err)?,
        )
        .header(VERSION_HEADER, format!("{PROTO_MIN}-{PROTO_MAX}"))
        .body(body)
        .map_err(err)?;
    let resp = send.send_request(req).await.map_err(err)?;
    let reason = || {
        resp.headers()
            .get(super::server::ERROR_HEADER)
            .and_then(|v| v.to_str().ok())
            .unwrap_or_default()
            .to_owned()
    };
    match resp.status() {
        StatusCode::OK => {}
        StatusCode::UNAUTHORIZED => {
            return Err(Error::Rejected(format!("unauthorized: {}", reason())));
        }
        s => return Err(Error::Rejected(format!("{s}: {}", reason()))),
    }
    // 网关选定的版本必须在本工人支持的区间里
    let version = resp
        .headers()
        .get(VERSION_HEADER)
        .and_then(|v| v.to_str().ok())
        .and_then(|v| v.parse::<u32>().ok())
        .filter(|v| (PROTO_MIN..=PROTO_MAX).contains(v))
        .ok_or_else(|| Error::Rejected("gateway chose an unsupported version".into()))?;
    Ok(Stream {
        out,
        reader: FrameReader::new(resp.into_body(), d.max_frame),
        version,
    })
}
