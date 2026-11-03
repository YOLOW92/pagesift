//! `POST /v2/scrape/{id}/interact` 和 `DELETE /v2/scrape/{id}/interact` 的请求和响应，字段照 Firecrawl v2。
//!
//! 和 Firecrawl 的差异：`code` / `language`（Playwright 代码）不支持，改用 `actions`（和 scrape 的动作列表相同）；
//! `cdpUrl`、`liveViewUrl`、`interactiveLiveViewUrl` 永远是 `null`，`warnings` 里写明。

use std::time::Duration;

use serde::{Deserialize, Serialize};
use serde_json::Value;

use super::Document;
use crate::error::{ApiError, ErrorCode};

/// [协议] Firecrawl API reference：`prompt` 1–10,000 字符
pub const MAX_PROMPT_CHARS: usize = 10_000;
/// [协议] Firecrawl API reference：`timeout` 默认 30 秒
pub const DEFAULT_INTERACT_TIMEOUT_SECS: f64 = 30.0;
/// [协议] Firecrawl API reference：`timeout` 上限 300 秒
pub const MAX_INTERACT_TIMEOUT_SECS: f64 = 300.0;
/// 警告代码：没有实时画面和 CDP 地址。
pub const LIVE_VIEW_UNAVAILABLE: &str = "live_view_unavailable";
/// 警告代码：页面没有保留到现在，这次重新打开了网址。
pub const SESSION_REPLAYED: &str = "session_replayed";

#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct InteractRequest {
    /// 自然语言任务（需要 `[llm]`）。和 `actions` 二选一。
    #[serde(default)]
    pub prompt: Option<String>,
    /// PageSift extension：和 scrape 相同的动作列表。和 `prompt` 二选一。
    #[serde(default)]
    pub actions: Option<Vec<Value>>,
    /// Firecrawl 的代码执行：不支持，收到时明确报错。
    #[serde(default)]
    pub code: Option<String>,
    #[serde(default)]
    pub language: Option<String>,
    /// 时限（秒），1–300，默认 30。
    #[serde(default)]
    pub timeout: Option<f64>,
    /// 原来的抓取已经没有记录时，在这个网址上开新会话。
    #[serde(default)]
    pub url: Option<String>,
}

impl InteractRequest {
    /// 请求本身的校验（和会话状态无关）。
    pub fn check(&self) -> Result<(), ApiError> {
        if self.code.is_some() {
            return Err(ApiError::unsupported(
                "code execution is not supported: pagesift runs no Playwright sandbox; send `actions` or `prompt`",
            ));
        }
        match (&self.prompt, &self.actions) {
            (Some(_), Some(_)) => {
                return Err(ApiError::invalid(
                    "send either `prompt` or `actions`, not both",
                ));
            }
            (None, None) => {
                return Err(ApiError::invalid("send `prompt` or `actions`"));
            }
            (Some(p), None) => {
                let n = p.chars().count();
                if n == 0 || n > MAX_PROMPT_CHARS {
                    return Err(ApiError::invalid(format!(
                        "prompt must be 1 to {MAX_PROMPT_CHARS} characters, got {n}"
                    )));
                }
            }
            (None, Some(_)) => {}
        }
        if let Some(t) = self.timeout
            && !(1.0..=MAX_INTERACT_TIMEOUT_SECS).contains(&t)
        {
            return Err(ApiError::invalid(format!(
                "timeout must be 1 to {MAX_INTERACT_TIMEOUT_SECS} seconds, got {t}"
            )));
        }
        Ok(())
    }

    pub fn time_limit(&self) -> Duration {
        Duration::from_secs_f64(self.timeout.unwrap_or(DEFAULT_INTERACT_TIMEOUT_SECS))
    }
}

/// `success: false` 是这次动作没有做完（选择器找不到、超时），会话还在，可以继续调用；
/// 会话本身不可用（已关闭、已回收、id 不对）走 HTTP 错误。
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct InteractResponse {
    pub success: bool,
    /// 永远是 `null`：不开放浏览器的调试端口。
    pub cdp_url: Option<String>,
    /// 永远是 `null`：没有实时画面。
    pub live_view_url: Option<String>,
    /// 永远是 `null`：没有实时画面。
    pub interactive_live_view_url: Option<String>,
    /// 模型对 `prompt` 的回答。
    #[serde(skip_serializing_if = "Option::is_none")]
    pub output: Option<String>,
    /// 没有代码执行，永远为空。
    pub stdout: String,
    /// 没有代码执行，永远为空。
    pub stderr: String,
    /// `prompt`：模型最后看到的页面快照；`actions`：最后一个 `executeJavascript` 的返回值。
    #[serde(skip_serializing_if = "Option::is_none")]
    pub result: Option<String>,
    pub exit_code: i32,
    /// 到时限被中止。
    pub killed: bool,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub error: Option<String>,
    /// PageSift extension：动作之后的页面，格式取自原来的抓取。
    #[serde(skip_serializing_if = "Option::is_none")]
    pub data: Option<Document>,
    /// PageSift extension：`actions` 的产出（截图、`scrape`、`executeJavascript`、PDF）。
    #[serde(skip_serializing_if = "Option::is_none")]
    pub actions: Option<Value>,
    /// PageSift extension：`prompt` 实际执行的步骤（动作、元素的角色和名称；不含输入的文字）。
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub steps: Vec<Value>,
    /// PageSift extension：`prompt` 调了几次模型、多少 token（`{calls, tokens}`）。
    #[serde(skip_serializing_if = "Option::is_none")]
    pub llm_usage: Option<Value>,
    #[serde(default)]
    pub warnings: Vec<String>,
}

impl Default for InteractResponse {
    fn default() -> Self {
        Self {
            success: true,
            cdp_url: None,
            live_view_url: None,
            interactive_live_view_url: None,
            output: None,
            stdout: String::new(),
            stderr: String::new(),
            result: None,
            exit_code: 0,
            killed: false,
            error: None,
            data: None,
            actions: None,
            steps: Vec::new(),
            llm_usage: None,
            warnings: vec![format!(
                "{LIVE_VIEW_UNAVAILABLE}: pagesift offers no live view and does not expose the browser's CDP endpoint; cdpUrl, liveViewUrl and interactiveLiveViewUrl are always null"
            )],
        }
    }
}

impl InteractResponse {
    /// 这次调用没有做完：记下原因，会话留着。
    pub fn fail(&mut self, error: impl Into<String>) {
        self.success = false;
        self.exit_code = 1;
        self.error = Some(error.into());
    }
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct InteractStopped {
    pub success: bool,
    pub session_duration_ms: u64,
    /// pagesift 没有计费，永远是 0。
    pub credits_billed: u64,
}

/// 这个进程的标记：scrapeId 的第一段。重启后的旧 id 第一段对不上，能明确告诉调用方原因。
pub fn instance_tag() -> String {
    // [协议] UUID 文本形式第一段 8 个十六进制字符（RFC 9562）
    uuid::Uuid::new_v4().to_string()[..8].to_string()
}

/// 带进程标记的 scrapeId，仍是合法的 UUID 文本。
pub fn scrape_id(tag: &str) -> String {
    // [协议] 同上：第一段 8 个字符
    format!("{tag}{}", &uuid::Uuid::new_v4().to_string()[8..])
}

/// 形状是 UUID（scrapeId 都是）。
pub fn is_scrape_id(id: &str) -> bool {
    uuid::Uuid::try_parse(id).is_ok()
}

/// 形状像 UUID，但第一段不是这个进程的标记。
pub fn issued_elsewhere(id: &str, tag: &str) -> bool {
    is_scrape_id(id) && !id.starts_with(tag)
}

/// 会话已经不在了（HTTP 410）。
pub fn session_gone(message: impl Into<String>) -> ApiError {
    ApiError::new(ErrorCode::SessionGone, message)
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn req(v: Value) -> InteractRequest {
        serde_json::from_value(v).unwrap()
    }

    #[test]
    fn request_rules_follow_firecrawl() {
        assert!(req(json!({"prompt": "x"})).check().is_ok());
        assert!(req(json!({"actions": []})).check().is_ok());
        assert_eq!(
            req(json!({"code": "1"})).check().unwrap_err().code,
            ErrorCode::UnsupportedOption
        );
        for bad in [
            json!({}),
            json!({"prompt": "x", "actions": []}),
            json!({"prompt": ""}),
            json!({"prompt": "x", "timeout": 0.5}),
            json!({"prompt": "x", "timeout": 301}),
        ] {
            assert_eq!(
                req(bad.clone()).check().unwrap_err().code,
                ErrorCode::InvalidRequest,
                "{bad}"
            );
        }
        assert_eq!(req(json!({"prompt": "x"})).time_limit().as_secs(), 30);
    }

    #[test]
    fn ids_are_uuids_and_tell_a_previous_process_apart() {
        let tag = instance_tag();
        let id = scrape_id(&tag);
        assert!(uuid::Uuid::try_parse(&id).is_ok(), "{id}");
        assert!(!issued_elsewhere(&id, &tag));
        assert!(issued_elsewhere(&scrape_id(&instance_tag()), &tag));
        assert!(!issued_elsewhere("not-a-uuid", &tag));
    }

    #[test]
    fn the_response_says_there_is_no_live_view() {
        let v = serde_json::to_value(InteractResponse::default()).unwrap();
        assert_eq!(v["liveViewUrl"], Value::Null);
        assert_eq!(v["interactiveLiveViewUrl"], Value::Null);
        assert_eq!(v["cdpUrl"], Value::Null);
        assert!(
            v["warnings"][0]
                .as_str()
                .unwrap()
                .starts_with("live_view_unavailable: ")
        );
    }
}
