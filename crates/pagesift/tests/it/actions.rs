//! `actions` 走真实浏览器：`/v2/scrape`、`/v1/scrape`、MCP、Jina 的 `x-wait-for-selector` 和
//! `x-respond-with: screenshot/pageshot`。真实 Chrome + 本地站点（没有浏览器时跳过），不连实网。

use std::sync::Arc;
use std::time::Duration;

use axum::Router;
use axum::http::StatusCode;
use axum::response::Html;
use axum::routing::get;
use pagesift::{Scraper, ScraperConfig};
use serde_json::{Value, json};

use super::interact::{available, call};

const PAGE: &str = r#"<html><head><title>Actions</title></head><body><main>
<h1 style="color: white; background: darkblue; padding: 40px">Start page with a coloured heading</h1>
<button id="more" onclick="const p = document.createElement('p'); p.textContent = 'Extra content after the click'; document.querySelector('main').appendChild(p);">Load more</button>
</main></body></html>"#;

/// 正文够长、脚本 800 ms 后才补上一段：不等选择器时抓不到它。
const LATE: &str = r#"<html><head><title>Late</title></head><body><main>
<h1>A page that finishes late</h1><p>This paragraph is here from the start and it is long enough, together with its neighbours, for the page not to look like an empty shell while the timer is still running on it. It keeps going for a while so that the renderer has no reason to wait for more text to show up.</p>
<script>setTimeout(() => { const p = document.createElement('p'); p.id = 'late'; p.textContent = 'Late content from a timer'; document.querySelector('main').appendChild(p); }, 800);</script>
</main></body></html>"#;

async fn site() -> String {
    let tall = format!(
        "<html><body>{}</body></html>",
        (0..200)
            .map(|i| format!("<p style='height: 40px'>Row {i}</p>"))
            .collect::<String>()
    );
    let app = Router::new()
        .route("/page", get(|| async { Html(PAGE) }))
        .route("/late", get(|| async { Html(LATE) }))
        .route(
            "/blank",
            get(|| async { Html("<html><body></body></html>") }),
        )
        .route(
            "/tall",
            get(move || {
                let t = tall.clone();
                async move { Html(t) }
            }),
        );
    let l = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let url = format!("http://{}", l.local_addr().unwrap());
    tokio::spawn(async move { axum::serve(l, app).await.unwrap() });
    url
}

fn serve() -> (Arc<Scraper>, Router) {
    let mut cfg = ScraperConfig::default();
    cfg.fetcher.env_proxy = false;
    cfg.fetcher.ssrf.allow_private_network = true;
    cfg.policy.rate.default_interval = Duration::from_millis(1);
    let s = Arc::new(Scraper::new(cfg).unwrap());
    let app = pagesift::api::router(s.clone(), Default::default());
    (s, app)
}

/// `data:image/png;base64,…` → PNG 字节。
fn png_bytes(data_url: &str) -> Vec<u8> {
    let b64 = data_url
        .strip_prefix("data:image/png;base64,")
        .unwrap_or_else(|| panic!("not a PNG data URL: {:.60}", data_url));
    let value = |c: u8| match c {
        b'A'..=b'Z' => c - b'A',
        b'a'..=b'z' => c - b'a' + 26,
        b'0'..=b'9' => c - b'0' + 52,
        b'+' => 62,
        b'/' => 63,
        _ => panic!("bad base64 byte {c}"),
    };
    let mut out = Vec::with_capacity(b64.len() / 4 * 3);
    for chunk in b64.trim_end_matches('=').as_bytes().chunks(4) {
        let mut n = 0u32;
        for &c in chunk {
            n = (n << 6) | u32::from(value(c));
        }
        n <<= 6 * (4 - chunk.len());
        out.extend_from_slice(&n.to_be_bytes()[1..chunk.len()]);
    }
    out
}

/// PNG 文件头里的宽和高。
fn png_size(png: &[u8]) -> (u32, u32) {
    assert_eq!(&png[..8], b"\x89PNG\r\n\x1a\n", "not a PNG");
    let be = |r: std::ops::Range<usize>| u32::from_be_bytes(png[r].try_into().unwrap());
    (be(16..20), be(20..24))
}

async fn jina(app: &Router, url: &str, headers: &[(&str, &str)]) -> (u16, String) {
    use http_body_util::BodyExt;
    use tower::ServiceExt;
    let mut req = axum::http::Request::builder().uri(format!("/r/{url}"));
    for (k, v) in headers {
        req = req.header(*k, *v);
    }
    let resp = app
        .clone()
        .oneshot(req.body(axum::body::Body::empty()).unwrap())
        .await
        .unwrap();
    let status = resp.status().as_u16();
    let body = resp.into_body().collect().await.unwrap().to_bytes();
    (status, String::from_utf8_lossy(&body).into_owned())
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn scrape_actions_run_in_the_browser() {
    if !available() {
        return;
    }
    let base = site().await;
    let (s, app) = serve();
    let url = format!("{base}/page");
    let body = json!({
        "url": url,
        "onlyMainContent": false,
        "actions": [
            {"type": "click", "selector": "#more"},
            {"type": "scrape"},
            {"type": "screenshot"},
        ],
    });

    // 没写 engine 也走浏览器：动作真的做了，提取的是动作之后的页面，产出在 data.actions
    for route in ["/v2/scrape", "/v1/scrape"] {
        let (status, v) = call(&app, "POST", route, Some(body.clone())).await;
        assert_eq!(status, StatusCode::OK, "{route}: {v}");
        let data = &v["data"];
        assert!(
            data["markdown"]
                .as_str()
                .unwrap()
                .contains("Extra content after the click"),
            "{route}: {v}"
        );
        assert_eq!(data["metadata"]["pagesift"]["engine"], "browser", "{v}");
        assert!(
            data["actions"]["scrapes"][0]["html"]
                .as_str()
                .unwrap()
                .contains("Extra content after the click"),
            "{route}: {v}"
        );
        let shot = png_bytes(data["actions"]["screenshots"][0].as_str().unwrap());
        assert!(png_size(&shot).0 > 0, "{route}");
    }

    let bad = [
        // 选择器找不到：指出是哪个动作，不是悄悄跳过
        (
            json!({"url": url, "actions": [{"type": "click", "selector": "#missing"}]}),
            StatusCode::BAD_REQUEST,
            "INVALID_REQUEST",
            "#missing",
        ),
        (
            json!({"url": url, "engine": "http", "actions": [{"type": "wait", "milliseconds": 1}]}),
            StatusCode::BAD_REQUEST,
            "INVALID_REQUEST",
            "engine",
        ),
        (
            json!({"url": url, "respondTiming": "html", "actions": [{"type": "wait", "milliseconds": 1}]}),
            StatusCode::BAD_REQUEST,
            "INVALID_REQUEST",
            "respondTiming",
        ),
        (
            json!({"url": url, "actions": [{"type": "fly"}]}),
            StatusCode::BAD_REQUEST,
            "INVALID_REQUEST",
            "fly",
        ),
        // 脚本动作默认关着
        (
            json!({"url": url, "actions": [{"type": "executeJavascript", "script": "1"}]}),
            StatusCode::BAD_REQUEST,
            "UNSUPPORTED_OPTION",
            "",
        ),
        // 等不到的选择器：按时限结束
        (
            json!({"url": url, "timeout": 2000, "actions": [{"type": "wait", "selector": "#never"}]}),
            StatusCode::REQUEST_TIMEOUT,
            "TIMEOUT",
            "",
        ),
    ];
    for (body, status, code, mention) in bad {
        let (st, v) = call(&app, "POST", "/v2/scrape", Some(body.clone())).await;
        assert_eq!(st, status, "{body}: {v}");
        assert_eq!(v["code"], code, "{body}: {v}");
        assert!(v["error"].as_str().unwrap().contains(mention), "{v}");
    }
    s.shutdown().await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn kept_pages_keep_the_state_their_actions_left() {
    if !available() {
        return;
    }
    let base = site().await;
    let (s, app) = serve();
    let (status, v) = call(
        &app,
        "POST",
        "/v2/scrape",
        Some(json!({
            "url": format!("{base}/page"),
            "keepSession": true,
            "onlyMainContent": false,
            "actions": [{"type": "click", "selector": "#more"}],
        })),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{v}");
    let id = v["data"]["metadata"]["scrapeId"].as_str().unwrap();
    let (_, v) = call(
        &app,
        "POST",
        &format!("/v2/scrape/{id}/interact"),
        Some(json!({"actions": [{"type": "scrape"}]})),
    )
    .await;
    assert!(
        v["actions"]["scrapes"][0]["html"]
            .as_str()
            .unwrap()
            .contains("Extra content after the click"),
        "{v}"
    );
    s.shutdown().await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn jina_screenshot_and_pageshot_have_real_content() {
    if !available() {
        return;
    }
    let base = site().await;
    let (s, app) = serve();
    let shoot = async |path: &str, kind: &str| {
        let (status, body) =
            jina(&app, &format!("{base}{path}"), &[("x-respond-with", kind)]).await;
        assert_eq!(status, 200, "{path} {kind}: {body:.300}");
        png_bytes(&body)
    };
    let blank = shoot("/blank", "screenshot").await;
    let page = shoot("/page", "screenshot").await;
    assert!(
        page.len() > blank.len(),
        "a page with text and colour must not look like a blank one: {} vs {} bytes",
        page.len(),
        blank.len()
    );
    // screenshot 是一屏，pageshot 是整页
    let (_, screen) = png_size(&shoot("/tall", "screenshot").await);
    let (_, whole) = png_size(&shoot("/tall", "pageshot").await);
    assert!(whole > screen, "pageshot {whole} vs screenshot {screen}");
    assert!(whole >= 200 * 40, "the whole page is 8000 px tall: {whole}");
    s.shutdown().await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn jina_wait_for_selector_waits() {
    if !available() {
        return;
    }
    let base = site().await;
    let (s, app) = serve();
    let url = format!("{base}/late");
    // 对照：不等选择器时，定时器补上的内容还没出现
    let (status, body) = jina(&app, &url, &[("x-engine", "browser")]).await;
    assert_eq!(status, 200, "{body}");
    assert!(!body.contains("Late content"), "control: {body}");
    // 等选择器：拿到定时器补上的内容
    let (status, body) = jina(&app, &url, &[("x-wait-for-selector", "#late")]).await;
    assert_eq!(status, 200, "{body}");
    assert!(body.contains("Late content"), "{body}");
    // 等不到：按时限报错，不是返回没等到的页面
    let (status, body) = jina(
        &app,
        &url,
        &[("x-wait-for-selector", "#never"), ("x-timeout", "2")],
    )
    .await;
    assert_eq!(status, 408, "{body}");
    assert!(body.contains("TIMEOUT"), "{body}");
    s.shutdown().await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn mcp_offers_and_runs_actions() {
    use pagesift_mcp::{ToolContext, ToolHandler};
    if !available() {
        return;
    }
    let base = site().await;
    let (s, _) = serve();
    let tools = pagesift::mcp::tools(s.clone());
    let scrape: &Arc<dyn ToolHandler> = tools
        .iter()
        .find(|t| t.definition().name == "pagesift_scrape")
        .unwrap();
    // 参数表如实列出动作
    let schema = scrape.definition().input_schema;
    let kinds = schema["properties"]["actions"]["items"]["properties"]["type"]["enum"]
        .as_array()
        .cloned()
        .unwrap();
    assert!(kinds.contains(&json!("click")), "{kinds:?}");
    let args: Value = json!({
        "url": format!("{base}/page"),
        "onlyMainContent": false,
        "actions": [{"type": "click", "selector": "#more"}],
    });
    let out = scrape
        .call(args.as_object().unwrap().clone(), ToolContext::default())
        .await
        .unwrap();
    let doc = out.structured_content.unwrap();
    assert!(
        doc["markdown"]
            .as_str()
            .unwrap()
            .contains("Extra content after the click"),
        "{doc}"
    );
    s.shutdown().await;
}
