//! HTTP-level contracts added in 0.72.8: thin-content signal, video metadata fallback,
//! feed parsing and JSON-LD pass-through.

use std::io::{Read, Write};
use std::net::{SocketAddr, TcpListener};
use std::path::Path;
use std::process::Command;

fn bin() -> &'static str {
    env!("CARGO_BIN_EXE_pagesift")
}

fn isolated(cmd: &mut Command, xdg: &Path) {
    cmd.env("XDG_CONFIG_HOME", xdg);
    cmd.env_remove("PAGESIFT_CONFIG");
}

fn serve() -> SocketAddr {
    let listener = TcpListener::bind("127.0.0.1:0").unwrap();
    let addr = listener.local_addr().unwrap();
    std::thread::spawn(move || {
        for stream in listener.incoming() {
            let Ok(mut stream) = stream else { continue };
            let mut buf = [0u8; 4096];
            let _ = stream.read(&mut buf);
            let request = String::from_utf8_lossy(&buf);
            let path = request.split_whitespace().nth(1).unwrap_or("/");
            let (content_type, body) = match path {
                "/robots.txt" => ("text/plain", "User-agent: *\nAllow: /\n".to_string()),
                "/thin" => {
                    let filler = format!("<script>var x={};</script>", "1".repeat(30_000));
                    (
                        "text/html; charset=utf-8",
                        format!(
                            "<html><head><title>Thin</title></head><body>{filler}<article><h1>Thin</h1><p>Only a few words.</p></article></body></html>"
                        ),
                    )
                }
                "/video" => (
                    "text/html; charset=utf-8",
                    r#"<html><head><title>Video - Site</title>
                       <script type="application/ld+json">{"@type":"VideoObject","name":"A Video","description":"A description.","uploadDate":"2026-01-01"}</script>
                       </head><body><div id="player"></div></body></html>"#
                        .to_string(),
                ),
                "/feed" => (
                    "application/rss+xml",
                    r#"<?xml version="1.0"?><rss version="2.0"><channel>
                       <title>Fixture Feed</title><link>https://example.com/</link>
                       <item><title>First</title><link>https://example.com/1</link>
                       <description>&lt;p&gt;Hello&lt;/p&gt;</description></item>
                       </channel></rss>"#
                        .to_string(),
                ),
                "/jsonld" => (
                    "text/html; charset=utf-8",
                    r#"<html><head><title>JSON-LD</title>
                       <script type="application/ld+json">{"@type":"Article","headline":"JSONLD_MARKER"}</script>
                       </head><body><article><p>Body text for extraction.</p></article></body></html>"#
                        .to_string(),
                ),
                _ => ("text/plain", "not found".to_string()),
            };
            let _ = write!(
                stream,
                "HTTP/1.1 200 OK\r\nContent-Type: {content_type}\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}",
                body.len()
            );
        }
    });
    addr
}

fn scrape_json(addr: SocketAddr, path: &str) -> serde_json::Value {
    let xdg = tempfile::tempdir().unwrap();
    let mut cmd = Command::new(bin());
    isolated(&mut cmd, xdg.path());
    let output = cmd
        .args([
            "scrape",
            &format!("http://{addr}{path}"),
            "--engine",
            "http",
            "--format",
            "json",
            "--allow-cidr",
            "127.0.0.1/32",
        ])
        .output()
        .unwrap();
    assert!(
        output.status.success(),
        "scrape {path} failed: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    serde_json::from_slice(&output.stdout).unwrap()
}

#[test]
fn thin_page_gets_a_thin_content_warning() {
    let addr = serve();
    let doc = scrape_json(addr, "/thin");
    let warnings = doc["data"]["metadata"]["pagesift"]["warnings"]
        .as_array()
        .unwrap()
        .iter()
        .filter_map(|w| w.as_str())
        .collect::<Vec<_>>();
    assert!(
        warnings.iter().any(|w| w.starts_with("thin_content:")),
        "{warnings:?}"
    );
}

#[test]
fn empty_video_page_uses_json_ld_metadata() {
    let addr = serve();
    let doc = scrape_json(addr, "/video");
    let markdown = doc["data"]["markdown"].as_str().unwrap();
    assert!(markdown.contains("# A Video"), "{markdown}");
    assert!(markdown.contains("A description."), "{markdown}");
    let warnings = doc["data"]["metadata"]["pagesift"]["warnings"]
        .as_array()
        .unwrap()
        .iter()
        .filter_map(|w| w.as_str())
        .collect::<Vec<_>>();
    assert!(
        warnings.iter().any(|w| w.starts_with("video_metadata:")),
        "{warnings:?}"
    );
    assert!(
        !warnings.iter().any(|w| w.starts_with("thin_content:")),
        "video fallback should not also be reported as thin: {warnings:?}"
    );
}

#[test]
fn rss_feed_is_parsed_into_items() {
    let addr = serve();
    let doc = scrape_json(addr, "/feed");
    let markdown = doc["data"]["markdown"].as_str().unwrap();
    assert!(markdown.contains("# Fixture Feed"), "{markdown}");
    assert!(
        markdown.contains("[First](https://example.com/1)"),
        "{markdown}"
    );
    assert!(markdown.contains("Hello"), "{markdown}");
    assert!(!markdown.contains("&lt;"), "{markdown}");
}

#[test]
fn json_ld_is_exposed_in_pagesift_metadata() {
    let addr = serve();
    let doc = scrape_json(addr, "/jsonld");
    assert_eq!(
        doc["data"]["metadata"]["pagesift"]["jsonLd"][0]["headline"],
        "JSONLD_MARKER"
    );
}
