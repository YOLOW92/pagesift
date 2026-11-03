//! 网关一侧：在一条已接受的连接上跑 HTTP/2 服务，校验认证与版本后把帧流交给会话处理者。

use std::convert::Infallible;
use std::sync::Arc;

use hyper::body::Incoming;
use hyper::header::AUTHORIZATION;
use hyper::service::service_fn;
use hyper::{Method, Request, Response, StatusCode};
use hyper_util::rt::{TokioExecutor, TokioIo};
use tokio::io::{AsyncRead, AsyncWrite};

use super::auth::TokenStore;
use super::{ChanBody, FrameReader, Outbox, PATH, VERSION_HEADER, WORKER_HEADER, negotiate};
use crate::coordinator::BoxFuture;
use crate::types::{Error, WorkerId};

/// 拒绝原因放在这个响应头里。
pub const ERROR_HEADER: &str = "pagesift-cluster-error";

/// 接受帧流的一方（网关）。
pub trait Accept: Send + Sync + 'static {
    fn tokens(&self) -> &TokenStore;
    fn max_frame(&self) -> usize;
    /// 一个通过认证的帧流。返回的 future 在会话结束时完成。
    fn session(
        self: Arc<Self>,
        worker: WorkerId,
        reader: FrameReader<Incoming>,
        out: Outbox,
    ) -> BoxFuture<'static, ()>;
}

fn reject(status: StatusCode, why: &str) -> Response<ChanBody> {
    let (_, body) = Outbox::pair();
    let mut r = Response::new(body);
    *r.status_mut() = status;
    if let Ok(v) = why.parse() {
        r.headers_mut().insert(ERROR_HEADER, v);
    }
    r
}

fn handle<H: Accept>(h: &Arc<H>, req: Request<Incoming>) -> Response<ChanBody> {
    if req.method() != Method::POST || req.uri().path() != PATH {
        return reject(StatusCode::NOT_FOUND, "not a pagesift cluster endpoint");
    }
    let header = |name| req.headers().get(name).and_then(|v| v.to_str().ok());
    let Some(version) = header(VERSION_HEADER).and_then(negotiate) else {
        return reject(
            StatusCode::BAD_REQUEST,
            &format!(
                "no common protocol version (gateway supports {}-{})",
                super::PROTO_MIN,
                super::PROTO_MAX
            ),
        );
    };
    let Some(worker) = header(WORKER_HEADER).map(WorkerId::new) else {
        return reject(StatusCode::UNAUTHORIZED, "missing worker id");
    };
    let token = header(AUTHORIZATION.as_str())
        .and_then(|v| v.strip_prefix("Bearer "))
        .unwrap_or_default();
    if !h.tokens().verify(&worker, token) {
        return reject(StatusCode::UNAUTHORIZED, "bad token");
    }
    let (out, body) = Outbox::pair();
    let reader = FrameReader::new(req.into_body(), h.max_frame());
    tokio::spawn(Arc::clone(h).session(worker, reader, out));
    let mut r = Response::new(body);
    r.headers_mut().insert(VERSION_HEADER, version.into());
    r
}

/// 在 `io` 上服务一条 HTTP/2 连接（h2c，或 [`super::tls::accept`] 之后的 TLS 流），直到连接关闭。
pub async fn serve_connection<IO, H>(io: IO, h: Arc<H>) -> Result<(), Error>
where
    IO: AsyncRead + AsyncWrite + Unpin + Send + 'static,
    H: Accept,
{
    let svc = service_fn(move |req| {
        let r = handle(&h, req);
        async move { Ok::<_, Infallible>(r) }
    });
    hyper::server::conn::http2::Builder::new(TokioExecutor::new())
        .serve_connection(TokioIo::new(io), svc)
        .await
        .map_err(|e| Error::Transport(e.to_string()))
}
