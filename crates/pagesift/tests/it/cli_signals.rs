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

fn nested_eml(depth: usize) -> Vec<u8> {
    let mut current = b"From: leaf@example.com\r\nTo: dest@example.com\r\nSubject: Leaf\r\nContent-Type: text/plain; charset=utf-8\r\n\r\nDeep body\r\n".to_vec();
    for level in (0..depth).rev() {
        let boundary = format!("b_{level}");
        let mut msg = format!(
            "From: s{level}@example.com\r\nTo: r{level}@example.com\r\nSubject: Level {level}\r\nMIME-Version: 1.0\r\nContent-Type: multipart/mixed; boundary=\"{boundary}\"\r\n\r\n--{boundary}\r\nContent-Type: text/plain; charset=utf-8\r\n\r\nMessage level {level}\r\n--{boundary}\r\nContent-Type: message/rfc822; name=\"inner{level}.eml\"\r\nContent-Disposition: attachment; filename=\"inner{level}.eml\"\r\n\r\n"
        )
        .into_bytes();
        msg.extend_from_slice(&current);
        msg.extend_from_slice(format!("\r\n--{boundary}--\r\n").as_bytes());
        current = msg;
    }
    current
}

#[test]
fn eml_nesting_limit_warns_on_markdown_stderr_and_json_top_level() {
    let dir = tempfile::tempdir().unwrap();
    let eml_path = dir.path().join("deep.eml");
    std::fs::write(&eml_path, nested_eml(5)).unwrap();

    let mut md_cmd = Command::new(bin());
    isolated(&mut md_cmd, dir.path());
    let md_out = md_cmd
        .args(["scrape", eml_path.to_str().unwrap()])
        .output()
        .unwrap();
    assert!(
        md_out.status.success(),
        "stderr: {}",
        String::from_utf8_lossy(&md_out.stderr)
    );
    let md_err = String::from_utf8_lossy(&md_out.stderr);
    assert!(
        md_err.contains("warning:") && md_err.contains("nesting limit"),
        "markdown CLI mode must emit nesting limit warning on stderr: {md_err}"
    );

    let mut json_cmd = Command::new(bin());
    isolated(&mut json_cmd, dir.path());
    let json_out = json_cmd
        .args(["scrape", eml_path.to_str().unwrap(), "--format", "json"])
        .output()
        .unwrap();
    assert!(
        json_out.status.success(),
        "stderr: {}",
        String::from_utf8_lossy(&json_out.stderr)
    );
    let doc: serde_json::Value = serde_json::from_slice(&json_out.stdout).unwrap();
    let top_warning = doc["warning"].as_str().unwrap_or("");
    assert!(
        top_warning.contains("nesting limit"),
        "CLI JSON output must expose top-level warning: {doc}"
    );
    let ps_warnings = doc["data"]["metadata"]["pagesift"]["warnings"].to_string();
    assert!(
        ps_warnings.contains("nesting limit"),
        "CLI JSON metadata.pagesift.warnings missing nesting warning: {ps_warnings}"
    );
}

#[test]
fn corrupt_supported_local_files_report_corrupt_not_unsupported_type() {
    let dir = tempfile::tempdir().unwrap();
    let epub = dir.path().join("broken.epub");
    let sqlite = dir.path().join("plain.sqlite");
    std::fs::write(&epub, b"not-a-valid-epub-zip-header-bytes").unwrap();
    std::fs::write(&sqlite, b"just plain text disguised as sqlite").unwrap();

    for path in [&epub, &sqlite] {
        let mut cmd = Command::new(bin());
        isolated(&mut cmd, dir.path());
        let out = cmd
            .args(["scrape", path.to_str().unwrap()])
            .output()
            .unwrap();
        assert!(!out.status.success(), "{path:?} unexpectedly succeeded");
        let stderr = String::from_utf8_lossy(&out.stderr).to_lowercase();
        assert!(
            stderr.contains("corrupt"),
            "{path:?}: expected corrupt error, got: {stderr}"
        );
        assert!(
            !stderr.contains("unsupported_option") && !stderr.contains("unsupported content type"),
            "{path:?}: must not claim unsupported content type: {stderr}"
        );
    }
}
