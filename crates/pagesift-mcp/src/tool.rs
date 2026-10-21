//! 工具注册接口。

use std::fmt;
use std::future::Future;
use std::pin::Pin;

use serde_json::{Map, Value, json};
use tokio::sync::mpsc::UnboundedSender;

use crate::jsonrpc;

pub type BoxFuture<'a, T> = Pin<Box<dyn Future<Output = T> + Send + 'a>>;

/// 工具调用的结果。`Err` 会转换成 `isError: true` 的工具结果，而不是协议错误。
pub type ToolResult = Result<ToolOutput, ToolError>;

/// 一个 MCP 工具。
///
/// ```
/// use pagesift_mcp::{BoxFuture, ToolContext, ToolDef, ToolHandler, ToolOutput, ToolResult};
/// use serde_json::{Map, Value, json};
///
/// struct Echo;
///
/// impl ToolHandler for Echo {
///     fn definition(&self) -> ToolDef {
///         ToolDef::new("echo", json!({
///             "type": "object",
///             "properties": { "text": { "type": "string" } },
///             "required": ["text"]
///         }))
///         .description("Echo the text back.")
///     }
///
///     fn call(&self, args: Map<String, Value>, _ctx: ToolContext) -> BoxFuture<'_, ToolResult> {
///         Box::pin(async move { Ok(ToolOutput::text(args["text"].as_str().unwrap_or_default())) })
///     }
/// }
/// ```
pub trait ToolHandler: Send + Sync + 'static {
    /// 工具定义。服务器构建时调用一次并缓存。
    fn definition(&self) -> ToolDef;

    /// 执行工具。参数已经按 `inputSchema` 校验过（见 [`crate::ServerBuilder::validate_arguments`]）。
    /// 调用被取消时这个 future 会被直接丢弃。
    fn call(&self, args: Map<String, Value>, ctx: ToolContext) -> BoxFuture<'_, ToolResult>;
}

/// 工具定义，对应 MCP 的 `Tool`。
#[derive(Debug, Clone, PartialEq)]
pub struct ToolDef {
    pub name: String,
    pub title: Option<String>,
    pub description: Option<String>,
    /// JSON Schema（默认 2020-12），顶层必须是 `"type": "object"`。
    pub input_schema: Value,
    pub output_schema: Option<Value>,
    pub annotations: Option<ToolAnnotations>,
}

impl ToolDef {
    pub fn new(name: impl Into<String>, input_schema: Value) -> Self {
        Self {
            name: name.into(),
            title: None,
            description: None,
            input_schema,
            output_schema: None,
            annotations: None,
        }
    }

    pub fn title(mut self, title: impl Into<String>) -> Self {
        self.title = Some(title.into());
        self
    }

    pub fn description(mut self, description: impl Into<String>) -> Self {
        self.description = Some(description.into());
        self
    }

    pub fn output_schema(mut self, schema: Value) -> Self {
        self.output_schema = Some(schema);
        self
    }

    pub fn annotations(mut self, annotations: ToolAnnotations) -> Self {
        self.annotations = Some(annotations);
        self
    }

    pub fn to_json(&self) -> Value {
        let mut m = Map::new();
        m.insert("name".into(), json!(self.name));
        if let Some(t) = &self.title {
            m.insert("title".into(), json!(t));
        }
        if let Some(d) = &self.description {
            m.insert("description".into(), json!(d));
        }
        m.insert("inputSchema".into(), self.input_schema.clone());
        if let Some(s) = &self.output_schema {
            m.insert("outputSchema".into(), s.clone());
        }
        if let Some(a) = &self.annotations {
            m.insert("annotations".into(), a.to_json());
        }
        Value::Object(m)
    }
}

/// 工具行为提示（客户端视为不可信信息）。
#[derive(Debug, Clone, Default, PartialEq)]
pub struct ToolAnnotations {
    pub title: Option<String>,
    pub read_only_hint: Option<bool>,
    pub destructive_hint: Option<bool>,
    pub idempotent_hint: Option<bool>,
    pub open_world_hint: Option<bool>,
}

impl ToolAnnotations {
    fn to_json(&self) -> Value {
        let mut m = Map::new();
        let mut put = |k: &str, v: Option<Value>| {
            if let Some(v) = v {
                m.insert(k.into(), v);
            }
        };
        put("title", self.title.as_ref().map(|t| json!(t)));
        put("readOnlyHint", self.read_only_hint.map(Value::from));
        put("destructiveHint", self.destructive_hint.map(Value::from));
        put("idempotentHint", self.idempotent_hint.map(Value::from));
        put("openWorldHint", self.open_world_hint.map(Value::from));
        Value::Object(m)
    }
}

/// 结果里的一个内容块。
#[derive(Debug, Clone, PartialEq)]
pub enum Content {
    Text(String),
    /// base64 编码的图片。
    Image {
        data: String,
        mime_type: String,
    },
}

impl Content {
    fn to_json(&self) -> Value {
        match self {
            Content::Text(t) => json!({ "type": "text", "text": t }),
            Content::Image { data, mime_type } => {
                json!({ "type": "image", "data": data, "mimeType": mime_type })
            }
        }
    }
}

/// 工具的输出，对应 `CallToolResult`。
#[derive(Debug, Clone, PartialEq, Default)]
pub struct ToolOutput {
    pub content: Vec<Content>,
    pub structured_content: Option<Value>,
    pub is_error: bool,
}

impl ToolOutput {
    pub fn text(text: impl Into<String>) -> Self {
        Self {
            content: vec![Content::Text(text.into())],
            ..Self::default()
        }
    }

    /// 结构化结果：放进 `structuredContent`，同时附上序列化后的文本，兼容老客户端。
    pub fn json(value: Value) -> Self {
        let text = serde_json::to_string_pretty(&value).unwrap_or_default();
        Self {
            content: vec![Content::Text(text)],
            structured_content: Some(value),
            is_error: false,
        }
    }

    /// 工具执行出错（`isError: true`），让模型能看到原因并重试。
    pub fn error(message: impl Into<String>) -> Self {
        Self {
            is_error: true,
            ..Self::text(message)
        }
    }

    pub fn to_json(&self) -> Value {
        let mut m = Map::new();
        m.insert(
            "content".into(),
            Value::Array(self.content.iter().map(Content::to_json).collect()),
        );
        if let Some(s) = &self.structured_content {
            m.insert("structuredContent".into(), s.clone());
        }
        m.insert("isError".into(), Value::Bool(self.is_error));
        Value::Object(m)
    }
}

/// 工具执行错误，返回给客户端时是 `isError: true` 的结果。
#[derive(Debug, Clone, PartialEq)]
pub struct ToolError {
    pub message: String,
    /// 可选的结构化错误信息（例如 pagesift 错误码），放进 `structuredContent`。
    pub details: Option<Value>,
}

impl ToolError {
    pub fn new(message: impl Into<String>) -> Self {
        Self {
            message: message.into(),
            details: None,
        }
    }

    pub fn with_details(mut self, details: Value) -> Self {
        self.details = Some(details);
        self
    }

    pub(crate) fn into_output(self) -> ToolOutput {
        ToolOutput {
            structured_content: self.details,
            ..ToolOutput::error(self.message)
        }
    }
}

impl fmt::Display for ToolError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.message)
    }
}

// 不实现 std::error::Error，这样任何错误类型都能用 `?` 转换过来。
impl<E: std::error::Error> From<E> for ToolError {
    fn from(e: E) -> Self {
        ToolError::new(e.to_string())
    }
}

/// 单次调用的上下文：发送进度通知，读取客户端信息。
#[derive(Debug, Clone, Default)]
pub struct ToolContext {
    pub(crate) progress_token: Option<Value>,
    pub(crate) notify: Option<UnboundedSender<Value>>,
    pub(crate) client_info: Option<Value>,
}

impl ToolContext {
    /// 客户端请求了进度通知时返回 `true`。
    pub fn wants_progress(&self) -> bool {
        self.progress_token.is_some() && self.notify.is_some()
    }

    /// 发送 `notifications/progress`。客户端没给 `progressToken` 时什么也不做。
    /// `progress` 必须单调递增。
    pub fn progress(&self, progress: f64, total: Option<f64>, message: Option<&str>) {
        let (Some(token), Some(tx)) = (&self.progress_token, &self.notify) else {
            return;
        };
        let mut p = Map::new();
        p.insert("progressToken".into(), token.clone());
        p.insert("progress".into(), json!(progress));
        if let Some(t) = total {
            p.insert("total".into(), json!(t));
        }
        if let Some(m) = message {
            p.insert("message".into(), json!(m));
        }
        let _ = tx.send(jsonrpc::notification(
            "notifications/progress",
            Value::Object(p),
        ));
    }

    /// 客户端自报的 `clientInfo`（不可信，只用于日志）。
    pub fn client_info(&self) -> Option<&Value> {
        self.client_info.as_ref()
    }
}
