//! stdio 传输：一行一条 JSON-RPC 消息。stdout 只写协议消息，日志走 stderr（tracing）。

use std::collections::HashMap;
use std::io;
use std::sync::{Arc, Mutex};

use serde_json::Value;
use tokio::io::{AsyncBufReadExt, AsyncRead, AsyncWrite, AsyncWriteExt, BufReader};
use tokio::sync::mpsc;
use tokio::task::{AbortHandle, JoinSet};

use crate::jsonrpc::{self, ErrorObject, Id, Message, Notification, Parsed, Request, Response};
use crate::protocol::{self, Era};
use crate::server::McpServer;

/// 单行上限，超过的行按解析错误处理并丢弃。
pub const MAX_LINE_BYTES: usize = 16 * 1024 * 1024;

type Inflight = Arc<Mutex<HashMap<Id, AbortHandle>>>;

/// 在进程的 stdin/stdout 上运行，stdin 关闭后等进行中的请求结束再返回。
pub async fn serve_stdio(server: McpServer) -> io::Result<()> {
    serve_stream(server, tokio::io::stdin(), tokio::io::stdout()).await
}

/// 在任意字节流上运行 stdio 协议（Unix socket、TCP、测试用的管道）。
pub async fn serve_stream<R, W>(server: McpServer, reader: R, writer: W) -> io::Result<()>
where
    R: AsyncRead + Unpin,
    W: AsyncWrite + Unpin + Send + 'static,
{
    let (tx, rx) = mpsc::unbounded_channel::<Value>();
    let writer_task = tokio::spawn(write_loop(writer, rx));
    let mut conn = Connection {
        server,
        tx,
        legacy: None,
        client: None,
        inflight: Arc::default(),
        tasks: JoinSet::new(),
    };

    let mut reader = BufReader::new(reader);
    let mut line = Vec::new();
    loop {
        line.clear();
        match read_line_limited(&mut reader, &mut line, MAX_LINE_BYTES).await? {
            Line::Eof => break,
            Line::TooLong => conn.send(Response::error(
                None,
                ErrorObject::new(jsonrpc::PARSE_ERROR, "Parse error: message too large"),
            )),
            Line::Ok => {
                let text = line.trim_ascii();
                if !text.is_empty() {
                    conn.handle_line(text);
                }
            }
        }
        while conn.tasks.try_join_next().is_some() {}
    }

    while conn.tasks.join_next().await.is_some() {}
    drop(conn);
    writer_task.await.map_err(io::Error::other)?
}

struct Connection {
    server: McpServer,
    tx: mpsc::UnboundedSender<Value>,
    /// 通过 `initialize` 协商出的旧版协议版本。
    legacy: Option<&'static str>,
    /// `initialize` 里的 `clientInfo`：旧版之后的请求不再带它，工具上下文要用这一份。
    client: Option<Value>,
    inflight: Inflight,
    tasks: JoinSet<()>,
}

impl Connection {
    fn send(&self, r: Response) {
        let _ = self.tx.send(r.to_value());
    }

    fn handle_line(&mut self, text: &[u8]) {
        match jsonrpc::parse(text) {
            Err(e) => self.send(e),
            Ok(Parsed::Single(m)) => self.handle_message(m),
            Ok(Parsed::Batch(items)) => self.handle_batch(items),
        }
    }

    fn handle_message(&mut self, m: Message) {
        match m {
            Message::Request(r) => self.handle_request(r),
            Message::Notification(n) => self.handle_notification(n),
            Message::Response(_) => tracing::debug!("ignoring response from client"),
        }
    }

    fn handle_request(&mut self, req: Request) {
        if req.method == "initialize" {
            let resp = self.initialize(&req);
            return self.send(resp);
        }
        let era = match self.server.classify(&req, self.legacy) {
            Ok(era) => era,
            Err(e) => return self.send(Response::error(Some(req.id), e)),
        };
        let id = req.id.clone();
        let mut inflight = lock(&self.inflight);
        if inflight.contains_key(&id) {
            drop(inflight);
            return self.send(Response::error(
                Some(id),
                ErrorObject::invalid_request("Invalid Request: duplicate in-flight request id"),
            ));
        }
        let server = self.server.clone();
        let tx = self.tx.clone();
        let client = self.client.clone();
        let map = self.inflight.clone();
        let task_id = id.clone();
        let handle = self.tasks.spawn(async move {
            let resp = server.dispatch(req, era, Some(tx.clone()), client).await;
            // 已被取消的请求不会走到这里：任务在 abort 时直接被丢弃。
            lock(&map).remove(&task_id);
            let _ = tx.send(resp.to_value());
        });
        inflight.insert(id, handle);
    }

    fn initialize(&mut self, req: &Request) -> Response {
        if self.legacy.is_some() {
            return Response::error(
                Some(req.id.clone()),
                ErrorObject::invalid_request("Invalid Request: already initialized"),
            );
        }
        match self.server.initialize(req) {
            Ok((version, result)) => {
                self.legacy = Some(version);
                self.client = crate::server::initialize_client_info(req);
                Response::result(req.id.clone(), result)
            }
            Err(e) => Response::error(Some(req.id.clone()), e),
        }
    }

    fn handle_notification(&mut self, n: Notification) {
        match n.method.as_str() {
            "notifications/cancelled" => {
                let Some(id) = n
                    .params
                    .as_ref()
                    .and_then(|p| p.get("requestId"))
                    .and_then(Id::from_value)
                else {
                    return;
                };
                if let Some(h) = lock(&self.inflight).remove(&id) {
                    tracing::debug!(%id, "request cancelled by client");
                    h.abort();
                }
            }
            "notifications/initialized" => {}
            other => tracing::debug!(method = other, "ignoring notification"),
        }
    }

    /// 只有协商到 2025-03-26 的连接才接受批量；批量里不能有 `initialize`。
    fn handle_batch(&mut self, items: Vec<Result<Message, Response>>) {
        let Some(version) = self.legacy.filter(|v| protocol::BATCH_VERSIONS.contains(v)) else {
            return self.send(Response::error(
                None,
                ErrorObject::invalid_request(
                    "Invalid Request: JSON-RPC batching is not supported by the negotiated protocol version",
                ),
            ));
        };
        let mut immediate = Vec::new();
        let mut requests = Vec::new();
        for item in items {
            match item {
                Err(e) => immediate.push(e),
                Ok(Message::Request(r)) if r.method == "initialize" => {
                    immediate.push(Response::error(
                        Some(r.id),
                        ErrorObject::invalid_request(
                            "Invalid Request: initialize must not be batched",
                        ),
                    ))
                }
                Ok(Message::Request(r)) => requests.push(r),
                Ok(Message::Notification(n)) => self.handle_notification(n),
                Ok(Message::Response(_)) => {}
            }
        }
        if immediate.is_empty() && requests.is_empty() {
            return;
        }
        let server = self.server.clone();
        let tx = self.tx.clone();
        let client = self.client.clone();
        self.tasks.spawn(async move {
            let era = Era::Legacy(version);
            let futs = requests
                .into_iter()
                .map(|r| server.dispatch(r, era, Some(tx.clone()), client.clone()));
            let mut out: Vec<Value> = immediate.iter().map(Response::to_value).collect();
            out.extend(
                futures_util::future::join_all(futs)
                    .await
                    .iter()
                    .map(Response::to_value),
            );
            let _ = tx.send(Value::Array(out));
        });
    }
}

fn lock<T>(m: &Mutex<T>) -> std::sync::MutexGuard<'_, T> {
    m.lock().unwrap_or_else(|e| e.into_inner())
}

async fn write_loop<W: AsyncWrite + Unpin>(
    mut w: W,
    mut rx: mpsc::UnboundedReceiver<Value>,
) -> io::Result<()> {
    while let Some(v) = rx.recv().await {
        // 紧凑序列化不会产生换行（字符串里的换行会被转义）。
        let mut buf = serde_json::to_vec(&v).map_err(io::Error::other)?;
        buf.push(b'\n');
        w.write_all(&buf).await?;
        w.flush().await?;
    }
    Ok(())
}

enum Line {
    Ok,
    TooLong,
    Eof,
}

async fn read_line_limited<R: AsyncBufReadExt + Unpin>(
    r: &mut R,
    buf: &mut Vec<u8>,
    max: usize,
) -> io::Result<Line> {
    let mut too_long = false;
    loop {
        let chunk = r.fill_buf().await?;
        if chunk.is_empty() {
            return Ok(if too_long {
                Line::TooLong
            } else if buf.is_empty() {
                Line::Eof
            } else {
                Line::Ok
            });
        }
        let (part, done) = match chunk.iter().position(|&b| b == b'\n') {
            Some(i) => (&chunk[..i], Some(i + 1)),
            None => (chunk, None),
        };
        if !too_long {
            if buf.len() + part.len() > max {
                too_long = true;
                buf.clear();
            } else {
                buf.extend_from_slice(part);
            }
        }
        let n = done.unwrap_or(chunk.len());
        r.consume(n);
        if done.is_some() {
            return Ok(if too_long { Line::TooLong } else { Line::Ok });
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::tool::{BoxFuture, ToolContext, ToolDef, ToolHandler, ToolOutput, ToolResult};
    use serde_json::{Map, json};
    use tokio::io::{AsyncBufReadExt, AsyncWriteExt, duplex};

    struct Slow;
    impl ToolHandler for Slow {
        fn definition(&self) -> ToolDef {
            ToolDef::new("slow", json!({"type": "object"}))
        }
        fn call(&self, _: Map<String, Value>, _: ToolContext) -> BoxFuture<'_, ToolResult> {
            Box::pin(async {
                tokio::time::sleep(std::time::Duration::from_secs(30)).await;
                Ok(ToolOutput::text("done"))
            })
        }
    }

    struct Client {
        w: tokio::io::DuplexStream,
        r: tokio::io::Lines<BufReader<tokio::io::DuplexStream>>,
    }

    impl Client {
        async fn send(&mut self, s: &str) {
            self.w.write_all(s.as_bytes()).await.unwrap();
            self.w.write_all(b"\n").await.unwrap();
        }
        async fn recv(&mut self) -> Value {
            let line = tokio::time::timeout(std::time::Duration::from_secs(5), self.r.next_line())
                .await
                .unwrap()
                .unwrap()
                .unwrap();
            serde_json::from_str(&line).unwrap()
        }
    }

    fn start() -> (Client, tokio::task::JoinHandle<io::Result<()>>) {
        let server = McpServer::builder("t", "1").tool(Slow).build().unwrap();
        let (cw, sr) = duplex(1 << 16);
        let (sw, cr) = duplex(1 << 16);
        let h = tokio::spawn(serve_stream(server, sr, sw));
        (
            Client {
                w: cw,
                r: BufReader::new(cr).lines(),
            },
            h,
        )
    }

    const INIT: &str = r#"{"jsonrpc":"2.0","id":0,"method":"initialize","params":{"protocolVersion":"2025-03-26","capabilities":{},"clientInfo":{"name":"t","version":"1"}}}"#;

    #[tokio::test]
    async fn errors_do_not_kill_the_connection() {
        let (mut c, _h) = start();
        c.send("not json").await;
        assert_eq!(c.recv().await["error"]["code"], jsonrpc::PARSE_ERROR);
        c.send(r#"{"jsonrpc":"2.0","id":1,"method":"tools/list"}"#)
            .await;
        let v = c.recv().await;
        assert_eq!(v["id"], 1);
        assert_eq!(v["error"]["code"], jsonrpc::INVALID_PARAMS);
        c.send("[]").await;
        assert_eq!(c.recv().await["error"]["code"], jsonrpc::INVALID_REQUEST);
        c.send(r#"[{"jsonrpc":"2.0","id":1,"method":"ping"}]"#)
            .await;
        assert_eq!(c.recv().await["error"]["code"], jsonrpc::INVALID_REQUEST);
        c.send(r#"{"jsonrpc":"2.0","id":2,"method":"ping"}"#).await;
        assert_eq!(c.recv().await["result"], json!({}));
    }

    #[tokio::test]
    async fn cancellation_suppresses_response() {
        let (mut c, _h) = start();
        c.send(INIT).await;
        assert_eq!(c.recv().await["result"]["protocolVersion"], "2025-03-26");
        c.send(INIT).await;
        assert_eq!(c.recv().await["error"]["code"], jsonrpc::INVALID_REQUEST);
        c.send(r#"{"jsonrpc":"2.0","method":"notifications/initialized"}"#)
            .await;
        c.send(r#"{"jsonrpc":"2.0","id":"s","method":"tools/call","params":{"name":"slow"}}"#)
            .await;
        c.send(r#"{"jsonrpc":"2.0","id":"s","method":"tools/call","params":{"name":"slow"}}"#)
            .await;
        assert_eq!(c.recv().await["error"]["code"], jsonrpc::INVALID_REQUEST);
        c.send(r#"{"jsonrpc":"2.0","method":"notifications/cancelled","params":{"requestId":"s","reason":"test"}}"#).await;
        c.send(r#"{"jsonrpc":"2.0","id":3,"method":"ping"}"#).await;
        // 被取消的请求没有响应，下一条就是 ping 的结果。
        assert_eq!(c.recv().await["id"], 3);
    }

    #[tokio::test]
    async fn batch_on_2025_03_26() {
        let (mut c, _h) = start();
        c.send(INIT).await;
        c.recv().await;
        c.send(r#"[{"jsonrpc":"2.0","id":1,"method":"ping"},{"jsonrpc":"2.0","method":"notifications/initialized"},{"jsonrpc":"2.0","id":2,"method":"tools/list"},5]"#).await;
        let v = c.recv().await;
        let arr = v.as_array().unwrap();
        assert_eq!(arr.len(), 3);
        assert_eq!(arr[0]["error"]["code"], jsonrpc::INVALID_REQUEST);
        assert_eq!(arr[1]["id"], 1);
        assert_eq!(arr[2]["result"]["tools"][0]["name"], "slow");
    }

    #[tokio::test]
    async fn oversized_line_is_rejected() {
        let mut r = BufReader::new(&b"aaaaaaaaaa\n{}\n"[..]);
        let mut buf = Vec::new();
        assert!(matches!(
            read_line_limited(&mut r, &mut buf, 4).await.unwrap(),
            Line::TooLong
        ));
        buf.clear();
        assert!(matches!(
            read_line_limited(&mut r, &mut buf, 4).await.unwrap(),
            Line::Ok
        ));
        assert_eq!(buf, b"{}");
        buf.clear();
        assert!(matches!(
            read_line_limited(&mut r, &mut buf, 4).await.unwrap(),
            Line::Eof
        ));
    }
}
