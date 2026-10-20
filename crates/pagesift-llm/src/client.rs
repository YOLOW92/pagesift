use std::time::Duration;

use reqwest::header::{AUTHORIZATION, CONTENT_TYPE, RETRY_AFTER};
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};

use crate::error::{LlmError, Result};

/// 服务商对 `response_format: json_schema` 的支持策略。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum SchemaMode {
    /// 先试 json_schema，服务商返回 400/422 时退回 prompt 约束。
    #[default]
    Auto,
    /// 一律使用 json_schema。
    JsonSchema,
    /// 一律用 prompt 约束（Ollama 旧版本、部分 vLLM 配置）。
    PromptOnly,
}

#[derive(Debug, Clone)]
pub struct LlmConfig {
    /// 例如 `https://api.openai.com/v1`、`http://localhost:11434/v1`，
    /// 请求发往 `{base_url}/chat/completions`。
    pub base_url: String,
    pub api_key: Option<String>,
    pub model: String,
    pub timeout: Duration,
    /// 遇到 429 和 5xx 时的最大重试次数（不含首次请求）。
    pub max_retries: u32,
    /// 没有 Retry-After 时的退避基数，按 2 的幂递增。
    pub retry_backoff: Duration,
    /// 单次等待的上限，Retry-After 超过它时直接放弃重试。
    pub max_retry_wait: Duration,
    pub schema_mode: SchemaMode,
}

impl LlmConfig {
    pub fn new(
        base_url: impl Into<String>,
        model: impl Into<String>,
        api_key: Option<String>,
    ) -> Self {
        Self {
            base_url: base_url.into(),
            api_key,
            model: model.into(),
            timeout: Duration::from_secs(60),
            max_retries: 2,
            retry_backoff: Duration::from_millis(500),
            max_retry_wait: Duration::from_secs(30),
            schema_mode: SchemaMode::Auto,
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Role {
    System,
    User,
    Assistant,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ChatMessage {
    pub role: Role,
    pub content: String,
}

impl ChatMessage {
    pub fn new(role: Role, content: impl Into<String>) -> Self {
        Self {
            role,
            content: content.into(),
        }
    }
}

#[derive(Debug, Clone)]
pub enum ResponseFormat {
    JsonObject,
    JsonSchema { name: String, schema: Value },
}

#[derive(Debug, Clone, Default)]
pub struct ChatRequest {
    pub messages: Vec<ChatMessage>,
    pub temperature: Option<f32>,
    pub max_tokens: Option<u32>,
    pub response_format: Option<ResponseFormat>,
}

#[derive(Debug, Clone)]
pub struct ChatResponse {
    pub content: String,
    pub finish_reason: Option<String>,
}

#[derive(Clone)]
pub struct ChatClient {
    http: reqwest::Client,
    config: LlmConfig,
}

impl ChatClient {
    pub fn new(config: LlmConfig) -> Result<Self> {
        if config.base_url.trim().is_empty() {
            return Err(LlmError::Config("base_url 不能为空".into()));
        }
        if config.model.trim().is_empty() {
            return Err(LlmError::Config("model 不能为空".into()));
        }
        let http = reqwest::Client::builder()
            .timeout(config.timeout)
            .build()
            .map_err(|e| LlmError::Config(e.to_string()))?;
        Ok(Self { http, config })
    }

    pub fn config(&self) -> &LlmConfig {
        &self.config
    }

    fn build_body(&self, req: &ChatRequest) -> Value {
        let mut body = json!({
            "model": self.config.model,
            "messages": req.messages,
            "stream": false,
        });
        if let Some(t) = req.temperature {
            body["temperature"] = json!(t);
        }
        if let Some(m) = req.max_tokens {
            body["max_tokens"] = json!(m);
        }
        match &req.response_format {
            Some(ResponseFormat::JsonObject) => {
                body["response_format"] = json!({"type": "json_object"});
            }
            Some(ResponseFormat::JsonSchema { name, schema }) => {
                body["response_format"] = json!({
                    "type": "json_schema",
                    "json_schema": {"name": name, "schema": schema},
                });
            }
            None => {}
        }
        body
    }

    /// 发送请求；429 和 5xx 按 Retry-After（缺省用指数退避）有限次重试。
    pub async fn chat(&self, req: &ChatRequest) -> Result<ChatResponse> {
        let url = format!(
            "{}/chat/completions",
            self.config.base_url.trim_end_matches('/')
        );
        let body = serde_json::to_vec(&self.build_body(req))
            .map_err(|e| LlmError::InvalidResponse(e.to_string()))?;

        let mut attempt = 0u32;
        loop {
            let err = match self.send_once(&url, &body).await {
                Ok(resp) => return Ok(resp),
                Err((err, retry_after)) => (err, retry_after),
            };
            let (err, retry_after) = err;
            let retryable = matches!(
                err,
                LlmError::RateLimited { .. } | LlmError::Upstream { status: 500.., .. }
            );
            if !retryable || attempt >= self.config.max_retries {
                return Err(err);
            }
            let wait = retry_after.unwrap_or_else(|| {
                self.config
                    .retry_backoff
                    .saturating_mul(1 << attempt.min(16))
            });
            if wait > self.config.max_retry_wait {
                return Err(err);
            }
            tokio::time::sleep(wait).await;
            attempt += 1;
        }
    }

    async fn send_once(
        &self,
        url: &str,
        body: &[u8],
    ) -> std::result::Result<ChatResponse, (LlmError, Option<Duration>)> {
        let mut rb = self
            .http
            .post(url)
            .header(CONTENT_TYPE, "application/json")
            .body(body.to_vec());
        if let Some(key) = &self.config.api_key {
            rb = rb.header(AUTHORIZATION, format!("Bearer {key}"));
        }
        let resp = rb.send().await.map_err(|e| (map_transport(e), None))?;
        let status = resp.status();
        let retry_after = resp
            .headers()
            .get(RETRY_AFTER)
            .and_then(|v| v.to_str().ok())
            .and_then(|v| v.trim().parse::<u64>().ok())
            .map(Duration::from_secs);
        let text = resp.text().await.map_err(|e| (map_transport(e), None))?;
        let code = status.as_u16();
        if status.is_success() {
            return parse_success(&text).map_err(|e| (e, None));
        }
        let message = error_message(&text);
        let err = match code {
            401 | 403 => LlmError::Auth {
                status: code,
                message,
            },
            429 => LlmError::RateLimited { retry_after },
            _ => LlmError::Upstream {
                status: code,
                message,
            },
        };
        Err((err, retry_after))
    }
}

fn map_transport(e: reqwest::Error) -> LlmError {
    if e.is_timeout() {
        LlmError::Timeout
    } else {
        LlmError::Network(e.to_string())
    }
}

fn error_message(body: &str) -> String {
    let parsed: Option<Value> = serde_json::from_str(body).ok();
    let msg = parsed.as_ref().and_then(|v| {
        v.pointer("/error/message")
            .or_else(|| v.get("error"))
            .or_else(|| v.get("message"))
            .and_then(Value::as_str)
            .map(str::to_owned)
    });
    let msg = msg.unwrap_or_else(|| body.to_owned());
    msg.chars().take(500).collect()
}

fn parse_success(text: &str) -> Result<ChatResponse> {
    let v: Value =
        serde_json::from_str(text).map_err(|e| LlmError::InvalidResponse(e.to_string()))?;
    let choice = v
        .pointer("/choices/0")
        .ok_or_else(|| LlmError::InvalidResponse("缺少 choices[0]".into()))?;
    let content = choice
        .pointer("/message/content")
        .and_then(Value::as_str)
        .ok_or_else(|| LlmError::InvalidResponse("缺少 message.content".into()))?
        .to_owned();
    let finish_reason = choice
        .get("finish_reason")
        .and_then(Value::as_str)
        .map(str::to_owned);
    Ok(ChatResponse {
        content,
        finish_reason,
    })
}
