//! CLI-level contracts added in 0.72.8:
//! - `--allow-cidr` on every command that performs outbound fetches
//! - honest exit codes for `crawl` / `map` when the seed is unreachable
//! - default config discovery, `PAGESIFT_CONFIG=`, and `doctor` reporting

use std::io::{Read, Write};
use std::net::{SocketAddr, TcpListener};
use std::path::Path;
use std::process::{Command, Output};

fn bin() -> &'static str {
    env!("CARGO_BIN_EXE_pagesift")
}

/// Keep tests independent from a developer's real `~/.config/pagesift`.
fn isolated(cmd: &mut Command, xdg: &Path) {
    cmd.env("XDG_CONFIG_HOME", xdg);
    cmd.env_remove("PAGESIFT_CONFIG");
}

fn run(mut cmd: Command) -> Output {
    cmd.output().unwrap()
}

/// Tiny blocking HTTP fixture: `/`, `/a.html`, `/b.html`, robots.
fn serve_pages() -> SocketAddr {
    let listener = TcpListener::bind("127.0.0.1:0").unwrap();
    let addr = listener.local_addr().unwrap();
    std::thread::spawn(move || {
        for stream in listener.incoming() {
            let Ok(mut stream) = stream else { continue };
            let mut buf = [0u8; 4096];
            let _ = stream.read(&mut buf);
            let request = String::from_utf8_lossy(&buf);
            let path = request.split_whitespace().nth(1).unwrap_or("/");
            let (status, body) = match path {
                "/robots.txt" => ("200 OK", "User-agent: *\nAllow: /\n".to_string()),
                "/sitemap.xml" => ("404 Not Found", "no sitemap".to_string()),
                "/" => (
                    "200 OK",
                    "<!doctype html><html><body><h1>home</h1><p>fixture page with enough text for extraction to keep it.</p><a href=\"/a.html\">a</a><a href=\"/b.html\">b</a></body></html>".to_string(),
                ),
                "/a.html" | "/b.html" => (
                    "200 OK",
                    format!(
                        "<!doctype html><html><body><h1>{path}</h1><p>fixture page with enough text for extraction to keep it.</p></body></html>"
                    ),
                ),
                _ => ("404 Not Found", "not found".to_string()),
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

#[test]
fn every_fetch_command_exposes_allow_cidr() {
    for command in [
        "scrape",
        "crawl",
        "map",
        "download",
        "session-fetch",
        "collect",
        "serve",
    ] {
        let output = Command::new(bin())
            .args([command, "--help"])
            .output()
            .unwrap();
        assert!(output.status.success(), "{command} --help failed");
        let help = String::from_utf8_lossy(&output.stdout);
        assert!(
            help.contains("--allow-cidr"),
            "{command} --help does not list --allow-cidr:\n{help}"
        );
    }
}

#[test]
fn invalid_cidr_is_rejected_by_the_parser() {
    let output = Command::new(bin())
        .args([
            "scrape",
            "--allow-cidr",
            "not-a-cidr",
            "http://127.0.0.1:9/",
        ])
        .output()
        .unwrap();
    assert_eq!(output.status.code(), Some(2));
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(stderr.contains("invalid CIDR"), "{stderr}");
}

#[test]
fn crawl_and_map_reach_a_private_origin_with_allow_cidr() {
    let addr = serve_pages();
    let root = format!("http://{addr}/");
    let xdg = tempfile::tempdir().unwrap();

    let mut crawl = Command::new(bin());
    isolated(&mut crawl, xdg.path());
    let crawl = run({
        crawl.args([
            "crawl",
            &root,
            "--engine",
            "http",
            "--sitemap",
            "skip",
            "--limit",
            "5",
            "--allow-cidr",
            "127.0.0.1/32",
        ]);
        crawl
    });
    assert!(
        crawl.status.success(),
        "crawl failed: {}",
        String::from_utf8_lossy(&crawl.stderr)
    );
    let lines = crawl
        .stdout
        .split(|byte| *byte == b'\n')
        .filter(|line| !line.is_empty())
        .count();
    assert_eq!(lines, 3, "crawl pages");

    let mut map = Command::new(bin());
    isolated(&mut map, xdg.path());
    let map = run({
        map.args([
            "map",
            &root,
            "--sitemap",
            "skip",
            "--limit",
            "10",
            "--allow-cidr",
            "127.0.0.1/32",
        ]);
        map
    });
    assert!(
        map.status.success(),
        "map failed: {}",
        String::from_utf8_lossy(&map.stderr)
    );
    let urls: Vec<_> = String::from_utf8_lossy(&map.stdout)
        .lines()
        .filter(|line| !line.is_empty())
        .map(str::to_owned)
        .collect();
    assert_eq!(urls.len(), 3, "map URLs: {urls:?}");
}

#[test]
fn crawl_with_a_blocked_seed_exits_1() {
    let xdg = tempfile::tempdir().unwrap();
    let mut cmd = Command::new(bin());
    isolated(&mut cmd, xdg.path());
    let output = cmd
        .args([
            "crawl",
            "http://127.0.0.1:9/",
            "--engine",
            "http",
            "--limit",
            "1",
        ])
        .output()
        .unwrap();
    assert_eq!(
        output.status.code(),
        Some(1),
        "stderr: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    assert!(
        String::from_utf8_lossy(&output.stderr).contains("SSRF_BLOCKED"),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
}

#[test]
fn map_with_a_blocked_seed_reports_an_error_instead_of_the_seed() {
    let xdg = tempfile::tempdir().unwrap();
    let mut cmd = Command::new(bin());
    isolated(&mut cmd, xdg.path());
    let output = cmd
        .args(["map", "http://127.0.0.1:9/", "--sitemap", "skip"])
        .output()
        .unwrap();
    assert_eq!(
        output.status.code(),
        Some(1),
        "stderr: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    assert!(
        String::from_utf8_lossy(&output.stderr).contains("SSRF_BLOCKED"),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    assert!(
        output.stdout.is_empty(),
        "map printed a fake result: {}",
        String::from_utf8_lossy(&output.stdout)
    );
}

#[test]
fn default_config_file_is_loaded_and_doctor_reports_it() {
    let xdg = tempfile::tempdir().unwrap();
    let dir = xdg.path().join("pagesift");
    std::fs::create_dir_all(&dir).unwrap();
    let config = dir.join("pagesift.toml");
    std::fs::write(&config, "[server]\nport = 3999\n").unwrap();

    let mut cmd = Command::new(bin());
    isolated(&mut cmd, xdg.path());
    let output = cmd.arg("doctor").output().unwrap();
    let stdout = String::from_utf8_lossy(&output.stdout);
    assert!(
        stdout.contains(&config.display().to_string()),
        "doctor did not report the loaded config:\n{stdout}"
    );
}

#[test]
fn doctor_reports_an_explicit_config_path() {
    let dir = tempfile::tempdir().unwrap();
    let config = dir.path().join("custom.toml");
    std::fs::write(&config, "[server]\nport = 3999\n").unwrap();

    let output = Command::new(bin())
        .args(["--config", config.to_str().unwrap(), "doctor"])
        .output()
        .unwrap();
    let stdout = String::from_utf8_lossy(&output.stdout);
    assert!(
        stdout.contains(&config.display().to_string()),
        "doctor did not report the explicit config:\n{stdout}"
    );
}

#[test]
fn empty_pagesift_config_disables_default_files() {
    let xdg = tempfile::tempdir().unwrap();
    let dir = xdg.path().join("pagesift");
    std::fs::create_dir_all(&dir).unwrap();
    std::fs::write(dir.join("pagesift.toml"), "[server]\nport = 3999\n").unwrap();

    let output = Command::new(bin())
        .env("XDG_CONFIG_HOME", xdg.path())
        .env("PAGESIFT_CONFIG", "")
        .arg("doctor")
        .output()
        .unwrap();
    let stdout = String::from_utf8_lossy(&output.stdout);
    assert!(
        stdout.contains("config files disabled"),
        "doctor did not report the disable switch:\n{stdout}"
    );
}
