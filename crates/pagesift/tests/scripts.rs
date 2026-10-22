#[path = "../../pagesift-net/tests/common/mod.rs"]
mod common;
use pagesift::scripts::{ScriptBudget, discover_script_graph};
use pagesift_net::{Fetcher, FetcherConfig};
use pagesift_policy::{Policy, PolicyConfig};
use std::sync::Arc;
use std::time::Duration;

fn policy() -> Policy {
    let mut fc = FetcherConfig::default();
    fc.ssrf.allow_private_network = true;
    let mut pc = PolicyConfig::default();
    pc.rate.default_interval = Duration::ZERO;
    Policy::new(Arc::new(Fetcher::new(fc).unwrap()), pc)
}
fn js(body: &str) -> Vec<u8> {
    format!("HTTP/1.1 200 OK\r\nConnection: close\r\nContent-Type: application/javascript\r\nContent-Length: {}\r\n\r\n{body}",body.len()).into_bytes()
}

#[tokio::test]
async fn native_graph_follows_modules_with_document_relative_navigation_and_cycles() {
    let srv=common::serve(|r|Some(match r.path.as_str() {
        "/robots.txt"=>common::ok("User-agent: *\nAllow: /\n"),
        "/assets/app.js"=>js("import './child.js'; const ROOT='/api/'; fetch(ROOT+'products'); fetch('./relative'); fetch('/write',{method:'POST'});"),
        "/assets/child.js"=>js("export {x} from './app.js'; location.href='/article';"),
        _=>common::ok("no script"),
    })).await;
    let root = srv.url("/page/story").parse().unwrap();
    let graph = discover_script_graph(
        &policy(),
        "<script src='/assets/app.js'></script>",
        &root,
        ScriptBudget::default(),
    )
    .await
    .unwrap();
    assert_eq!(graph.fetched_files, 2);
    assert!(!graph.incomplete);
    for path in ["/api/products", "/page/relative", "/article"] {
        assert!(
            graph.edges.iter().any(|e| e.url == srv.url(path)),
            "missing {path}"
        );
    }
    assert!(!graph.edges.iter().any(|e| e.url.contains("/write")));
    assert!(
        !srv.seen().iter().any(|r| r.path.starts_with("/api/")),
        "discovery must not fetch navigation candidates"
    );
}

#[tokio::test]
async fn file_budget_counts_failed_attempts_and_redirects_cannot_escape_origin() {
    let external = common::serve(|_| Some(common::ok("external"))).await;
    let external_url = external.url("/must-not-send");
    let srv=common::serve(move|r|Some(if r.path=="/robots.txt" {common::ok("User-agent: *\nAllow: /\n")}
        else {format!("HTTP/1.1 302 Found\r\nLocation: {external_url}\r\nContent-Length: 0\r\nConnection: close\r\n\r\n").into_bytes()})).await;
    let graph = discover_script_graph(
        &policy(),
        "<script src='/a.js'></script><script src='/b.js'></script>",
        &srv.url("/").parse().unwrap(),
        ScriptBudget {
            max_files: 1,
            ..Default::default()
        },
    )
    .await
    .unwrap();
    assert!(graph.incomplete);
    assert_eq!(graph.failures.len(), 1);
    assert_eq!(
        srv.seen()
            .iter()
            .filter(|r| r.path.ends_with(".js"))
            .count(),
        1
    );
    assert!(external.seen().is_empty());
}
