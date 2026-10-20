mod common;

use std::time::Duration;

use common::{Resp, spawn};
use pagesift_llm::{ChatClient, ChatMessage, ChatRequest, LlmConfig, LlmError, Role};

fn ok_body(content: &str) -> String {
    serde_json::json!({"choices":[{"message":{"role":"assistant","content":content},"finish_reason":"stop"}]}).to_string()
}

pub fn client(base: &str) -> ChatClient {
    let mut cfg = LlmConfig::new(format!("{base}/v1/"), "test-model", Some("test-key".into()));
    cfg.retry_backoff = Duration::from_millis(10);
    cfg.timeout = Duration::from_secs(2);
    ChatClient::new(cfg).unwrap()
}

fn req() -> ChatRequest {
    ChatRequest {
        messages: vec![ChatMessage::new(Role::User, "hi")],
        ..Default::default()
    }
}

#[tokio::test]
async fn sends_openai_request() {
    let srv = spawn(vec![Resp::json(200, ok_body("hello"))]).await;
    let out = client(&srv.base).chat(&req()).await.unwrap();
    assert_eq!(out.content, "hello");
    let r = &srv.requests()[0];
    assert_eq!(r.request_line, "POST /v1/chat/completions HTTP/1.1");
    assert_eq!(r.header("authorization"), Some("Bearer test-key"));
    let body: serde_json::Value = serde_json::from_str(&r.body).unwrap();
    assert_eq!(body["model"], "test-model");
    assert_eq!(body["messages"][0]["role"], "user");
}

#[tokio::test]
async fn no_auth_header_without_key() {
    let srv = spawn(vec![Resp::json(200, ok_body("x"))]).await;
    let cfg = LlmConfig::new(srv.base.clone(), "m", None);
    ChatClient::new(cfg).unwrap().chat(&req()).await.unwrap();
    assert!(srv.requests()[0].header("authorization").is_none());
}

#[tokio::test]
async fn auth_error_not_retried() {
    let srv = spawn(vec![Resp::json(401, r#"{"error":{"message":"bad key"}}"#)]).await;
    let err = client(&srv.base).chat(&req()).await.unwrap_err();
    assert!(matches!(err, LlmError::Auth { status: 401, ref message } if message == "bad key"));
    assert_eq!(srv.requests().len(), 1);
}

#[tokio::test]
async fn retries_429_with_retry_after_then_succeeds() {
    let srv = spawn(vec![
        Resp::json(429, "{}").header("Retry-After", "0"),
        Resp::json(200, ok_body("ok")),
    ])
    .await;
    let out = client(&srv.base).chat(&req()).await.unwrap();
    assert_eq!(out.content, "ok");
    assert_eq!(srv.requests().len(), 2);
}

#[tokio::test]
async fn rate_limit_exhausts_retries() {
    let srv = spawn(vec![Resp::json(429, "{}").header("Retry-After", "0")]).await;
    let err = client(&srv.base).chat(&req()).await.unwrap_err();
    assert!(matches!(err, LlmError::RateLimited { .. }));
    assert_eq!(srv.requests().len(), 3);
}

#[tokio::test]
async fn long_retry_after_gives_up_immediately() {
    let srv = spawn(vec![Resp::json(429, "{}").header("Retry-After", "3600")]).await;
    let err = client(&srv.base).chat(&req()).await.unwrap_err();
    assert!(matches!(err, LlmError::RateLimited { retry_after: Some(d) } if d.as_secs() == 3600));
    assert_eq!(srv.requests().len(), 1);
}

#[tokio::test]
async fn retries_5xx() {
    let srv = spawn(vec![
        Resp::json(503, r#"{"error":"down"}"#),
        Resp::json(200, ok_body("back")),
    ])
    .await;
    assert_eq!(
        client(&srv.base).chat(&req()).await.unwrap().content,
        "back"
    );
}

#[tokio::test]
async fn persistent_5xx_is_upstream_error() {
    let srv = spawn(vec![Resp::json(500, "boom")]).await;
    let err = client(&srv.base).chat(&req()).await.unwrap_err();
    assert!(matches!(err, LlmError::Upstream { status: 500, .. }));
    assert!(err.is_retryable());
}

#[tokio::test]
async fn timeout_is_classified() {
    let srv = spawn(vec![
        Resp::json(200, ok_body("late")).delay(Duration::from_secs(3)),
    ])
    .await;
    let mut cfg = LlmConfig::new(srv.base.clone(), "m", None);
    cfg.timeout = Duration::from_millis(200);
    let err = ChatClient::new(cfg)
        .unwrap()
        .chat(&req())
        .await
        .unwrap_err();
    assert!(matches!(err, LlmError::Timeout));
}

#[tokio::test]
async fn malformed_success_body() {
    let srv = spawn(vec![Resp::json(200, r#"{"nope":1}"#)]).await;
    let err = client(&srv.base).chat(&req()).await.unwrap_err();
    assert!(matches!(err, LlmError::InvalidResponse(_)));
}

#[test]
fn rejects_empty_config() {
    assert!(ChatClient::new(LlmConfig::new("", "m", None)).is_err());
    assert!(ChatClient::new(LlmConfig::new("http://x", "", None)).is_err());
}
