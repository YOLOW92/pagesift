//! CDP 协议层：命令 id 与响应的对应、事件分发、flatten 模式的 session。

use std::collections::HashMap;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex};

use futures_util::{SinkExt, StreamExt};
use serde_json::{Value, json};
use tokio::sync::{mpsc, oneshot};
use tokio_tungstenite::tungstenite::Message;
use tokio_tungstenite::tungstenite::protocol::WebSocketConfig;

use crate::error::{Error, Result};

/// 截图和 PDF 的消息可能很大，放宽 tungstenite 的默认上限。
const MAX_MESSAGE: usize = 512 << 20;

#[derive(Debug, Clone)]
pub struct Event {
    pub method: String,
    pub params: Value,
    pub session_id: Option<String>,
}

type Reply = Result<Value>;

struct Subscriber {
    /// `None` 表示接收所有事件。
    session_id: Option<String>,
    tx: mpsc::UnboundedSender<Event>,
}

#[derive(Default)]
struct State {
    pending: HashMap<u64, (String, oneshot::Sender<Reply>)>,
    subscribers: Vec<Subscriber>,
    closed: bool,
}

struct Inner {
    out: mpsc::UnboundedSender<Message>,
    next_id: AtomicU64,
    state: Mutex<State>,
}

/// 一条到浏览器的 CDP WebSocket 连接。克隆开销很小，共享同一条连接。
#[derive(Clone)]
pub struct Connection {
    inner: Arc<Inner>,
}

impl Connection {
    pub async fn connect(ws_url: &str) -> Result<Connection> {
        let config = WebSocketConfig::default()
            .max_message_size(Some(MAX_MESSAGE))
            .max_frame_size(Some(MAX_MESSAGE));
        let (ws, _) = tokio_tungstenite::connect_async_with_config(ws_url, Some(config), true)
            .await
            .map_err(|e| Error::Connect(e.to_string()))?;
        let (mut sink, mut stream) = ws.split();
        let (out, mut out_rx) = mpsc::unbounded_channel::<Message>();
        let inner = Arc::new(Inner {
            out,
            next_id: AtomicU64::new(1),
            state: Mutex::new(State::default()),
        });

        tokio::spawn(async move {
            while let Some(msg) = out_rx.recv().await {
                if sink.send(msg).await.is_err() {
                    break;
                }
            }
            let _ = sink.close().await;
        });

        let reader = Arc::downgrade(&inner);
        tokio::spawn(async move {
            while let Some(msg) = stream.next().await {
                let Some(inner) = reader.upgrade() else { break };
                match msg {
                    Ok(Message::Text(text)) => inner.dispatch(text.as_str()),
                    Ok(Message::Binary(bin)) => {
                        if let Ok(text) = std::str::from_utf8(&bin) {
                            inner.dispatch(text);
                        }
                    }
                    Ok(Message::Close(_)) | Err(_) => break,
                    Ok(_) => {}
                }
            }
            if let Some(inner) = reader.upgrade() {
                inner.close();
            }
        });

        Ok(Connection { inner })
    }

    pub fn is_closed(&self) -> bool {
        self.inner.state.lock().unwrap().closed
    }

    /// 发送命令并等待对应 id 的响应。`session_id` 为 `None` 时是浏览器级命令。
    pub async fn call(
        &self,
        method: &str,
        params: Value,
        session_id: Option<&str>,
    ) -> Result<Value> {
        let id = self.inner.next_id.fetch_add(1, Ordering::Relaxed);
        let (tx, rx) = oneshot::channel();
        {
            let mut st = self.inner.state.lock().unwrap();
            if st.closed {
                return Err(Error::ConnectionClosed);
            }
            st.pending.insert(id, (method.to_owned(), tx));
        }
        // 调用方中途放弃（超时）时把挂起项清掉。
        let _guard = PendingGuard {
            inner: &self.inner,
            id,
        };
        let mut msg = json!({ "id": id, "method": method, "params": params });
        if let Some(sid) = session_id {
            msg["sessionId"] = Value::String(sid.to_owned());
        }
        if self.inner.out.send(Message::text(msg.to_string())).is_err() {
            return Err(Error::ConnectionClosed);
        }
        rx.await.map_err(|_| Error::ConnectionClosed)?
    }

    /// 订阅事件。`session_id` 为 `None` 时接收这条连接上的全部事件。
    pub fn subscribe(&self, session_id: Option<&str>) -> EventStream {
        let (tx, rx) = mpsc::unbounded_channel();
        let mut st = self.inner.state.lock().unwrap();
        if !st.closed {
            st.subscribers.push(Subscriber {
                session_id: session_id.map(str::to_owned),
                tx,
            });
        }
        EventStream { rx }
    }

    pub fn session(&self, session_id: impl Into<String>) -> Session {
        Session {
            conn: self.clone(),
            id: session_id.into(),
        }
    }

    /// `Target.attachToTarget`（flatten 模式），返回该 target 的 session。
    pub async fn attach(&self, target_id: &str) -> Result<Session> {
        let r = self
            .call(
                "Target.attachToTarget",
                json!({ "targetId": target_id, "flatten": true }),
                None,
            )
            .await?;
        let sid = str_field(&r, "sessionId")?;
        Ok(self.session(sid))
    }

    pub async fn create_target(
        &self,
        url: &str,
        browser_context_id: Option<&str>,
    ) -> Result<String> {
        let mut params = json!({ "url": url });
        if let Some(ctx) = browser_context_id {
            params["browserContextId"] = Value::String(ctx.to_owned());
        }
        let r = self.call("Target.createTarget", params, None).await?;
        str_field(&r, "targetId")
    }

    pub async fn close_target(&self, target_id: &str) -> Result<()> {
        self.call("Target.closeTarget", json!({ "targetId": target_id }), None)
            .await
            .map(drop)
    }

    pub async fn create_browser_context(&self) -> Result<String> {
        let r = self
            .call(
                "Target.createBrowserContext",
                json!({ "disposeOnDetach": true }),
                None,
            )
            .await?;
        str_field(&r, "browserContextId")
    }

    pub async fn dispose_browser_context(&self, id: &str) -> Result<()> {
        self.call(
            "Target.disposeBrowserContext",
            json!({ "browserContextId": id }),
            None,
        )
        .await
        .map(drop)
    }
}

impl Inner {
    fn dispatch(&self, text: &str) {
        let Ok(mut v) = serde_json::from_str::<Value>(text) else {
            tracing::debug!("ignoring malformed CDP message");
            return;
        };
        if let Some(id) = v.get("id").and_then(Value::as_u64) {
            let Some((method, tx)) = self.state.lock().unwrap().pending.remove(&id) else {
                return;
            };
            let reply = match v.get_mut("error") {
                Some(err) => Err(Error::Protocol {
                    method,
                    code: err.get("code").and_then(Value::as_i64).unwrap_or(0),
                    message: err
                        .get("message")
                        .and_then(Value::as_str)
                        .unwrap_or_default()
                        .to_owned(),
                }),
                None => Ok(v.get_mut("result").map(Value::take).unwrap_or(Value::Null)),
            };
            let _ = tx.send(reply);
            return;
        }
        let Some(method) = v.get("method").and_then(Value::as_str) else {
            return;
        };
        let event = Event {
            method: method.to_owned(),
            session_id: v
                .get("sessionId")
                .and_then(Value::as_str)
                .map(str::to_owned),
            params: v.get_mut("params").map(Value::take).unwrap_or(Value::Null),
        };
        let mut st = self.state.lock().unwrap();
        st.subscribers.retain(|s| {
            if s.session_id.is_some() && s.session_id != event.session_id {
                return !s.tx.is_closed();
            }
            s.tx.send(event.clone()).is_ok()
        });
    }

    fn close(&self) {
        let mut st = self.state.lock().unwrap();
        st.closed = true;
        for (_, (_, tx)) in st.pending.drain() {
            let _ = tx.send(Err(Error::ConnectionClosed));
        }
        st.subscribers.clear();
    }
}

struct PendingGuard<'a> {
    inner: &'a Inner,
    id: u64,
}

impl Drop for PendingGuard<'_> {
    fn drop(&mut self) {
        self.inner.state.lock().unwrap().pending.remove(&self.id);
    }
}

pub struct EventStream {
    rx: mpsc::UnboundedReceiver<Event>,
}

impl EventStream {
    /// 连接关闭后返回 `None`。
    pub async fn recv(&mut self) -> Option<Event> {
        self.rx.recv().await
    }
}

/// flatten 模式下挂在某个 target 上的 session。
#[derive(Clone)]
pub struct Session {
    conn: Connection,
    id: String,
}

impl Session {
    pub fn id(&self) -> &str {
        &self.id
    }

    pub fn connection(&self) -> &Connection {
        &self.conn
    }

    pub async fn call(&self, method: &str, params: Value) -> Result<Value> {
        self.conn.call(method, params, Some(&self.id)).await
    }

    pub fn events(&self) -> EventStream {
        self.conn.subscribe(Some(&self.id))
    }
}

pub(crate) fn str_field(v: &Value, key: &str) -> Result<String> {
    v.get(key)
        .and_then(Value::as_str)
        .map(str::to_owned)
        .ok_or_else(|| Error::UnexpectedResponse(format!("missing `{key}`")))
}
