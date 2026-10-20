//! 集成测试共用：本地 HTTP 服务器、放行一切的 guard、进程清理检查。

#![allow(dead_code)]

use std::sync::Arc;
use std::time::{Duration, Instant};

use pagesift_cdp::*;
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::TcpListener;

pub fn browser_available() -> bool {
    if detect_browser().is_none() {
        eprintln!("skipping: no Chrome/Chromium found");
        return false;
    }
    if nix::unistd::geteuid().is_root() {
        eprintln!("skipping: running as root");
        return false;
    }
    true
}

#[macro_export]
macro_rules! require_browser {
    () => {
        if !common::browser_available() {
            return;
        }
    };
}

pub struct Req {
    pub path: String,
    pub query: String,
    /// 请求的 Host 头里的主机名（`localhost` 或 `127.0.0.1`）。
    pub host: String,
    pub port: u16,
}

impl Req {
    pub fn param(&self, key: &str) -> Option<&str> {
        self.query
            .split('&')
            .filter_map(|kv| kv.split_once('='))
            .find(|(k, _)| *k == key)
            .map(|(_, v)| v)
    }
}

pub struct Resp {
    pub status: &'static str,
    pub ctype: &'static str,
    pub body: String,
    pub delay: Duration,
}

pub fn html(body: &str) -> Resp {
    Resp {
        status: "200 OK",
        ctype: "text/html; charset=utf-8",
        body: format!(
            "<!doctype html><html><head><title>t</title></head><body>{body}</body></html>"
        ),
        delay: Duration::ZERO,
    }
}

pub fn js(body: &str) -> Resp {
    Resp {
        ctype: "text/javascript",
        ..html_raw(body)
    }
}

pub fn html_raw(body: &str) -> Resp {
    Resp {
        status: "200 OK",
        ctype: "text/html; charset=utf-8",
        body: body.to_owned(),
        delay: Duration::ZERO,
    }
}

pub fn not_found() -> Resp {
    Resp {
        status: "404 Not Found",
        ctype: "text/plain",
        body: String::new(),
        delay: Duration::ZERO,
    }
}

impl Resp {
    pub fn delayed(mut self, ms: u64) -> Resp {
        self.delay = Duration::from_millis(ms);
        self
    }
}

/// 启动测试服务器，返回 `http://127.0.0.1:port`。
pub async fn serve(route: fn(&Req) -> Resp) -> (String, u16) {
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let port = listener.local_addr().unwrap().port();
    tokio::spawn(async move {
        loop {
            let Ok((mut tcp, _)) = listener.accept().await else {
                return;
            };
            tokio::spawn(async move {
                let mut buf = Vec::new();
                let mut chunk = [0u8; 4096];
                while !buf.windows(4).any(|w| w == b"\r\n\r\n") {
                    match tcp.read(&mut chunk).await {
                        Ok(0) | Err(_) => return,
                        Ok(n) => buf.extend_from_slice(&chunk[..n]),
                    }
                }
                let head = String::from_utf8_lossy(&buf).into_owned();
                let mut lines = head.lines();
                let target = lines
                    .next()
                    .unwrap_or_default()
                    .split(' ')
                    .nth(1)
                    .unwrap_or("/")
                    .to_owned();
                let host = lines
                    .filter_map(|l| l.split_once(':'))
                    .find(|(k, _)| k.eq_ignore_ascii_case("host"))
                    .map(|(_, v)| v.trim().split(':').next().unwrap_or_default().to_owned())
                    .unwrap_or_default();
                let (path, query) = target.split_once('?').unwrap_or((&target, ""));
                let r = route(&Req {
                    path: path.to_owned(),
                    query: query.to_owned(),
                    host,
                    port,
                });
                if !r.delay.is_zero() {
                    tokio::time::sleep(r.delay).await;
                }
                let resp = format!(
                    "HTTP/1.1 {}\r\nContent-Type: {}\r\nContent-Length: {}\r\nCache-Control: no-store\r\nConnection: close\r\n\r\n",
                    r.status,
                    r.ctype,
                    r.body.len()
                );
                let _ = tcp.write_all(resp.as_bytes()).await;
                let _ = tcp.write_all(r.body.as_bytes()).await;
            });
        }
    });
    (format!("http://127.0.0.1:{port}"), port)
}

struct AllowAll;

impl RequestGuard for AllowAll {
    fn check<'a>(&'a self, _req: &'a SubRequest) -> BoxFuture<'a, Verdict> {
        Box::pin(async { Verdict::Allow })
    }
}

/// 按子串拒绝 URL，其余放行（测试服务器在回环地址上）。
pub struct DenySubstrings(pub Vec<&'static str>);

impl RequestGuard for DenySubstrings {
    fn check<'a>(&'a self, req: &'a SubRequest) -> BoxFuture<'a, Verdict> {
        Box::pin(async move {
            match self.0.iter().find(|s| req.url.contains(*s)) {
                Some(s) => Verdict::Block(format!("matched {s}")),
                None => Verdict::Allow,
            }
        })
    }
}

pub fn allow_all() -> Arc<dyn RequestGuard> {
    Arc::new(AllowAll)
}

pub fn config() -> PoolConfig {
    PoolConfig {
        max_browsers: 1,
        max_tabs_per_browser: 4,
        health_check_interval: Duration::ZERO,
        ..Default::default()
    }
}

pub fn opts() -> RenderOptions {
    RenderOptions {
        timeout: Duration::from_secs(30),
        ..Default::default()
    }
}

/// 关闭池并确认浏览器整个进程组都退出了。
pub async fn shutdown(pool: BrowserPool) {
    let pids: Vec<u32> = pool.stats().await.iter().filter_map(|s| s.pid).collect();
    pool.shutdown().await;
    for pid in pids {
        assert!(group_gone(pid), "browser {pid} left processes behind");
    }
}

pub fn group_gone(pgid: u32) -> bool {
    let deadline = Instant::now() + Duration::from_secs(10);
    while Instant::now() < deadline {
        if nix::sys::signal::killpg(nix::unistd::Pid::from_raw(pgid as i32), None).is_err() {
            return true;
        }
        std::thread::sleep(Duration::from_millis(50));
    }
    false
}

pub fn png_size(png: &[u8]) -> (u32, u32) {
    let be = |b: &[u8]| u32::from_be_bytes([b[0], b[1], b[2], b[3]]);
    (be(&png[16..20]), be(&png[20..24]))
}
