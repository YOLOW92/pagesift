mod common;

use std::time::Duration;

use common::{Resp, spawn};
use pagesift_llm::{
    ChatClient, ExtractOptions, LlmConfig, LlmError, SchemaMode, SummaryOptions, estimate_tokens,
    extract_json, summarize,
};
use serde_json::{Value, json};

fn reply(content: &str) -> Resp {
    Resp::json(
        200,
        json!({"choices":[{"message":{"role":"assistant","content":content}}]}).to_string(),
    )
}

fn client(base: &str, mode: SchemaMode) -> ChatClient {
    let mut cfg = LlmConfig::new(base, "m", None);
    cfg.schema_mode = mode;
    cfg.retry_backoff = Duration::from_millis(5);
    ChatClient::new(cfg).unwrap()
}

fn schema() -> Value {
    json!({"type":"object","required":["title","price"],"properties":{
        "title":{"type":"string"},"price":{"type":"number"}}})
}

fn body(r: &common::Recorded) -> Value {
    serde_json::from_str(&r.body).unwrap()
}

#[tokio::test]
async fn extract_uses_native_json_schema() {
    let srv = spawn(vec![reply(r#"{"title":"A","price":9.5}"#)]).await;
    let out = extract_json(
        &client(&srv.base, SchemaMode::Auto),
        "# A\nprice 9.5",
        &schema(),
        &ExtractOptions {
            prompt: Some("get product".into()),
        },
    )
    .await
    .unwrap();
    assert_eq!(out, json!({"title":"A","price":9.5}));
    let b = body(&srv.requests()[0]);
    assert_eq!(b["response_format"]["type"], "json_schema");
    assert_eq!(b["response_format"]["json_schema"]["schema"], schema());
    assert!(
        b["messages"][1]["content"]
            .as_str()
            .unwrap()
            .contains("get product")
    );
}

#[tokio::test]
async fn extract_falls_back_to_prompt_when_unsupported() {
    let srv = spawn(vec![
        Resp::json(
            400,
            r#"{"error":{"message":"response_format unsupported"}}"#,
        ),
        reply("```json\n{\"title\":\"A\",\"price\":1}\n```"),
    ])
    .await;
    let out = extract_json(
        &client(&srv.base, SchemaMode::Auto),
        "md",
        &schema(),
        &Default::default(),
    )
    .await
    .unwrap();
    assert_eq!(out["title"], "A");
    let reqs = srv.requests();
    assert_eq!(reqs.len(), 2);
    let b2 = body(&reqs[1]);
    assert!(b2.get("response_format").is_none());
    assert!(
        b2["messages"][0]["content"]
            .as_str()
            .unwrap()
            .contains("JSON Schema")
    );
}

#[tokio::test]
async fn extract_prompt_only_mode_never_sends_response_format() {
    let srv = spawn(vec![reply(r#"{"title":"A","price":1}"#)]).await;
    extract_json(
        &client(&srv.base, SchemaMode::PromptOnly),
        "md",
        &schema(),
        &Default::default(),
    )
    .await
    .unwrap();
    assert!(body(&srv.requests()[0]).get("response_format").is_none());
}

#[tokio::test]
async fn extract_retries_once_on_schema_violation() {
    let srv = spawn(vec![
        reply(r#"{"title":"A","price":"free"}"#),
        reply(r#"{"title":"A","price":0}"#),
    ])
    .await;
    let out = extract_json(
        &client(&srv.base, SchemaMode::Auto),
        "md",
        &schema(),
        &Default::default(),
    )
    .await
    .unwrap();
    assert_eq!(out["price"], 0);
    let b2 = body(&srv.requests()[1]);
    assert_eq!(b2["messages"].as_array().unwrap().len(), 4);
    assert!(
        b2["messages"][3]["content"]
            .as_str()
            .unwrap()
            .contains("$.price")
    );
}

#[tokio::test]
async fn extract_fails_after_second_violation() {
    let srv = spawn(vec![reply("not json at all")]).await;
    let err = extract_json(
        &client(&srv.base, SchemaMode::Auto),
        "md",
        &schema(),
        &Default::default(),
    )
    .await
    .unwrap_err();
    assert!(matches!(err, LlmError::SchemaViolation(_)));
    assert_eq!(srv.requests().len(), 2);
}

#[tokio::test]
async fn summary_single_call_when_short() {
    let srv = spawn(vec![reply("  short summary ")]).await;
    let out = summarize(
        &client(&srv.base, SchemaMode::Auto),
        "small page",
        &SummaryOptions {
            language: Some("中文".into()),
            ..Default::default()
        },
    )
    .await
    .unwrap();
    assert_eq!(out, "short summary");
    let reqs = srv.requests();
    assert_eq!(reqs.len(), 1);
    assert!(
        body(&reqs[0])["messages"][0]["content"]
            .as_str()
            .unwrap()
            .contains("中文")
    );
}

#[tokio::test]
async fn summary_chunks_long_input() {
    let srv = spawn(vec![reply("part")]).await;
    let text = (0..40)
        .map(|i| format!("para {i} {}", "w".repeat(80)))
        .collect::<Vec<_>>()
        .join("\n\n");
    let opts = SummaryOptions {
        max_input_tokens: 200,
        ..Default::default()
    };
    assert!(estimate_tokens(&text) > 200);
    summarize(&client(&srv.base, SchemaMode::Auto), &text, &opts)
        .await
        .unwrap();
    let reqs = srv.requests();
    assert!(reqs.len() > 2, "应先分段再合并，实际 {}", reqs.len());
    for r in &reqs {
        let content = body(r)["messages"][1]["content"]
            .as_str()
            .unwrap()
            .to_owned();
        assert!(estimate_tokens(&content) <= 200);
    }
}
