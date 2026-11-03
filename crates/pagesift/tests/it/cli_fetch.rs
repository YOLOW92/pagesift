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
    serve_pages_with_robots("User-agent: *\nAllow: /\n")
}

fn serve_pages_with_robots(robots: &'static str) -> SocketAddr {
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
                "/robots.txt" => ("200 OK", robots.to_string()),
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

/// 共享服务的配置文件里开了审计日志时，`doctor` 的自检抓取不能写进去（每次都是一条误报）。
#[test]
fn doctor_does_not_write_the_audit_log() {
    let dir = tempfile::tempdir().unwrap();
    let log = dir.path().join("audit.jsonl");
    let config = dir.path().join("audit.toml");
    std::fs::write(
        &config,
        format!("[audit]\npath = {:?}\n", log.to_str().unwrap()),
    )
    .unwrap();

    let output = Command::new(bin())
        .args(["--config", config.to_str().unwrap(), "doctor"])
        .output()
        .unwrap();
    let stdout = String::from_utf8_lossy(&output.stdout);
    assert!(stdout.contains("network"), "doctor did not run:\n{stdout}");
    let written = std::fs::read_to_string(&log).unwrap_or_default();
    assert!(written.is_empty(), "doctor wrote the audit log:\n{written}");
}

/// `--out` 目录里除了每页一行的 index.jsonl，还有 summary.json：停止原因和覆盖度不只打在 stderr。
/// 没有数据风险的页面，index 行里没有 warning。
#[test]
fn crawl_out_writes_a_summary_next_to_the_index() {
    let addr = serve_pages();
    let root = format!("http://{addr}/");
    let xdg = tempfile::tempdir().unwrap();
    let out = tempfile::tempdir().unwrap();

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
            "--query",
            "fixture page",
            "--adaptive",
            "--allow-cidr",
            "127.0.0.1/32",
            "--out",
        ]);
        crawl.arg(out.path());
        crawl
    });
    assert!(
        crawl.status.success(),
        "crawl failed: {}",
        String::from_utf8_lossy(&crawl.stderr)
    );
    let summary: serde_json::Value =
        serde_json::from_slice(&std::fs::read(out.path().join("summary.json")).unwrap()).unwrap();
    assert!(summary["stopReason"].is_string(), "{summary}");
    assert!(summary["coverage"].is_number(), "{summary}");
    assert_eq!(summary["pages"], 3, "{summary}");
    let index = std::fs::read_to_string(out.path().join("index.jsonl")).unwrap();
    let rows: Vec<serde_json::Value> = index
        .lines()
        .map(|l| serde_json::from_str(l).unwrap())
        .collect();
    assert_eq!(rows.len(), 3);
    assert!(rows.iter().all(|r| r.get("warning").is_none()), "{index}");
}

/// 取值固定的参数拼错了要报错，不能悄悄按默认值干活（`--engine potato` 以前照常按 auto 抓取）。
#[test]
fn misspelled_choices_are_rejected() {
    for args in [
        ["scrape", "http://127.0.0.1:9/", "--engine", "potato"],
        ["scrape", "http://127.0.0.1:9/", "--format", "yaml"],
        ["crawl", "http://127.0.0.1:9/", "--engine", "chrome"],
        ["crawl", "http://127.0.0.1:9/", "--sitemap", "never"],
        ["map", "http://127.0.0.1:9/", "--sitemap", "no"],
    ] {
        let output = Command::new(bin()).args(args).output().unwrap();
        assert!(!output.status.success(), "{args:?} was accepted");
        let stderr = String::from_utf8_lossy(&output.stderr);
        assert!(stderr.contains("invalid value"), "{args:?}: {stderr}");
    }
}

/// crawl 和 map 也有 --ignore-robots：命令行用户自己就是管理员，参数本身就是许可，
/// 不用再改配置（0.80.5 以前命令行里没有办法，只能经 HTTP API 并在配置里放行）。
#[test]
fn crawl_and_map_can_ignore_robots_on_the_command_line() {
    let addr = serve_pages_with_robots("User-agent: *\nDisallow: /\n");
    let root = format!("http://{addr}/");
    let xdg = tempfile::tempdir().unwrap();
    let crawl = |extra: &[&str]| {
        let mut cmd = Command::new(bin());
        isolated(&mut cmd, xdg.path());
        cmd.args([
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
        ])
        .args(extra);
        run(cmd)
    };
    let pages = |o: &Output| {
        o.stdout
            .split(|b| *b == b'\n')
            .filter(|l| !l.is_empty())
            .count()
    };
    // robots.txt 不许抓：默认照办
    let respected = crawl(&[]);
    assert_eq!(
        pages(&respected),
        0,
        "{}",
        String::from_utf8_lossy(&respected.stderr)
    );
    let ignored = crawl(&["--ignore-robots"]);
    assert!(
        ignored.status.success(),
        "{}",
        String::from_utf8_lossy(&ignored.stderr)
    );
    assert_eq!(pages(&ignored), 3);

    let mut map = Command::new(bin());
    isolated(&mut map, xdg.path());
    map.args([
        "map",
        &root,
        "--sitemap",
        "skip",
        "--allow-cidr",
        "127.0.0.1/32",
        "--ignore-robots",
    ]);
    let map = run(map);
    assert!(
        map.status.success(),
        "{}",
        String::from_utf8_lossy(&map.stderr)
    );
    assert_eq!(String::from_utf8_lossy(&map.stdout).lines().count(), 3);
}

/// doctor 真的启动内核：文件在、但一启动就崩的内核要报出来（0.80.5 只看文件在不在，报整条链 ok）。
#[test]
fn doctor_reports_an_engine_that_does_not_start() {
    if pagesift_cdp::running_as_root() {
        // root 下 doctor 直接报"不能以 root 运行浏览器"，不做启动检查
        return;
    }
    let dir = tempfile::tempdir().unwrap();
    let stub = dir.path().join("lightpanda");
    std::fs::write(&stub, "#!/bin/sh\necho 'crashed at startup' >&2\nexit 1\n").unwrap();
    use std::os::unix::fs::PermissionsExt;
    std::fs::set_permissions(&stub, std::fs::Permissions::from_mode(0o755)).unwrap();
    let xdg = tempfile::tempdir().unwrap();
    let mut doctor = Command::new(bin());
    isolated(&mut doctor, xdg.path());
    let path = format!(
        "{}:{}",
        dir.path().display(),
        std::env::var("PATH").unwrap_or_default()
    );
    doctor.env("PATH", path).arg("doctor");
    let out = String::from_utf8_lossy(&run(doctor).stdout).into_owned();
    let line = out
        .lines()
        .find(|l| l.contains("engine lightpanda"))
        .unwrap_or_else(|| panic!("no launch check for the stub engine:\n{out}"));
    assert!(line.contains("does not start"), "{line}");
    assert!(
        line.starts_with("WARN") || line.starts_with("FAIL"),
        "{line}"
    );
}
