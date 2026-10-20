use serde_json::Value;

use crate::client::{ChatClient, ChatMessage, ChatRequest, ResponseFormat, Role, SchemaMode};
use crate::error::{LlmError, Result};
use crate::schema;

#[derive(Debug, Clone, Default)]
pub struct ExtractOptions {
    /// 额外的用户提示，例如"只提取价格"。
    pub prompt: Option<String>,
}

const SYSTEM: &str = "You extract structured data from web page content (Markdown). \
Reply with a single JSON value only, no commentary and no code fences. \
Use only information present in the page; use null where a value is missing and allowed.";

/// 从 Markdown 中提取满足 `schema` 的 JSON。
/// 优先用 `response_format: json_schema`，不支持则退回 prompt 约束；
/// 无论哪种都会本地校验，校验失败带着错误信息重试一次。
pub async fn extract_json(
    client: &ChatClient,
    markdown: &str,
    schema: &Value,
    opts: &ExtractOptions,
) -> Result<Value> {
    let mut use_native = client.config().schema_mode != SchemaMode::PromptOnly;
    let mut messages = vec![
        ChatMessage::new(Role::System, system_prompt(schema, use_native)),
        ChatMessage::new(Role::User, user_prompt(markdown, opts)),
    ];

    let mut last_err = String::new();
    for round in 0..2 {
        let reply = loop {
            let req = ChatRequest {
                messages: messages.clone(),
                temperature: Some(0.0),
                response_format: use_native.then(|| ResponseFormat::JsonSchema {
                    name: "extraction".into(),
                    schema: schema.clone(),
                }),
                ..Default::default()
            };
            match client.chat(&req).await {
                Ok(r) => break r,
                Err(LlmError::Upstream { status, .. })
                    if use_native
                        && client.config().schema_mode == SchemaMode::Auto
                        && matches!(status, 400 | 404 | 415 | 422) =>
                {
                    // 服务商不认 response_format，退回 prompt 约束
                    use_native = false;
                    messages[0] = ChatMessage::new(Role::System, system_prompt(schema, false));
                }
                Err(e) => return Err(e),
            }
        };

        let problem = match parse_json(&reply.content) {
            Ok(v) => match schema::validate(schema, &v) {
                Ok(()) => return Ok(v),
                Err(errs) => errs.join("; "),
            },
            Err(e) => format!("不是合法 JSON: {e}"),
        };
        last_err = problem.clone();
        if round == 0 {
            messages.push(ChatMessage::new(Role::Assistant, reply.content));
            messages.push(ChatMessage::new(
                Role::User,
                format!(
                    "The previous reply was invalid: {problem}. Reply again with only the corrected JSON."
                ),
            ));
        }
    }
    Err(LlmError::SchemaViolation(last_err))
}

fn system_prompt(schema: &Value, native: bool) -> String {
    if native {
        SYSTEM.to_owned()
    } else {
        format!("{SYSTEM}\nThe JSON must conform to this JSON Schema:\n{schema}")
    }
}

fn user_prompt(markdown: &str, opts: &ExtractOptions) -> String {
    let mut s = String::new();
    if let Some(p) = &opts.prompt {
        s.push_str(p);
        s.push_str("\n\n");
    }
    s.push_str("Page content:\n\n");
    s.push_str(markdown);
    s
}

/// 容忍模型包上 ```json 围栏或前后多余文字。
fn parse_json(text: &str) -> std::result::Result<Value, serde_json::Error> {
    let t = text.trim();
    if let Ok(v) = serde_json::from_str(t) {
        return Ok(v);
    }
    let inner = t
        .strip_prefix("```json")
        .or_else(|| t.strip_prefix("```"))
        .map(|s| s.trim_end().trim_end_matches("```").trim())
        .unwrap_or(t);
    match serde_json::from_str(inner) {
        Ok(v) => Ok(v),
        Err(e) => {
            let start = inner.find(['{', '[']);
            let end = inner.rfind(['}', ']']);
            if let (Some(s), Some(en)) = (start, end)
                && s < en
            {
                return serde_json::from_str(&inner[s..=en]);
            }
            Err(e)
        }
    }
}
