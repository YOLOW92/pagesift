//! WARC 往返：`scrape --warc` / `crawl --warc` 导出，再用 `scrape x.warc`、`crawl --from-warc` 读回，
//! Markdown 和直接抓取逐字节一致（SHA-256 相同）。

use std::io::{Read, Write};
use std::net::{SocketAddr, TcpListener};
use std::process::Command;

fn bin() -> &'static str {
    env!("CARGO_BIN_EXE_pagesift")
}

fn serve() -> SocketAddr {
    let listener = TcpListener::bind("127.0.0.1:0").unwrap();
    let addr = listener.local_addr().unwrap();
    std::thread::spawn(move || {
        for stream in listener.incoming() {
            let Ok(mut stream) = stream else { continue };
            let mut buf = [0u8; 4096];
            let _ = stream.read(&mut buf);
            let req = String::from_utf8_lossy(&buf);
            let path = req.split_whitespace().nth(1).unwrap_or("/").to_string();
            let (status, body) = match path.as_str() {
                "/robots.txt" => ("200 OK", "User-agent: *\nAllow: /\n".to_string()),
                "/" => ("200 OK", "<!doctype html><html><head><title>Home</title></head><body><main><h1>Archive home</h1>\
                    <p>A fixture page with <a href=\"/a.html\">a relative link</a> and enough text to be kept by extraction.</p>\
                    <ul><li>one</li><li>two</li></ul><a href=\"/b.html\">b</a></main></body></html>".to_string()),
                "/a.html" | "/b.html" => ("200 OK", format!("<!doctype html><html><body><article><h1>Page {path}</h1>\
                    <p>Second-level fixture page with a paragraph long enough for the extractor.</p></article></body></html>")),
                _ => ("404 Not Found", "nope".to_string()),
            };
            let _ = write!(
                stream,
                "HTTP/1.1 {status}\r\nContent-Type: text/html; charset=utf-8\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}",
                body.len()
            );
        }
    });
    addr
}

fn pagesift(xdg: &std::path::Path, args: &[&str]) -> std::process::Output {
    Command::new(bin())
        .env("XDG_CONFIG_HOME", xdg)
        .env_remove("PAGESIFT_CONFIG")
        .args(args)
        .output()
        .unwrap()
}

#[test]
fn scrape_warc_round_trip_keeps_the_markdown() {
    let addr = serve();
    let url = format!("http://{addr}/");
    let dir = tempfile::tempdir().unwrap();
    for name in ["page.warc", "page.warc.gz"] {
        let warc = dir.path().join(name);
        let direct = pagesift(
            dir.path(),
            &[
                "scrape",
                &url,
                "--engine",
                "http",
                "--allow-cidr",
                "127.0.0.1/32",
                "--warc",
                warc.to_str().unwrap(),
            ],
        );
        assert!(
            direct.status.success(),
            "{}",
            String::from_utf8_lossy(&direct.stderr)
        );
        let back = pagesift(dir.path(), &["scrape", warc.to_str().unwrap()]);
        assert!(
            back.status.success(),
            "{}",
            String::from_utf8_lossy(&back.stderr)
        );
        assert!(!direct.stdout.is_empty());
        assert_eq!(
            String::from_utf8_lossy(&back.stdout),
            String::from_utf8_lossy(&direct.stdout),
            "{name}"
        );
    }
}

#[test]
fn crawl_from_warc_converts_archived_pages_offline() {
    let addr = serve();
    let url = format!("http://{addr}/");
    let dir = tempfile::tempdir().unwrap();
    let warc = dir.path().join("site.warc.gz");
    let crawl = pagesift(
        dir.path(),
        &[
            "crawl",
            &url,
            "--engine",
            "http",
            "--allow-cidr",
            "127.0.0.1/32",
            "--warc",
            warc.to_str().unwrap(),
        ],
    );
    assert!(
        crawl.status.success(),
        "{}",
        String::from_utf8_lossy(&crawl.stderr)
    );
    let live: Vec<serde_json::Value> = String::from_utf8_lossy(&crawl.stdout)
        .lines()
        .map(|l| serde_json::from_str(l).unwrap())
        .collect();
    assert_eq!(live.len(), 3);
    // 不联网：给一个连不上的起始网址主机也照样转换（只用来定范围）
    let offline = pagesift(
        dir.path(),
        &[
            "crawl",
            &url,
            "--from-warc",
            warc.to_str().unwrap(),
            "--exclude",
            "^/b",
        ],
    );
    assert!(
        offline.status.success(),
        "{}",
        String::from_utf8_lossy(&offline.stderr)
    );
    let pages: Vec<serde_json::Value> = String::from_utf8_lossy(&offline.stdout)
        .lines()
        .map(|l| serde_json::from_str(l).unwrap())
        .collect();
    assert_eq!(pages.len(), 2, "{pages:?}");
    for p in &pages {
        let src = p["url"].as_str().unwrap();
        let twin = live.iter().find(|l| l["url"] == src).unwrap();
        assert_eq!(p["markdown"], twin["markdown"], "{src}");
    }
}
