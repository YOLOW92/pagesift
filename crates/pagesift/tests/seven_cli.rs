//! Real production CLI processes, local HTTP origin and filesystem recovery.
#[path = "../../pagesift-net/tests/common/mod.rs"]
mod common;
use std::process::Command;

fn cli(config: &std::path::Path, args: &[&str]) -> serde_json::Value {
    let output = Command::new(env!("CARGO_BIN_EXE_pagesift"))
        .args(["--config", config.to_str().unwrap()])
        .args(args)
        .output()
        .unwrap();
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    serde_json::from_slice(&output.stdout).unwrap()
}
fn response(mime: &str, body: &[u8]) -> Vec<u8> {
    let mut r=format!("HTTP/1.1 200 OK\r\nConnection: close\r\nContent-Type: {mime}\r\nContent-Length: {}\r\nETag: \"fixture\"\r\n\r\n",body.len()).into_bytes();
    r.extend_from_slice(body);
    r
}
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn real_cli_extracts_archives_resumes_and_publishes_metadata() {
    let packets: Vec<u8> = (0..3)
        .flat_map(|i| {
            let mut p = vec![i; 188];
            p[0] = 0x47;
            p
        })
        .collect();
    let payload = packets.clone();
    let srv = common::serve(move |r| {
        Some(match r.path.as_str() {
            "/robots.txt" => common::ok("User-agent: *\nAllow: /\n"),
            "/wp-json/wp/v2/media" => response(
                "application/json",
                br#"[{"id":42,"slug":"photo","source_url":"/image.png","mime_type":"image/png"}]"#,
            ),
            "/image.png" => response("image/png", b"\x89PNG\r\nfixture"),
            "/hls.m3u8" => response(
                "application/vnd.apple.mpegurl",
                b"#EXTM3U\n#EXTINF:2,\none.ts\n#EXTINF:2,\ntwo.ts\n#EXT-X-ENDLIST\n",
            ),
            "/one.ts" | "/two.ts" => response("video/mp2t", &payload),
            _ => response("text/html", b"<p>not found</p>"),
        })
    })
    .await;
    let dir = tempfile::tempdir().unwrap();
    let config = dir.path().join("config.toml");
    std::fs::write(
        &config,
        "[fetch]\nallow_private_network=true\n[policy]\ninterval_ms=0\n",
    )
    .unwrap();
    let media = dir.path().join("media");
    let media_str = media.to_str().unwrap();
    let listing = srv.url("/wp-json/wp/v2/media");
    let first = cli(
        &config,
        &[
            "media-archive",
            &listing,
            "--adapter",
            "wordpress",
            "--out",
            media_str,
        ],
    );
    let filename = first[0]["path"].as_str().unwrap();
    assert_eq!(std::fs::read(filename).unwrap(), b"\x89PNG\r\nfixture");
    let second = cli(
        &config,
        &[
            "media-archive",
            &listing,
            "--adapter",
            "wordpress",
            "--out",
            media_str,
        ],
    );
    assert_eq!(second[0]["reused"], true);
    assert_eq!(
        srv.seen().iter().filter(|r| r.path == "/image.png").count(),
        1
    );
    let stream = dir.path().join("stream");
    let url = srv.url("/hls.m3u8");
    let result = cli(
        &config,
        &["stream-download", &url, "--out", stream.to_str().unwrap()],
    );
    assert_eq!(
        std::fs::read(result[0]["path"].as_str().unwrap()).unwrap(),
        packets.repeat(2)
    );
    let repeated = cli(
        &config,
        &["stream-download", &url, "--out", stream.to_str().unwrap()],
    );
    assert_eq!(repeated[0]["reused"], true);
    assert_eq!(
        srv.seen()
            .iter()
            .filter(|r| r.path == "/one.ts" || r.path == "/two.ts")
            .count(),
        2
    );
}

#[test]
fn real_cli_exposes_fuzzy_abstention_forms_and_redacted_indicators() {
    let dir = tempfile::tempdir().unwrap();
    let config = dir.path().join("config.toml");
    std::fs::write(&config, "").unwrap();
    let old = dir.path().join("old.html");
    let new = dir.path().join("new.html");
    std::fs::write(&old, "<article class='card'>Copper kettle</article>").unwrap();
    std::fs::write(&new,"<article class='card'>Copper kettle</article><article class='card'>Copper kettle</article>").unwrap();
    let result = cli(
        &config,
        &[
            "relocate",
            new.to_str().unwrap(),
            "--original",
            old.to_str().unwrap(),
            "--selector",
            "article",
        ],
    );
    assert_eq!(result["status"], "ambiguous");
    let page = dir.path().join("form.html");
    std::fs::write(&page,"<form><input name='q'><input name='token' type='hidden' value='SECRET'></form><script>AKIAABCDEFGHIJKLMNOP</script>").unwrap();
    let result = cli(
        &config,
        &[
            "inspect",
            page.to_str().unwrap(),
            "--base-url",
            "https://example.test/",
        ],
    );
    assert_eq!(result["forms"][0]["controls"].as_array().unwrap().len(), 2);
    let text = serde_json::to_string(&result).unwrap();
    assert!(!text.contains("SECRET"));
    assert!(!text.contains("ABCDEFGHIJKLMNOP"));
    assert!(text.contains("aws_access_key_id"));
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn production_crawl_integrates_external_discovery_without_rendering() {
    let srv=common::serve(|r|Some(match r.path.as_str() {
        "/robots.txt"=>common::ok("User-agent: *\nAllow: /\n"),
        "/"=>response("text/html",b"<article><h1>Root</h1><p>Root document.</p></article><script src='/assets/main.js'></script>"),
        "/assets/main.js"=>response("application/javascript",b"import './child.js'; const ROOT='/stories/'; fetch(ROOT+'one');"),
        "/assets/child.js"=>response("application/javascript",b"location.href='/stories/two';"),
        "/stories/one"|"/stories/two"=>response("text/html",b"<article><h1>Found</h1><p>Text from an external JavaScript entry.</p></article>"),
        _=>response("text/html",b"<p>unexpected</p>"),
    })).await;
    let dir = tempfile::tempdir().unwrap();
    let config = dir.path().join("config.toml");
    std::fs::write(
        &config,
        "[fetch]\nallow_private_network=true\n[policy]\ninterval_ms=0\n",
    )
    .unwrap();
    let output = Command::new(env!("CARGO_BIN_EXE_pagesift"))
        .args([
            "--config",
            config.to_str().unwrap(),
            "crawl",
            &srv.url("/"),
            "--limit",
            "10",
            "--depth",
            "2",
            "--sitemap",
            "skip",
            "--engine",
            "http",
            "--external-scripts",
        ])
        .output()
        .unwrap();
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    let docs: Vec<serde_json::Value> = String::from_utf8(output.stdout)
        .unwrap()
        .lines()
        .map(|l| serde_json::from_str(l).unwrap())
        .collect();
    assert_eq!(docs.len(), 3);
    assert!(docs.iter().any(|d| d["url"] == srv.url("/stories/one")));
    assert!(docs.iter().any(|d| d["url"] == srv.url("/stories/two")));
    assert!(
        !docs
            .iter()
            .any(|d| d["url"].as_str().unwrap().ends_with(".js"))
    );
}
