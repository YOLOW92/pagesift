mod common;

use common::{Resp, spawn};
use pagesift_search::*;

fn fixture(name: &str) -> String {
    std::fs::read_to_string(format!(
        "{}/tests/fixtures/{name}",
        env!("CARGO_MANIFEST_DIR")
    ))
    .unwrap()
}

fn opts() -> HttpOptions {
    HttpOptions {
        timeout: std::time::Duration::from_millis(500),
    }
}

fn assert_rust_results(r: &[SearchResult], n: usize) {
    assert_eq!(r.len(), n);
    assert_eq!(r[0].url, "https://www.rust-lang.org/");
    assert_eq!(r[0].title, "Rust Programming Language");
    assert_eq!(r[0].description, "A language empowering everyone.");
    assert_eq!(
        r.iter().map(|x| x.position).collect::<Vec<_>>(),
        (1..=n).collect::<Vec<_>>()
    );
}

fn query_of(path: &str) -> std::collections::HashMap<String, String> {
    let q = path.split_once('?').map(|x| x.1).unwrap_or("");
    q.split('&')
        .filter_map(|kv| kv.split_once('='))
        .map(|(k, v)| (k.to_owned(), v.to_owned()))
        .collect()
}

fn full_query() -> SearchQuery {
    SearchQuery {
        query: "rust".into(),
        limit: 3,
        lang: Some("zh".into()),
        country: Some("cn".into()),
        time_range: Some(TimeRange::Week),
    }
}

#[tokio::test]
async fn searxng() {
    let srv = spawn(vec![Resp::json(200, fixture("searxng.json"))]).await;
    let b = SearxngBackend::new(&srv.base, None, opts()).unwrap();
    let r = b.search(&full_query()).await.unwrap();
    assert_rust_results(&r, 3);
    let q = query_of(srv.requests()[0].path());
    assert_eq!(q["format"], "json");
    assert_eq!(q["language"], "zh-CN");
    assert_eq!(q["time_range"], "month");
    assert!(srv.requests()[0].path().starts_with("/search?"));
}

#[tokio::test]
async fn searxng_truncates_to_limit() {
    let srv = spawn(vec![Resp::json(200, fixture("searxng.json"))]).await;
    let b = SearxngBackend::new(&srv.base, None, opts()).unwrap();
    assert_eq!(
        b.search(&SearchQuery::new("rust", 2)).await.unwrap().len(),
        2
    );
}

#[tokio::test]
async fn searxng_json_disabled_is_explained() {
    let srv = spawn(vec![Resp::json(403, "Forbidden")]).await;
    let b = SearxngBackend::new(&srv.base, None, opts()).unwrap();
    let err = b.search(&SearchQuery::new("rust", 5)).await.unwrap_err();
    assert!(
        matches!(err, SearchError::Upstream { status: 403, ref message } if message.contains("json"))
    );
}

#[tokio::test]
async fn brave() {
    let srv = spawn(vec![Resp::json(200, fixture("brave.json"))]).await;
    let b =
        BraveBackend::with_endpoint(format!("{}/res/v1/web/search", srv.base), "k-brave", opts())
            .unwrap();
    let r = b.search(&full_query()).await.unwrap();
    assert_rust_results(&r, 3);
    let req = &srv.requests()[0];
    assert_eq!(req.header("x-subscription-token"), Some("k-brave"));
    let q = query_of(req.path());
    assert_eq!(
        (
            q["count"].as_str(),
            q["country"].as_str(),
            q["search_lang"].as_str(),
            q["freshness"].as_str()
        ),
        ("3", "CN", "zh", "pw")
    );
}

#[tokio::test]
async fn brave_empty_results() {
    let srv = spawn(vec![Resp::json(
        200,
        r#"{"type":"search","query":{"original":"zzz"}}"#,
    )])
    .await;
    let b = BraveBackend::with_endpoint(&srv.base, "k", opts()).unwrap();
    assert!(
        b.search(&SearchQuery::new("zzz", 5))
            .await
            .unwrap()
            .is_empty()
    );
}

#[tokio::test]
async fn bing() {
    let srv = spawn(vec![Resp::json(200, fixture("bing.json"))]).await;
    let b =
        BingBackend::with_endpoint(format!("{}/v7.0/search", srv.base), "k-bing", opts()).unwrap();
    let r = b.search(&full_query()).await.unwrap();
    assert_rust_results(&r, 3);
    let req = &srv.requests()[0];
    assert_eq!(req.header("ocp-apim-subscription-key"), Some("k-bing"));
    let q = query_of(req.path());
    assert_eq!(q["mkt"], "zh-CN");
    assert!(!q.contains_key("cc"));
    assert_eq!(q["freshness"], "Week");
}

#[tokio::test]
async fn bing_year_unsupported() {
    let b = BingBackend::with_endpoint("http://127.0.0.1:1", "k", opts()).unwrap();
    let mut q = SearchQuery::new("x", 5);
    q.time_range = Some(TimeRange::Year);
    assert!(matches!(
        b.search(&q).await.unwrap_err(),
        SearchError::Unsupported(_)
    ));
}

#[tokio::test]
async fn google_cse() {
    let srv = spawn(vec![Resp::json(200, fixture("google_cse.json"))]).await;
    let b = GoogleCseBackend::with_endpoint(
        format!("{}/customsearch/v1", srv.base),
        "k-g",
        "cx-1",
        opts(),
    )
    .unwrap();
    let mut q = full_query();
    q.limit = 50;
    let r = b.search(&q).await.unwrap();
    assert_rust_results(&r, 3);
    let q = query_of(srv.requests()[0].path());
    assert_eq!(
        (
            q["key"].as_str(),
            q["cx"].as_str(),
            q["num"].as_str(),
            q["lr"].as_str(),
            q["gl"].as_str(),
            q["dateRestrict"].as_str()
        ),
        ("k-g", "cx-1", "10", "lang_zh-CN", "cn", "w1")
    );
}

#[tokio::test]
async fn google_quota_403_is_rate_limited() {
    let body = r#"{"error":{"code":403,"errors":[{"reason":"dailyLimitExceeded"}]}}"#;
    let srv = spawn(vec![Resp::json(403, body)]).await;
    let b = GoogleCseBackend::with_endpoint(&srv.base, "k", "cx", opts()).unwrap();
    assert!(matches!(
        b.search(&SearchQuery::new("x", 5)).await.unwrap_err(),
        SearchError::RateLimited { .. }
    ));
}

#[tokio::test]
async fn google_no_items_is_empty() {
    let srv = spawn(vec![Resp::json(200, r#"{"kind":"customsearch#search"}"#)]).await;
    let b = GoogleCseBackend::with_endpoint(&srv.base, "k", "cx", opts()).unwrap();
    assert!(
        b.search(&SearchQuery::new("x", 5))
            .await
            .unwrap()
            .is_empty()
    );
}

#[tokio::test]
async fn tavily() {
    let srv = spawn(vec![Resp::json(200, fixture("tavily.json"))]).await;
    let b = TavilyBackend::with_endpoint(format!("{}/search", srv.base), "tvly-k", opts()).unwrap();
    let r = b.search(&full_query()).await.unwrap();
    assert_rust_results(&r, 3);
    let req = &srv.requests()[0];
    assert!(req.request_line.starts_with("POST /search"));
    assert_eq!(req.header("authorization"), Some("Bearer tvly-k"));
    let body: serde_json::Value = serde_json::from_str(&req.body).unwrap();
    assert_eq!(body["query"], "rust");
    assert_eq!(body["max_results"], 3);
    assert_eq!(body["time_range"], "week");
}

#[tokio::test]
#[allow(clippy::type_complexity)]
async fn error_classification() {
    let cases: [(Resp, fn(&SearchError) -> bool); 4] = [
        (Resp::json(401, "{}"), |e| {
            matches!(e, SearchError::Auth { status: 401, .. })
        }),
        (
            Resp::json(429, "{}").header("Retry-After", "7"),
            |e| matches!(e, SearchError::RateLimited { retry_after: Some(d) } if d.as_secs() == 7),
        ),
        (Resp::json(502, "bad gateway"), |e| {
            matches!(e, SearchError::Upstream { status: 502, .. }) && e.is_retryable()
        }),
        (Resp::json(200, "<html>"), |e| {
            matches!(e, SearchError::InvalidResponse(_))
        }),
    ];
    for (resp, check) in cases {
        let srv = spawn(vec![resp]).await;
        let b = BraveBackend::with_endpoint(&srv.base, "k", opts()).unwrap();
        let err = b.search(&SearchQuery::new("x", 5)).await.unwrap_err();
        assert!(check(&err), "{err:?}");
    }
}

#[tokio::test]
async fn timeout_classified() {
    let srv = spawn(vec![
        Resp::json(200, "{}").delay(std::time::Duration::from_secs(2)),
    ])
    .await;
    let b = TavilyBackend::with_endpoint(
        &srv.base,
        "k",
        HttpOptions {
            timeout: std::time::Duration::from_millis(150),
        },
    )
    .unwrap();
    assert!(matches!(
        b.search(&SearchQuery::new("x", 5)).await.unwrap_err(),
        SearchError::Timeout
    ));
}

#[tokio::test]
async fn works_as_trait_object() {
    let srv = spawn(vec![Resp::json(200, fixture("tavily.json"))]).await;
    let b: Box<dyn SearchBackend> =
        Box::new(TavilyBackend::with_endpoint(&srv.base, "k", opts()).unwrap());
    assert_eq!(b.name(), "tavily");
    assert_eq!(
        b.search(&SearchQuery::new("rust", 2)).await.unwrap().len(),
        2
    );
}

#[test]
fn empty_keys_rejected() {
    assert!(BraveBackend::new("", opts()).is_err());
    assert!(GoogleCseBackend::new("k", " ", opts()).is_err());
}
