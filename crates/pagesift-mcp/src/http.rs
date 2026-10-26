//! Streamable HTTP 传输（axum）。
//!
//! 同一个端点同时服务两代客户端：
//! - 新版（2026-07-28）：无状态，每个 POST 带 `MCP-Protocol-Version`、`Mcp-Method`、`Mcp-Name`
//!   头并与请求体核对；GET/DELETE 对新版无意义。
//! - 旧版（2025-11-25 及更早）：`initialize` 时分配 `Mcp-Session-Id`，GET 打开服务器推送的
//!   SSE 流，DELETE 结束会话，`notifications/cancelled` 取消请求。
//!
//! 所有请求都先校验 `Host` 和 `Origin`，防止 DNS 重绑定。

use std::collections::HashMap;
use std::convert::Infallible;
use std::future::Future;
use std::io;
use std::net::SocketAddr;
use std::pin::Pin;
use std::sync::{Arc, Mutex};
use std::task::{Context, Poll};
use std::time::{Duration, Instant};

use axum::Router;
use axum::body::Bytes;
use axum::extract::{DefaultBodyLimit, State};
use axum::http::{HeaderMap, HeaderValue, Method, StatusCode, header};
use axum::response::sse::{Event, KeepAlive, Sse};
use axum::response::{IntoResponse, Response};
use axum::routing::any;
use base64::Engine as _;
use futures_util::Stream;
use serde_json::{Value, json};
use tokio::net::TcpListener;
use tokio::sync::{mpsc, watch};
use tokio::task::{AbortHandle, JoinHandle};

use crate::jsonrpc::{self, ErrorObject, Id, Message, Notification, Parsed, Request};
use crate::protocol::{self, Era};
use crate::server::McpServer;

const SESSION_HEADER: &str = "mcp-session-id";
const VERSION_HEADER: &str = "mcp-protocol-version";
const METHOD_HEADER: &str = "mcp-method";
const NAME_HEADER: &str = "mcp-name";

/// HTTP 传输配置。
#[derive(Debug, Clone)]
pub struct HttpConfig {
    /// 监听地址，默认只监听回环 `127.0.0.1:3003`。
    pub bind: SocketAddr,
    /// MCP 端点路径，默认 `/mcp`。
    pub path: String,
    /// 允许的 `Host` 主机名（不含端口，大小写不敏感）。`None` 表示不检查；
    /// 默认只允许回环名称。监听公网地址时需要改成实际域名。
    pub allowed_hosts: Option<Vec<String>>,
    /// 额外允许的 `Origin`（完整写法，如 `https://app.example.com`）。
    pub allowed_origins: Vec<String>,
    /// 是否放行主机为回环地址的 `Origin`（任意端口），默认开启。
    pub allow_loopback_origins: bool,
    pub max_body_bytes: usize,
    /// 旧版会话上限和空闲超时。
    pub max_sessions: usize,
    pub session_idle_timeout: Duration,
    pub sse_keep_alive: Duration,
}

impl Default for HttpConfig {
    fn default() -> Self {
        Self {
            bind: SocketAddr::from(([127, 0, 0, 1], 3003)),
            path: "/mcp".into(),
            allowed_hosts: Some(vec!["localhost".into(), "127.0.0.1".into(), "[::1]".into()]),
            allowed_origins: Vec::new(),
            allow_loopback_origins: true,
            max_body_bytes: 4 * 1024 * 1024,
            max_sessions: 1024,
            session_idle_timeout: Duration::from_secs(30 * 60),
            sse_keep_alive: Duration::from_secs(15),
        }
    }
}

/// 生成挂好 MCP 端点的 router，可以合并进其他 axum 应用。
pub fn router(server: McpServer, config: &HttpConfig) -> Router {
    let state = Arc::new(Shared {
        server,
        config: config.clone(),
        sessions: Mutex::new(HashMap::new()),
    });
    Router::new()
        .route(&config.path, any(endpoint))
        .layer(DefaultBodyLimit::max(config.max_body_bytes))
        .with_state(state)
}

/// 绑定 `config.bind` 并一直服务。
pub async fn serve_http(server: McpServer, config: HttpConfig) -> io::Result<()> {
    let listener = TcpListener::bind(config.bind).await?;
    tracing::info!(addr = %listener.local_addr()?, path = %config.path, "MCP HTTP listening");
    serve_listener(server, &config, listener, std::future::pending()).await
}

/// 在已绑定的监听器上服务，`shutdown` 完成后优雅退出。
pub async fn serve_listener(
    server: McpServer,
    config: &HttpConfig,
    listener: TcpListener,
    shutdown: impl Future<Output = ()> + Send + 'static,
) -> io::Result<()> {
    use axum::serve::ListenerExt;
    // 响应头先发、正文分块后发：不关 Nagle 的话，保持连接的客户端每次调用都要多等约 40 ms 的延迟确认
    let listener = listener.tap_io(|tcp| {
        let _ = tcp.set_nodelay(true);
    });
    axum::serve(listener, router(server, config))
        .with_graceful_shutdown(shutdown)
        .await
}

struct Shared {
    server: McpServer,
    config: HttpConfig,
    sessions: Mutex<HashMap<String, Arc<Session>>>,
}

struct Session {
    version: &'static str,
    inflight: Mutex<HashMap<Id, AbortHandle>>,
    last_seen: Mutex<Instant>,
    closed: watch::Sender<bool>,
}

impl Session {
    fn close(&self) {
        let _ = self.closed.send(true);
        for (_, h) in lock(&self.inflight).drain() {
            h.abort();
        }
    }
}

fn lock<T>(m: &Mutex<T>) -> std::sync::MutexGuard<'_, T> {
    m.lock().unwrap_or_else(|e| e.into_inner())
}

async fn endpoint(
    State(st): State<Arc<Shared>>,
    method: Method,
    headers: HeaderMap,
    body: Bytes,
) -> Response {
    if let Err(r) = check_host_and_origin(&st.config, &headers) {
        return *r;
    }
    match method {
        Method::POST => post(st, headers, body).await,
        Method::GET => get(&st, &headers),
        Method::DELETE => delete(&st, &headers),
        _ => method_not_allowed(),
    }
}

fn method_not_allowed() -> Response {
    (
        StatusCode::METHOD_NOT_ALLOWED,
        [(header::ALLOW, "GET, POST, DELETE")],
    )
        .into_response()
}

// ---------- 安全检查 ----------

fn check_host_and_origin(cfg: &HttpConfig, headers: &HeaderMap) -> Result<(), Box<Response>> {
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
fn host_without_port(h: &str) -> &str {
    if h.starts_with('[') {
        return h.find(']').map_or(h, |i| &h[..=i]);
    }
    h.rsplit_once(':').map_or(h, |(host, _)| host)
}

fn origin_allowed(cfg: &HttpConfig, origin: &str) -> bool {
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

fn forbidden(why: &str) -> Response {
    // 规范允许附上不带 id 的 JSON-RPC 错误。
    let body = json!({ "jsonrpc": "2.0", "error": { "code": jsonrpc::INVALID_REQUEST, "message": format!("Forbidden: {why}") } });
    json_response(StatusCode::FORBIDDEN, body)
}

// ---------- POST ----------

struct Accept {
    json: bool,
    sse: bool,
}

fn parse_accept(headers: &HeaderMap) -> Accept {
    let Some(v) = headers.get(header::ACCEPT).and_then(|v| v.to_str().ok()) else {
        return Accept {
            json: true,
            sse: false,
        };
    };
    let mut a = Accept {
        json: false,
        sse: false,
    };
    for part in v.split(',') {
        let mime = part
            .split(';')
            .next()
            .unwrap_or("")
            .trim()
            .to_ascii_lowercase();
        match mime.as_str() {
            "*/*" => {
                a.json = true;
                a.sse = true;
            }
            "application/json" | "application/*" => a.json = true,
            "text/event-stream" | "text/*" => a.sse = true,
            _ => {}
        }
    }
    a
}

fn header_str<'a>(headers: &'a HeaderMap, name: &str) -> Option<&'a str> {
    headers.get(name).and_then(|v| v.to_str().ok())
}

fn rpc_error(status: StatusCode, id: Option<Id>, e: ErrorObject) -> Response {
    json_response(status, jsonrpc::Response::error(id, e).to_value())
}

fn json_response(status: StatusCode, body: Value) -> Response {
    let bytes = serde_json::to_vec(&body).unwrap_or_default();
    (
        status,
        [(
            header::CONTENT_TYPE,
            HeaderValue::from_static("application/json"),
        )],
        bytes,
    )
        .into_response()
}

async fn post(st: Arc<Shared>, headers: HeaderMap, body: Bytes) -> Response {
    let content_type = header_str(&headers, header::CONTENT_TYPE.as_str()).unwrap_or("");
    let ct = content_type.split(';').next().unwrap_or("").trim();
    if !ct.eq_ignore_ascii_case("application/json") {
        return (
            StatusCode::UNSUPPORTED_MEDIA_TYPE,
            "Content-Type must be application/json",
        )
            .into_response();
    }
    let accept = parse_accept(&headers);
    if !accept.json && !accept.sse {
        return (
            StatusCode::NOT_ACCEPTABLE,
            "Accept must include application/json or text/event-stream",
        )
            .into_response();
    }
    let parsed = match jsonrpc::parse(&body) {
        Ok(p) => p,
        Err(e) => return json_response(StatusCode::BAD_REQUEST, e.to_value()),
    };
    let session = match lookup_session(&st, &headers) {
        Ok(s) => s,
        Err(r) => return *r,
    };
    let msg = match parsed {
        Parsed::Batch(items) => return post_batch(&st, session, items).await,
        Parsed::Single(m) => m,
    };
    match msg {
        Message::Response(_) => match session {
            Some(_) => StatusCode::ACCEPTED.into_response(),
            None => rpc_error(
                StatusCode::BAD_REQUEST,
                None,
                ErrorObject::invalid_request("Invalid Request: clients must not send responses"),
            ),
        },
        Message::Notification(n) => {
            if let Some(s) = &session {
                session_notification(s, &n);
            }
            StatusCode::ACCEPTED.into_response()
        }
        Message::Request(req) => post_request(st, &headers, accept, session, req).await,
    }
}

/// 有 `Mcp-Session-Id` 时找出会话：找不到返回 404。
fn lookup_session(st: &Shared, headers: &HeaderMap) -> Result<Option<Arc<Session>>, Box<Response>> {
    let Some(sid) = headers.get(SESSION_HEADER) else {
        return Ok(None);
    };
    let found = sid
        .to_str()
        .ok()
        .and_then(|sid| lock(&st.sessions).get(sid).cloned());
    match found {
        Some(s) => {
            *lock(&s.last_seen) = Instant::now();
            Ok(Some(s))
        }
        None => Err(Box::new(rpc_error(
            StatusCode::NOT_FOUND,
            None,
            ErrorObject::invalid_request("Session not found; send a new initialize request"),
        ))),
    }
}

fn session_notification(s: &Session, n: &Notification) {
    if n.method == "notifications/cancelled"
        && let Some(id) = n
            .params
            .as_ref()
            .and_then(|p| p.get("requestId"))
            .and_then(Id::from_value)
        && let Some(h) = lock(&s.inflight).remove(&id)
    {
        h.abort();
    }
}

async fn post_request(
    st: Arc<Shared>,
    headers: &HeaderMap,
    accept: Accept,
    session: Option<Arc<Session>>,
    req: Request,
) -> Response {
    let id = req.id.clone();
    let is_modern = req
        .params
        .as_ref()
        .and_then(|p| p.get("_meta"))
        .and_then(|m| m.get(protocol::META_PROTOCOL_VERSION))
        .is_some();

    // 新版：校验镜像头，按请求无状态处理（忽略会话头）。
    if is_modern {
        if let Err(e) = check_modern_headers(headers, &req) {
            return rpc_error(StatusCode::BAD_REQUEST, Some(id), e);
        }
        let era = match st.server.classify(&req, None) {
            Ok(era) => era,
            Err(e) => return rpc_error(StatusCode::BAD_REQUEST, Some(id), e),
        };
        if !McpServer::has_method(era, &req.method) {
            return rpc_error(
                StatusCode::NOT_FOUND,
                Some(id),
                ErrorObject::method_not_found(&req.method),
            );
        }
        return respond(&st, accept, req, era, None);
    }

    if req.method == "initialize" {
        return initialize(&st, &req);
    }

    let Some(session) = session else {
        return rpc_error(
            StatusCode::BAD_REQUEST,
            Some(id),
            ErrorObject::invalid_params(format!(
                "missing Mcp-Session-Id header or _meta[\"{}\"]; clients on protocol {} or earlier must send \"initialize\" first",
                protocol::META_PROTOCOL_VERSION,
                protocol::LEGACY_VERSIONS[0]
            ))
            .with_data(json!({ "supported": protocol::supported_versions() })),
        );
    };
    if let Some(v) = header_str(headers, VERSION_HEADER)
        && protocol::find_version(protocol::LEGACY_VERSIONS, v).is_none()
    {
        return rpc_error(
            StatusCode::BAD_REQUEST,
            Some(id),
            crate::server::unsupported_version(v),
        );
    }
    let era = match st.server.classify(&req, Some(session.version)) {
        Ok(era) => era,
        Err(e) => return rpc_error(StatusCode::OK, Some(id), e),
    };
    respond(&st, accept, req, era, Some(session))
}

fn check_modern_headers(headers: &HeaderMap, req: &Request) -> Result<(), ErrorObject> {
    let mismatch = |msg: String| ErrorObject::new(protocol::HEADER_MISMATCH, msg);
    for name in [VERSION_HEADER, METHOD_HEADER, NAME_HEADER] {
        if headers.get_all(name).iter().count() > 1 {
            return Err(mismatch(format!(
                "Header mismatch: {name} header appears more than once"
            )));
        }
    }
    let body_version = req
        .params
        .as_ref()
        .and_then(|p| p.get("_meta"))
        .and_then(|m| m.get(protocol::META_PROTOCOL_VERSION))
        .and_then(Value::as_str)
        .unwrap_or("");
    match header_str(headers, VERSION_HEADER) {
        None => {
            return Err(mismatch(
                "Header mismatch: missing MCP-Protocol-Version header".into(),
            ));
        }
        Some(h) if h != body_version => {
            return Err(mismatch(format!(
                "Header mismatch: MCP-Protocol-Version header value '{h}' does not match body value '{body_version}'"
            )));
        }
        Some(_) => {}
    }
    match header_str(headers, METHOD_HEADER) {
        None => {
            return Err(mismatch(
                "Header mismatch: missing Mcp-Method header".into(),
            ));
        }
        Some(h) if h != req.method => {
            return Err(mismatch(format!(
                "Header mismatch: Mcp-Method header value '{h}' does not match body value '{}'",
                req.method
            )));
        }
        Some(_) => {}
    }
    if matches!(
        req.method.as_str(),
        "tools/call" | "prompts/get" | "resources/read"
    ) {
        let key = if req.method == "resources/read" {
            "uri"
        } else {
            "name"
        };
        let body_name = req
            .params
            .as_ref()
            .and_then(|p| p.get(key))
            .and_then(Value::as_str);
        let Some(raw) = header_str(headers, NAME_HEADER) else {
            return Err(mismatch("Header mismatch: missing Mcp-Name header".into()));
        };
        let decoded = decode_header_value(raw)
            .ok_or_else(|| mismatch("Header mismatch: Mcp-Name header is malformed".into()))?;
        if Some(decoded.as_str()) != body_name {
            return Err(mismatch(format!(
                "Header mismatch: Mcp-Name header value '{decoded}' does not match body value '{}'",
                body_name.unwrap_or("")
            )));
        }
    }
    Ok(())
}

/// 解码 `=?base64?...?=` 形式的头值；普通值原样返回。
fn decode_header_value(raw: &str) -> Option<String> {
    match raw
        .strip_prefix("=?base64?")
        .and_then(|r| r.strip_suffix("?="))
    {
        Some(b64) => {
            let bytes = base64::engine::general_purpose::STANDARD.decode(b64).ok()?;
            String::from_utf8(bytes).ok()
        }
        None => Some(raw.to_owned()),
    }
}

fn initialize(st: &Shared, req: &Request) -> Response {
    let (version, result) = match st.server.initialize(req) {
        Ok(ok) => ok,
        Err(e) => return rpc_error(StatusCode::BAD_REQUEST, Some(req.id.clone()), e),
    };
    let sid = match new_session_id() {
        Ok(s) => s,
        Err(e) => {
            return rpc_error(
                StatusCode::INTERNAL_SERVER_ERROR,
                Some(req.id.clone()),
                ErrorObject::internal(format!("cannot create session: {e}")),
            );
        }
    };
    {
        let mut sessions = lock(&st.sessions);
        let timeout = st.config.session_idle_timeout;
        sessions.retain(|_, s| {
            let keep = lock(&s.last_seen).elapsed() < timeout;
            if !keep {
                s.close();
            }
            keep
        });
        if sessions.len() >= st.config.max_sessions {
            return rpc_error(
                StatusCode::SERVICE_UNAVAILABLE,
                Some(req.id.clone()),
                ErrorObject::internal("too many sessions"),
            );
        }
        sessions.insert(
            sid.clone(),
            Arc::new(Session {
                version,
                inflight: Mutex::new(HashMap::new()),
                last_seen: Mutex::new(Instant::now()),
                closed: watch::channel(false).0,
            }),
        );
    }
    let mut resp = json_response(
        StatusCode::OK,
        jsonrpc::Response::result(req.id.clone(), result).to_value(),
    );
    if let Ok(v) = HeaderValue::from_str(&sid) {
        resp.headers_mut().insert(SESSION_HEADER, v);
    }
    resp
}

fn new_session_id() -> Result<String, getrandom::Error> {
    let mut b = [0u8; 16];
    getrandom::fill(&mut b)?;
    Ok(b.iter().map(|x| format!("{x:02x}")).collect())
}

/// 执行请求，按客户端的 Accept 和是否要进度选择 JSON 或 SSE。
fn respond(
    st: &Shared,
    accept: Accept,
    req: Request,
    era: Era,
    session: Option<Arc<Session>>,
) -> Response {
    let wants_progress = req
        .params
        .as_ref()
        .and_then(|p| p.get("_meta"))
        .and_then(|m| m.get("progressToken"))
        .is_some();
    let use_sse = accept.sse && (wants_progress || !accept.json);

    let (tx, rx) = mpsc::unbounded_channel::<Value>();
    let server = st.server.clone();
    let id = req.id.clone();
    let task_session = session.clone();
    let task_id = id.clone();
    let notify = use_sse.then(|| tx.clone());
    let task = tokio::spawn(async move {
        let resp = server.dispatch(req, era, notify).await;
        if let Some(s) = &task_session {
            lock(&s.inflight).remove(&task_id);
        }
        let _ = tx.send(resp.to_value());
    });
    if let Some(s) = &session {
        lock(&s.inflight).insert(id, task.abort_handle());
    }
    // 连接断开时 guard 被丢弃，任务随之取消。
    let guard = AbortOnDrop(task);

    if use_sse {
        let stream = ResponseStream {
            rx,
            _guard: guard,
            done: false,
        };
        let mut resp = Sse::new(stream).into_response();
        resp.headers_mut()
            .insert("x-accel-buffering", HeaderValue::from_static("no"));
        resp.headers_mut()
            .insert(header::CACHE_CONTROL, HeaderValue::from_static("no-cache"));
        resp
    } else {
        JsonReply { rx, _guard: guard }.into_response_future()
    }
}

struct AbortOnDrop(JoinHandle<()>);

impl Drop for AbortOnDrop {
    fn drop(&mut self) {
        self.0.abort();
    }
}

/// SSE 流：先转发通知，收到最终响应后结束。
struct ResponseStream {
    rx: mpsc::UnboundedReceiver<Value>,
    _guard: AbortOnDrop,
    done: bool,
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

struct JsonReply {
    rx: mpsc::UnboundedReceiver<Value>,
    _guard: AbortOnDrop,
}

impl JsonReply {
    /// 返回一个 body 是"等待最终响应"的流式 JSON 响应，这样连接断开能被感知并取消任务。
    fn into_response_future(self) -> Response {
        let body = axum::body::Body::from_stream(JsonBody { reply: Some(self) });
        let mut resp = Response::new(body);
        resp.headers_mut().insert(
            header::CONTENT_TYPE,
            HeaderValue::from_static("application/json"),
        );
        resp
    }
}

struct JsonBody {
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

async fn post_batch(
    st: &Shared,
    session: Option<Arc<Session>>,
    items: Vec<Result<Message, jsonrpc::Response>>,
) -> Response {
    let Some(session) = session.filter(|s| protocol::BATCH_VERSIONS.contains(&s.version)) else {
        return rpc_error(
            StatusCode::BAD_REQUEST,
            None,
            ErrorObject::invalid_request(
                "Invalid Request: JSON-RPC batching is only supported in sessions negotiated at protocol 2025-03-26",
            ),
        );
    };
    let era = Era::Legacy(session.version);
    let mut out = Vec::new();
    let mut futs = Vec::new();
    for item in items {
        match item {
            Err(e) => out.push(e.to_value()),
            Ok(Message::Request(r)) if r.method == "initialize" => out.push(
                jsonrpc::Response::error(
                    Some(r.id),
                    ErrorObject::invalid_request("Invalid Request: initialize must not be batched"),
                )
                .to_value(),
            ),
            Ok(Message::Request(r)) => futs.push(st.server.dispatch(r, era, None)),
            Ok(Message::Notification(n)) => session_notification(&session, &n),
            Ok(Message::Response(_)) => {}
        }
    }
    out.extend(
        futures_util::future::join_all(futs)
            .await
            .iter()
            .map(jsonrpc::Response::to_value),
    );
    if out.is_empty() {
        StatusCode::ACCEPTED.into_response()
    } else {
        json_response(StatusCode::OK, Value::Array(out))
    }
}

// ---------- GET / DELETE（旧版会话） ----------

fn get(st: &Shared, headers: &HeaderMap) -> Response {
    if headers.get(SESSION_HEADER).is_none() {
        return method_not_allowed();
    }
    let session = match lookup_session(st, headers) {
        Ok(Some(s)) => s,
        Ok(None) => return method_not_allowed(),
        Err(r) => return *r,
    };
    if !parse_accept(headers).sse || headers.get(header::ACCEPT).is_none() {
        return (
            StatusCode::NOT_ACCEPTABLE,
            "Accept must include text/event-stream",
        )
            .into_response();
    }
    // 本服务器没有主动推送的消息；流保持打开直到会话结束，只发保活注释。
    let mut closed = session.closed.subscribe();
    let stream = NoEvents(Box::pin(futures_util::stream::once(async move {
        let _ = closed.wait_for(|c| *c).await;
    })));
    let mut resp = Sse::new(stream)
        .keep_alive(KeepAlive::new().interval(st.config.sse_keep_alive))
        .into_response();
    resp.headers_mut()
        .insert("x-accel-buffering", HeaderValue::from_static("no"));
    resp
}

fn delete(st: &Shared, headers: &HeaderMap) -> Response {
    let Some(sid) = header_str(headers, SESSION_HEADER) else {
        return method_not_allowed();
    };
    match lock(&st.sessions).remove(sid) {
        Some(s) => {
            s.close();
            StatusCode::NO_CONTENT.into_response()
        }
        None => StatusCode::NOT_FOUND.into_response(),
    }
}

/// 不产生事件的 SSE 流，内部流结束（会话关闭）时结束。
struct NoEvents<S>(S);

impl<S: Stream<Item = ()> + Unpin> Stream for NoEvents<S> {
    type Item = Result<Event, Infallible>;

    fn poll_next(mut self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<Option<Self::Item>> {
        match Pin::new(&mut self.0).poll_next(cx) {
            Poll::Ready(_) => Poll::Ready(None),
            Poll::Pending => Poll::Pending,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn host_parsing() {
        assert_eq!(host_without_port("localhost:3003"), "localhost");
        assert_eq!(host_without_port("[::1]:3003"), "[::1]");
        assert_eq!(host_without_port("[::1]"), "[::1]");
        assert_eq!(host_without_port("example.com"), "example.com");
    }

    #[test]
    fn origin_rules() {
        let cfg = HttpConfig {
            allowed_origins: vec!["https://app.example.com".into()],
            ..HttpConfig::default()
        };
        for ok in [
            "http://localhost:6274",
            "http://127.0.0.1",
            "https://[::1]:8443",
            "https://app.example.com",
        ] {
            assert!(origin_allowed(&cfg, ok), "{ok}");
        }
        for bad in [
            "null",
            "http://evil.com",
            "http://localhost.evil.com",
            "http://127.0.0.1.nip.io",
            "http://evil.com@localhost",
            "file://",
            "https://app.example.com.evil",
        ] {
            assert!(!origin_allowed(&cfg, bad), "{bad}");
        }
    }

    #[test]
    fn accept_parsing() {
        let mut h = HeaderMap::new();
        let a = parse_accept(&h);
        assert!(a.json && !a.sse);
        h.insert(
            header::ACCEPT,
            HeaderValue::from_static("application/json, text/event-stream"),
        );
        let a = parse_accept(&h);
        assert!(a.json && a.sse);
        h.insert(header::ACCEPT, HeaderValue::from_static("text/html"));
        let a = parse_accept(&h);
        assert!(!a.json && !a.sse);
    }

    #[test]
    fn header_value_decoding() {
        assert_eq!(decode_header_value("plain").as_deref(), Some("plain"));
        assert_eq!(
            decode_header_value("=?base64?SGVsbG8sIOS4lueVjA==?=").as_deref(),
            Some("Hello, 世界")
        );
        assert_eq!(decode_header_value("=?base64?!!!?="), None);
    }

    #[test]
    fn session_ids_are_random_hex() {
        let a = new_session_id().unwrap();
        let b = new_session_id().unwrap();
        assert_eq!(a.len(), 32);
        assert_ne!(a, b);
        assert!(a.bytes().all(|c| c.is_ascii_hexdigit()));
    }
}
