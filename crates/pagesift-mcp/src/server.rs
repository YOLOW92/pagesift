//! 和传输无关的 MCP 服务器：版本判定、握手、方法分发。

use std::collections::HashMap;
use std::fmt;
use std::panic::AssertUnwindSafe;
use std::sync::Arc;

use futures_util::FutureExt as _;
use serde_json::{Map, Value, json};
use tokio::sync::mpsc::UnboundedSender;

use crate::jsonrpc::{self, ErrorObject, Request, Response};
use crate::protocol::{self, Era};
use crate::schema;
use crate::tool::{ToolContext, ToolDef, ToolHandler, ToolOutput};

/// `tools/list` 结果的缓存时长：工具集合在进程生命周期内不变。
const TOOLS_TTL_MS: u64 = 3_600_000;

/// MCP 服务器。克隆开销很小，各传输共享同一个实例。
#[derive(Clone)]
pub struct McpServer {
    inner: Arc<Inner>,
}

struct Inner {
    name: String,
    version: String,
    title: Option<String>,
    instructions: Option<String>,
    tools: Vec<(ToolDef, Arc<dyn ToolHandler>)>,
    index: HashMap<String, usize>,
    validate_arguments: bool,
}

impl fmt::Debug for McpServer {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("McpServer")
            .field("name", &self.inner.name)
            .field("tools", &self.tool_names())
            .finish()
    }
}

/// [`McpServer`] 的构建器。
pub struct ServerBuilder {
    name: String,
    version: String,
    title: Option<String>,
    instructions: Option<String>,
    tools: Vec<Arc<dyn ToolHandler>>,
    validate_arguments: bool,
}

/// 构建服务器时发现的问题。
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum BuildError {
    DuplicateTool(String),
    InvalidToolName(String),
    InvalidInputSchema(String),
}

impl fmt::Display for BuildError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            BuildError::DuplicateTool(n) => write!(f, "duplicate tool name: {n}"),
            BuildError::InvalidToolName(n) => {
                write!(f, "invalid tool name {n:?}: use 1-128 of [A-Za-z0-9_.-]")
            }
            BuildError::InvalidInputSchema(n) => {
                write!(
                    f,
                    "tool {n}: inputSchema must be an object schema with \"type\": \"object\""
                )
            }
        }
    }
}

impl std::error::Error for BuildError {}

impl ServerBuilder {
    pub fn title(mut self, title: impl Into<String>) -> Self {
        self.title = Some(title.into());
        self
    }

    /// 给模型看的使用说明（`instructions`）。
    pub fn instructions(mut self, instructions: impl Into<String>) -> Self {
        self.instructions = Some(instructions.into());
        self
    }

    pub fn tool(self, handler: impl ToolHandler) -> Self {
        self.tool_arc(Arc::new(handler))
    }

    pub fn tool_arc(mut self, handler: Arc<dyn ToolHandler>) -> Self {
        self.tools.push(handler);
        self
    }

    pub fn tools(mut self, handlers: impl IntoIterator<Item = Arc<dyn ToolHandler>>) -> Self {
        self.tools.extend(handlers);
        self
    }

    /// 调用前是否按 `inputSchema` 校验参数（默认开启）。校验失败返回 `isError` 结果。
    pub fn validate_arguments(mut self, on: bool) -> Self {
        self.validate_arguments = on;
        self
    }

    pub fn build(self) -> Result<McpServer, BuildError> {
        let mut tools = Vec::with_capacity(self.tools.len());
        let mut index = HashMap::new();
        for handler in self.tools {
            let def = handler.definition();
            if !valid_tool_name(&def.name) {
                return Err(BuildError::InvalidToolName(def.name));
            }
            if def.input_schema.get("type").and_then(Value::as_str) != Some("object") {
                return Err(BuildError::InvalidInputSchema(def.name));
            }
            if index.insert(def.name.clone(), tools.len()).is_some() {
                return Err(BuildError::DuplicateTool(def.name));
            }
            tools.push((def, handler));
        }
        Ok(McpServer {
            inner: Arc::new(Inner {
                name: self.name,
                version: self.version,
                title: self.title,
                instructions: self.instructions,
                tools,
                index,
                validate_arguments: self.validate_arguments,
            }),
        })
    }
}

fn valid_tool_name(n: &str) -> bool {
    (1..=128).contains(&n.len())
        && n.bytes()
            .all(|b| b.is_ascii_alphanumeric() || matches!(b, b'_' | b'-' | b'.'))
}

impl McpServer {
    /// `name` 和 `version` 出现在 `serverInfo` 里。
    pub fn builder(name: impl Into<String>, version: impl Into<String>) -> ServerBuilder {
        ServerBuilder {
            name: name.into(),
            version: version.into(),
            title: None,
            instructions: None,
            tools: Vec::new(),
            validate_arguments: true,
        }
    }

    pub fn tool_names(&self) -> Vec<&str> {
        self.inner
            .tools
            .iter()
            .map(|(d, _)| d.name.as_str())
            .collect()
    }

    fn server_info(&self) -> Value {
        let mut m = Map::new();
        m.insert("name".into(), json!(self.inner.name));
        m.insert("version".into(), json!(self.inner.version));
        if let Some(t) = &self.inner.title {
            m.insert("title".into(), json!(t));
        }
        Value::Object(m)
    }

    /// 判断请求属于哪一代协议。`legacy` 是本连接（或会话）通过 `initialize` 协商出的版本。
    pub(crate) fn classify(
        &self,
        req: &Request,
        legacy: Option<&'static str>,
    ) -> Result<Era, ErrorObject> {
        if let Some(requested) = modern_version_of(req) {
            let requested = requested.ok_or_else(|| {
                ErrorObject::invalid_params(format!(
                    "_meta[\"{}\"] must be a string",
                    protocol::META_PROTOCOL_VERSION
                ))
            })?;
            let Some(v) = protocol::find_version(protocol::MODERN_VERSIONS, requested) else {
                return Err(unsupported_version(requested));
            };
            let meta = meta_of(req);
            match meta.and_then(|m| m.get(protocol::META_CLIENT_CAPABILITIES)) {
                Some(Value::Object(_)) => {}
                Some(_) => {
                    return Err(ErrorObject::invalid_params(format!(
                        "_meta[\"{}\"] must be an object",
                        protocol::META_CLIENT_CAPABILITIES
                    )));
                }
                None => {
                    return Err(ErrorObject::invalid_params(format!(
                        "missing required _meta[\"{}\"]",
                        protocol::META_CLIENT_CAPABILITIES
                    )));
                }
            }
            return Ok(Era::Modern(v));
        }
        if let Some(v) = legacy {
            return Ok(Era::Legacy(v));
        }
        // 旧版客户端在握手前也可以 ping。
        if req.method == "ping" {
            return Ok(Era::Legacy(protocol::LEGACY_VERSIONS[0]));
        }
        Err(ErrorObject::invalid_params(format!(
            "missing required _meta[\"{}\"]; clients on protocol {} or earlier must send \"initialize\" first",
            protocol::META_PROTOCOL_VERSION,
            protocol::LEGACY_VERSIONS[0]
        ))
        .with_data(json!({ "supported": protocol::supported_versions() })))
    }

    /// 处理旧版 `initialize`，返回协商出的版本和结果。
    pub(crate) fn initialize(&self, req: &Request) -> Result<(&'static str, Value), ErrorObject> {
        let params = jsonrpc::params_object(req.params.as_ref())?;
        let Some(Value::String(requested)) = params.get("protocolVersion") else {
            return Err(ErrorObject::invalid_params(
                "initialize: \"protocolVersion\" must be a string",
            ));
        };
        if !params.get("capabilities").is_some_and(Value::is_object) {
            return Err(ErrorObject::invalid_params(
                "initialize: \"capabilities\" must be an object",
            ));
        }
        match params.get("clientInfo") {
            Some(Value::Object(ci)) if ci.get("name").is_some_and(Value::is_string) => {}
            _ => {
                return Err(ErrorObject::invalid_params(
                    "initialize: \"clientInfo\" must be an object with a string \"name\"",
                ));
            }
        }
        // 支持就原样返回，否则给出支持的最新旧版，由客户端决定是否断开。
        let version = protocol::find_version(protocol::LEGACY_VERSIONS, requested)
            .unwrap_or(protocol::LEGACY_VERSIONS[0]);
        let mut result = Map::new();
        result.insert("protocolVersion".into(), json!(version));
        result.insert(
            "capabilities".into(),
            json!({ "tools": { "listChanged": false } }),
        );
        result.insert("serverInfo".into(), self.server_info());
        if let Some(i) = &self.inner.instructions {
            result.insert("instructions".into(), json!(i));
        }
        Ok((version, Value::Object(result)))
    }

    /// 该方法在这一代协议里是否存在（HTTP 传输据此返回 404）。
    pub(crate) fn has_method(era: Era, method: &str) -> bool {
        match era {
            Era::Modern(_) => matches!(method, "server/discover" | "tools/list" | "tools/call"),
            Era::Legacy(_) => matches!(method, "ping" | "tools/list" | "tools/call"),
        }
    }

    /// 处理一个已判定代际的请求。`notify` 用于发送和该请求相关的通知（进度）。
    pub(crate) async fn dispatch(
        &self,
        req: Request,
        era: Era,
        notify: Option<UnboundedSender<Value>>,
    ) -> Response {
        let id = req.id.clone();
        let outcome = match (era, req.method.as_str()) {
            (_, m) if !Self::has_method(era, m) => Err(ErrorObject::method_not_found(m)),
            (Era::Legacy(_), "ping") => Ok(json!({})),
            (Era::Modern(_), "server/discover") => Ok(self.discover()),
            (_, "tools/list") => self.list_tools(&req),
            (_, "tools/call") => self.call_tool(&req, notify).await,
            (_, m) => Err(ErrorObject::method_not_found(m)),
        };
        match outcome {
            Ok(result) => Response::result(id, self.finish_result(era, result)),
            Err(e) => Response::error(Some(id), e),
        }
    }

    fn finish_result(&self, era: Era, result: Value) -> Value {
        let Value::Object(mut m) = result else {
            return result;
        };
        if let Era::Legacy(_) = era {
            // 缓存提示是新版才有的字段。
            m.remove("ttlMs");
            m.remove("cacheScope");
            return Value::Object(m);
        }
        m.insert("resultType".into(), json!("complete"));
        let meta = m
            .entry("_meta")
            .or_insert_with(|| Value::Object(Map::new()));
        if let Value::Object(meta) = meta {
            meta.insert(protocol::META_SERVER_INFO.into(), self.server_info());
        }
        Value::Object(m)
    }

    fn discover(&self) -> Value {
        let mut m = Map::new();
        m.insert(
            "supportedVersions".into(),
            json!(protocol::supported_versions()),
        );
        m.insert("capabilities".into(), json!({ "tools": {} }));
        if let Some(i) = &self.inner.instructions {
            m.insert("instructions".into(), json!(i));
        }
        m.insert("ttlMs".into(), json!(TOOLS_TTL_MS));
        m.insert("cacheScope".into(), json!("public"));
        Value::Object(m)
    }

    fn list_tools(&self, req: &Request) -> Result<Value, ErrorObject> {
        let params = jsonrpc::params_object(req.params.as_ref())?;
        // 工具全部放在一页里，任何游标都是无效的。
        if let Some(c) = params.get("cursor")
            && !c.is_null()
        {
            return Err(ErrorObject::invalid_params("invalid cursor"));
        }
        let tools: Vec<Value> = self.inner.tools.iter().map(|(d, _)| d.to_json()).collect();
        Ok(json!({ "tools": tools, "ttlMs": TOOLS_TTL_MS, "cacheScope": "public" }))
    }

    async fn call_tool(
        &self,
        req: &Request,
        notify: Option<UnboundedSender<Value>>,
    ) -> Result<Value, ErrorObject> {
        let params = jsonrpc::params_object(req.params.as_ref())?;
        let Some(Value::String(name)) = params.get("name") else {
            return Err(ErrorObject::invalid_params(
                "tools/call: \"name\" must be a string",
            ));
        };
        let Some(&i) = self.inner.index.get(name) else {
            return Err(ErrorObject::invalid_params(format!("Unknown tool: {name}")));
        };
        let args = match params.get("arguments") {
            None | Some(Value::Null) => Map::new(),
            Some(Value::Object(a)) => a.clone(),
            Some(_) => {
                return Err(ErrorObject::invalid_params(
                    "tools/call: \"arguments\" must be an object",
                ));
            }
        };
        let (def, handler) = &self.inner.tools[i];
        if self.inner.validate_arguments
            && let Err(why) = schema::validate(&def.input_schema, &Value::Object(args.clone()))
        {
            return Ok(ToolOutput::error(format!("Invalid arguments for {name}: {why}")).to_json());
        }

        let meta = params.get("_meta").and_then(Value::as_object);
        let ctx = ToolContext {
            progress_token: meta
                .and_then(|m| m.get("progressToken"))
                .filter(|t| t.is_string() || t.is_i64() || t.is_u64())
                .cloned(),
            notify,
            client_info: meta
                .and_then(|m| m.get(protocol::META_CLIENT_INFO))
                .cloned(),
        };
        let output = match AssertUnwindSafe(handler.call(args, ctx))
            .catch_unwind()
            .await
        {
            Ok(Ok(out)) => out,
            Ok(Err(e)) => e.into_output(),
            Err(_) => {
                tracing::error!(tool = %name, "tool handler panicked");
                ToolOutput::error(format!("Internal error while running {name}"))
            }
        };
        Ok(output.to_json())
    }
}

fn meta_of(req: &Request) -> Option<&Map<String, Value>> {
    req.params.as_ref()?.get("_meta")?.as_object()
}

/// `Some(Some(v))`：带了新版版本号；`Some(None)`：字段存在但不是字符串；`None`：没带。
fn modern_version_of(req: &Request) -> Option<Option<&str>> {
    meta_of(req)?
        .get(protocol::META_PROTOCOL_VERSION)
        .map(Value::as_str)
}

pub(crate) fn unsupported_version(requested: &str) -> ErrorObject {
    ErrorObject::new(
        protocol::UNSUPPORTED_PROTOCOL_VERSION,
        "Unsupported protocol version",
    )
    .with_data(json!({ "supported": protocol::supported_versions(), "requested": requested }))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::jsonrpc::Id;
    use crate::tool::{BoxFuture, ToolError, ToolResult};

    struct Echo;
    impl ToolHandler for Echo {
        fn definition(&self) -> ToolDef {
            ToolDef::new(
                "echo",
                json!({"type": "object", "properties": {"text": {"type": "string"}}, "required": ["text"]}),
            )
        }
        fn call(&self, args: Map<String, Value>, ctx: ToolContext) -> BoxFuture<'_, ToolResult> {
            Box::pin(async move {
                ctx.progress(1.0, Some(1.0), None);
                match args["text"].as_str() {
                    Some("fail") => Err(ToolError::new("asked to fail")),
                    Some("panic") => panic!("boom"),
                    Some(t) => Ok(ToolOutput::text(t)),
                    None => unreachable!(),
                }
            })
        }
    }

    fn server() -> McpServer {
        McpServer::builder("t", "1").tool(Echo).build().unwrap()
    }

    fn req(method: &str, params: Value) -> Request {
        Request {
            id: Id::Int(1),
            method: method.into(),
            params: Some(params),
        }
    }

    fn modern(mut params: Value) -> Value {
        params["_meta"] = json!({
            "io.modelcontextprotocol/protocolVersion": "2026-07-28",
            "io.modelcontextprotocol/clientCapabilities": {},
        });
        params
    }

    #[test]
    fn builder_rejects_bad_tools() {
        let e = McpServer::builder("t", "1")
            .tool(Echo)
            .tool(Echo)
            .build()
            .unwrap_err();
        assert_eq!(e, BuildError::DuplicateTool("echo".into()));
        assert!(valid_tool_name("pagesift_scrape"));
        assert!(!valid_tool_name("has space"));
        assert!(!valid_tool_name(""));
    }

    #[test]
    fn classify_eras() {
        let s = server();
        assert_eq!(
            s.classify(&req("tools/list", modern(json!({}))), None),
            Ok(Era::Modern("2026-07-28"))
        );
        let mut p = modern(json!({}));
        p["_meta"]["io.modelcontextprotocol/protocolVersion"] = json!("1900-01-01");
        let e = s.classify(&req("tools/list", p), None).unwrap_err();
        assert_eq!(e.code, protocol::UNSUPPORTED_PROTOCOL_VERSION);
        assert_eq!(e.data.unwrap()["requested"], "1900-01-01");
        let p = json!({"_meta": {"io.modelcontextprotocol/protocolVersion": "2026-07-28"}});
        assert_eq!(
            s.classify(&req("tools/list", p), None).unwrap_err().code,
            jsonrpc::INVALID_PARAMS
        );
        assert_eq!(
            s.classify(&req("tools/list", json!({})), None)
                .unwrap_err()
                .code,
            jsonrpc::INVALID_PARAMS
        );
        assert_eq!(
            s.classify(&req("tools/list", json!({})), Some("2025-06-18")),
            Ok(Era::Legacy("2025-06-18"))
        );
        assert!(s.classify(&req("ping", json!({})), None).is_ok());
    }

    #[test]
    fn initialize_negotiates() {
        let s = server();
        let p = |v: &str| json!({"protocolVersion": v, "capabilities": {}, "clientInfo": {"name": "c", "version": "1"}});
        assert_eq!(
            s.initialize(&req("initialize", p("2025-06-18"))).unwrap().0,
            "2025-06-18"
        );
        assert_eq!(
            s.initialize(&req("initialize", p("2024-11-05"))).unwrap().0,
            "2024-11-05"
        );
        let (v, r) = s.initialize(&req("initialize", p("2099-01-01"))).unwrap();
        assert_eq!(v, "2025-11-25");
        assert_eq!(r["protocolVersion"], "2025-11-25");
        assert_eq!(r["serverInfo"]["name"], "t");
        assert!(r["capabilities"]["tools"].is_object());
        assert!(
            s.initialize(&req("initialize", json!({"protocolVersion": "2025-06-18"})))
                .is_err()
        );
        assert!(s.initialize(&req("initialize", json!([]))).is_err());
    }

    #[tokio::test]
    async fn dispatch_modern_and_legacy() {
        let s = server();
        let m = Era::Modern("2026-07-28");
        let l = Era::Legacy("2025-11-25");

        let r = s
            .dispatch(req("server/discover", modern(json!({}))), m, None)
            .await;
        let v = r.payload.unwrap();
        assert_eq!(v["resultType"], "complete");
        assert_eq!(
            v["_meta"]["io.modelcontextprotocol/serverInfo"]["name"],
            "t"
        );
        assert!(
            v["supportedVersions"]
                .as_array()
                .unwrap()
                .contains(&json!("2025-06-18"))
        );

        let v = s
            .dispatch(req("tools/list", json!({})), l, None)
            .await
            .payload
            .unwrap();
        assert_eq!(v["tools"][0]["name"], "echo");
        assert!(v.get("resultType").is_none());

        let r = s.dispatch(req("ping", json!({})), m, None).await;
        assert_eq!(r.payload.unwrap_err().code, jsonrpc::METHOD_NOT_FOUND);
        let r = s.dispatch(req("server/discover", json!({})), l, None).await;
        assert_eq!(r.payload.unwrap_err().code, jsonrpc::METHOD_NOT_FOUND);
        let r = s
            .dispatch(req("tools/list", json!({"cursor": "x"})), l, None)
            .await;
        assert_eq!(r.payload.unwrap_err().code, jsonrpc::INVALID_PARAMS);
    }

    #[tokio::test]
    async fn tool_call_outcomes() {
        let s = server();
        let l = Era::Legacy("2025-11-25");
        let call = |args: Value| req("tools/call", json!({"name": "echo", "arguments": args}));

        let v = s
            .dispatch(call(json!({"text": "hi"})), l, None)
            .await
            .payload
            .unwrap();
        assert_eq!(v["isError"], false);
        assert_eq!(v["content"][0]["text"], "hi");

        for (args, needle) in [
            (json!({"text": "fail"}), "asked to fail"),
            (json!({"text": "panic"}), "Internal error"),
            (json!({}), "missing required property"),
            (json!({"text": 3}), "expected string"),
        ] {
            let v = s.dispatch(call(args), l, None).await.payload.unwrap();
            assert_eq!(v["isError"], true);
            assert!(
                v["content"][0]["text"].as_str().unwrap().contains(needle),
                "{v}"
            );
        }

        let r = s
            .dispatch(req("tools/call", json!({"name": "nope"})), l, None)
            .await;
        assert_eq!(r.payload.unwrap_err().code, jsonrpc::INVALID_PARAMS);
        let r = s
            .dispatch(
                req("tools/call", json!({"name": "echo", "arguments": [1]})),
                l,
                None,
            )
            .await;
        assert_eq!(r.payload.unwrap_err().code, jsonrpc::INVALID_PARAMS);
        let r = s.dispatch(req("tools/call", json!({})), l, None).await;
        assert_eq!(r.payload.unwrap_err().code, jsonrpc::INVALID_PARAMS);
    }

    #[tokio::test]
    async fn progress_notifications() {
        let s = server();
        let (tx, mut rx) = tokio::sync::mpsc::unbounded_channel();
        let p =
            json!({"name": "echo", "arguments": {"text": "x"}, "_meta": {"progressToken": "tok"}});
        s.dispatch(
            req("tools/call", p),
            Era::Legacy("2025-11-25"),
            Some(tx.clone()),
        )
        .await;
        let n = rx.try_recv().unwrap();
        assert_eq!(n["method"], "notifications/progress");
        assert_eq!(n["params"]["progressToken"], "tok");
        // 没有 progressToken 就不发。
        let p = json!({"name": "echo", "arguments": {"text": "x"}});
        s.dispatch(req("tools/call", p), Era::Legacy("2025-11-25"), Some(tx))
            .await;
        assert!(rx.try_recv().is_err());
    }
}
