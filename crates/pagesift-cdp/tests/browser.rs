//! 需要真实浏览器的集成测试。机器上没有 Chrome/Chromium（或当前是 root）时跳过。

use std::collections::HashMap;
use std::sync::Arc;
use std::time::{Duration, Instant};

use pagesift_cdp::*;
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::TcpListener;

fn browser_available() -> bool {
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

macro_rules! require_browser {
    () => {
        if !browser_available() {
            return;
        }
    };
}

/// 按子串拒绝 URL 的测试用 guard；其余全部放行（测试服务器在回环地址上）。
struct DenySubstrings(Vec<&'static str>);

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

fn allow_all() -> Arc<dyn RequestGuard> {
    Arc::new(DenySubstrings(vec![]))
}

fn config() -> PoolConfig {
    PoolConfig {
        max_browsers: 1,
        max_tabs_per_browser: 4,
        health_check_interval: Duration::ZERO,
        ..Default::default()
    }
}

fn opts() -> RenderOptions {
    RenderOptions {
        timeout: Duration::from_secs(30),
        ..Default::default()
    }
}

/// 极简的 HTTP 测试服务器。
async fn serve() -> String {
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
                let headers: HashMap<String, String> = lines
                    .filter_map(|l| l.split_once(':'))
                    .map(|(k, v)| (k.trim().to_ascii_lowercase(), v.trim().to_owned()))
                    .collect();
                let (path, query) = target.split_once('?').unwrap_or((&target, ""));
                let Some((status, ctype, extra, body)) = route(path, query, &headers, port) else {
                    // 永不响应，用来测超时。
                    tokio::time::sleep(Duration::from_secs(120)).await;
                    return;
                };
                let resp = format!(
                    "HTTP/1.1 {status}\r\nContent-Type: {ctype}\r\nContent-Length: {}\r\n{extra}Connection: close\r\n\r\n",
                    body.len()
                );
                let _ = tcp.write_all(resp.as_bytes()).await;
                let _ = tcp.write_all(body.as_bytes()).await;
            });
        }
    });
    format!("http://127.0.0.1:{port}")
}

type Response = (&'static str, &'static str, String, String);

fn html(body: &str) -> Option<Response> {
    Some((
        "200 OK",
        "text/html; charset=utf-8",
        String::new(),
        format!("<!doctype html><html><head><title>t</title></head><body>{body}</body></html>"),
    ))
}

fn route(
    path: &str,
    query: &str,
    headers: &HashMap<String, String>,
    port: u16,
) -> Option<Response> {
    match path {
        "/spa" => html(r#"<div id="app">loading</div><script>setTimeout(() => { document.getElementById('app').textContent = 'rendered by js'; }, 50);</script>"#),
        "/missing" => Some(("404 Not Found", "text/html", String::new(), "<html><body>gone</body></html>".into())),
        "/redirect" => Some(("302 Found", "text/plain", "Location: /spa\r\n".into(), String::new())),
        "/img-redirect" => Some(("302 Found", "text/plain", "Location: /secret.png\r\n".into(), String::new())),
        "/subresources" => html(
            r#"<img src="/secret-direct.png"><img src="/img-redirect"><img src="/ok.png">
               <link rel="stylesheet" href="/font.css"><p style="font-family: F">x</p>"#,
        ),
        "/font.css" => Some(("200 OK", "text/css", String::new(), "@font-face { font-family: F; src: url(/f.woff2); }".into())),
        "/iframe" => html(&format!(r#"<iframe src="http://localhost:{port}/frame"></iframe>"#)),
        "/frame" => html(r#"<img src="/secret-in-frame.png">"#),
        "/worker" => html(r#"<script>new Worker('/worker.js');</script><p>worker page</p>"#),
        "/worker.js" => Some(("200 OK", "text/javascript", String::new(), "fetch('/secret-from-worker');".into())),
        "/sw" => html(r#"<script>navigator.serviceWorker.register('/sw.js').then(() => navigator.serviceWorker.ready).then(() => { document.title = 'sw ready'; });</script>"#),
        "/sw.js" => Some(("200 OK", "text/javascript", String::new(), "self.addEventListener('install', e => e.waitUntil(fetch('/secret-from-sw').catch(() => {})));".into())),
        "/headers" => html(&format!(
            r#"<pre id="al">{}</pre><script>document.title = JSON.stringify({{w: screen.width, touch: navigator.maxTouchPoints, langs: navigator.languages, ua: navigator.userAgent}});</script>"#,
            headers.get("accept-language").cloned().unwrap_or_default()
        )),
        "/form" => html(
            r#"<input id="q" name="q"><button id="go" onclick="document.getElementById('out').textContent = 'clicked:' + document.getElementById('q').value">go</button>
               <div id="out"></div><form action="/result"><input id="f" name="v"></form>
               <div id="tall" style="height: 5000px">tall</div>
               <script>setTimeout(() => { const d = document.createElement('p'); d.id = 'late'; d.textContent = 'late'; document.body.appendChild(d); }, 300);</script>"#,
        ),
        "/result" => html(&format!("<p id=\"r\">{query}</p>")),
        "/hang" => None,
        _ if path.ends_with(".png") || path.ends_with(".woff2") || path.starts_with("/secret") => {
            Some(("404 Not Found", "text/plain", String::new(), String::new()))
        }
        _ => html("<p>default</p>"),
    }
}

fn group_gone(pgid: u32) -> bool {
    let deadline = Instant::now() + Duration::from_secs(10);
    while Instant::now() < deadline {
        let alive = nix::sys::signal::killpg(nix::unistd::Pid::from_raw(pgid as i32), None).is_ok();
        if !alive {
            #[cfg(target_os = "linux")]
            assert!(
                !std::path::Path::new(&format!("/proc/{pgid}")).exists(),
                "zombie left behind: {pgid}"
            );
            return true;
        }
        std::thread::sleep(Duration::from_millis(50));
    }
    false
}

#[tokio::test(flavor = "multi_thread")]
async fn renders_javascript_and_reports_status() {
    require_browser!();
    let base = serve().await;
    let pool = BrowserPool::new(config(), allow_all()).await.unwrap();

    let page = pool
        .render(
            &format!("{base}/redirect"),
            &RenderOptions {
                wait_for: Duration::from_millis(300),
                ..opts()
            },
        )
        .await
        .unwrap();
    assert_eq!(page.status, Some(200));
    assert_eq!(page.final_url, format!("{base}/spa"));
    assert!(page.html.contains("rendered by js"), "{}", page.html);
    assert!(page.html.starts_with("<!DOCTYPE html>"));

    let page = pool
        .render(&format!("{base}/missing"), &opts())
        .await
        .unwrap();
    assert_eq!(page.status, Some(404));
    assert!(page.html.contains("gone"));
    assert!(page.timings.total >= page.timings.navigation);
    pool.shutdown().await;
}

#[tokio::test(flavor = "multi_thread")]
async fn private_network_guard_blocks_main_document() {
    require_browser!();
    let base = serve().await;
    let pool = BrowserPool::new(config(), Arc::new(PrivateNetworkGuard))
        .await
        .unwrap();
    match pool.render(&format!("{base}/spa"), &opts()).await {
        Err(Error::Blocked { url, reason }) => {
            assert!(url.starts_with(&base));
            assert!(reason.contains("non-public"), "{reason}");
        }
        other => panic!("expected Blocked, got {other:?}"),
    }
    pool.shutdown().await;
}

#[tokio::test(flavor = "multi_thread")]
async fn guard_sees_subrequests_and_redirect_hops() {
    require_browser!();
    let base = serve().await;
    let pool = BrowserPool::new(config(), Arc::new(DenySubstrings(vec!["/secret"])))
        .await
        .unwrap();
    let page = pool
        .render(
            &format!("{base}/subresources"),
            &RenderOptions {
                wait_for: Duration::from_millis(300),
                block: BlockResources {
                    fonts: true,
                    ..Default::default()
                },
                ..opts()
            },
        )
        .await
        .unwrap();
    let blocked: Vec<(&str, &str)> = page
        .blocked_requests
        .iter()
        .map(|b| (b.url.as_str(), b.reason.as_str()))
        .collect();
    let has = |suffix: &str| blocked.iter().any(|(u, _)| u.ends_with(suffix));
    assert!(has("/secret-direct.png"), "{blocked:?}");
    // 重定向的下一跳也要过 guard。
    assert!(has("/secret.png"), "{blocked:?}");
    assert!(has("/f.woff2"), "{blocked:?}");
    assert!(!has("/ok.png"), "{blocked:?}");
    pool.shutdown().await;
}

#[tokio::test(flavor = "multi_thread")]
async fn guard_covers_cross_site_iframes_and_workers() {
    require_browser!();
    let base = serve().await;
    let pool = BrowserPool::new(config(), Arc::new(DenySubstrings(vec!["/secret"])))
        .await
        .unwrap();
    let o = RenderOptions {
        wait_for: Duration::from_millis(500),
        ..opts()
    };
    let page = pool.render(&format!("{base}/iframe"), &o).await.unwrap();
    assert!(
        page.blocked_requests
            .iter()
            .any(|b| b.url.ends_with("/secret-in-frame.png")),
        "{:?}",
        page.blocked_requests
    );
    let page = pool
        .render(
            &format!("{base}/sw"),
            &RenderOptions {
                wait_for: Duration::from_millis(1500),
                ..opts()
            },
        )
        .await;
    let page = page.unwrap();
    assert!(
        page.html.contains("<title>sw ready</title>"),
        "{}",
        page.html
    );
    assert!(
        page.blocked_requests
            .iter()
            .any(|b| b.url.ends_with("/secret-from-sw")),
        "{:?}",
        page.blocked_requests
    );
    let page = pool.render(&format!("{base}/worker"), &o).await.unwrap();
    assert!(
        page.blocked_requests
            .iter()
            .any(|b| b.url.ends_with("/secret-from-worker")),
        "{:?}",
        page.blocked_requests
    );
    pool.shutdown().await;
}

#[tokio::test(flavor = "multi_thread")]
async fn mobile_and_languages_without_changing_ua() {
    require_browser!();
    let base = serve().await;
    let pool = BrowserPool::new(config(), allow_all()).await.unwrap();
    let url = format!("{base}/headers");
    let title = |p: &RenderedPage| -> serde_json::Value {
        let t = p
            .html
            .split("<title>")
            .nth(1)
            .unwrap()
            .split("</title>")
            .next()
            .unwrap();
        serde_json::from_str(&t.replace("&quot;", "\"")).unwrap()
    };
    let desktop = pool.render(&url, &opts()).await.unwrap();
    let mobile = pool
        .render(
            &url,
            &RenderOptions {
                mobile: true,
                languages: vec!["de-DE".into(), "en".into()],
                ..opts()
            },
        )
        .await
        .unwrap();
    let (d, m) = (title(&desktop), title(&mobile));
    assert_eq!(d["w"], 1920);
    assert_eq!(m["w"], 390);
    assert_eq!(d["touch"], 0);
    assert!(m["touch"].as_u64().unwrap() > 0);
    assert_eq!(d["ua"], m["ua"], "UA must not change");
    assert_eq!(m["langs"], serde_json::json!(["de-DE", "en"]));
    assert!(
        mobile.html.contains(r#"<pre id="al">de-DE"#),
        "{}",
        mobile.html
    );
    pool.shutdown().await;
}

#[tokio::test(flavor = "multi_thread")]
async fn actions_screenshots_and_pdf() {
    require_browser!();
    let base = serve().await;
    let pool = BrowserPool::new(
        PoolConfig {
            allow_execute_javascript: true,
            ..config()
        },
        allow_all(),
    )
    .await
    .unwrap();
    let actions: Vec<Action> = serde_json::from_str(
        r##"[
            {"type": "wait", "selector": "#late"},
            {"type": "click", "selector": "#q"},
            {"type": "write", "text": "hello"},
            {"type": "click", "selector": "#go"},
            {"type": "scrape"},
            {"type": "scroll", "direction": "down"},
            {"type": "executeJavascript", "script": "window.scrollY > 0 ? document.getElementById('out').textContent : 'not scrolled'"},
            {"type": "executeJavascript", "script": "Promise.resolve({a: 1})"},
            {"type": "screenshot"},
            {"type": "screenshot", "fullPage": true, "quality": 50},
            {"type": "screenshot", "viewport": {"width": 400, "height": 300}},
            {"type": "pdf", "format": "A4"},
            {"type": "click", "selector": "#f"},
            {"type": "write", "text": "abc"},
            {"type": "press", "key": "Enter"},
            {"type": "wait", "selector": "#r"}
        ]"##,
    )
    .unwrap();
    let page = pool
        .render(
            &format!("{base}/form"),
            &RenderOptions {
                actions,
                screenshot: Some(ScreenshotOptions {
                    full_page: false,
                    quality: Some(70),
                    viewport: None,
                }),
                ..opts()
            },
        )
        .await
        .unwrap();
    let a = &page.actions;
    assert!(
        a.scrapes[0].html.contains("clicked:hello"),
        "{}",
        a.scrapes[0].html
    );
    assert_eq!(
        a.javascript_returns[0],
        JavascriptReturn {
            kind: "string".into(),
            value: "clicked:hello".into()
        }
    );
    assert_eq!(a.javascript_returns[1].value, serde_json::json!({ "a": 1 }));
    assert_eq!(a.screenshots.len(), 3);
    assert_eq!(a.screenshots[0].format, ImageFormat::Png);
    assert!(a.screenshots[0].data.starts_with(b"\x89PNG"));
    assert_eq!(a.screenshots[1].format, ImageFormat::Jpeg);
    assert!(a.screenshots[1].data.starts_with(&[0xFF, 0xD8]));
    assert_eq!(png_size(&a.screenshots[0].data), (1920, 1080));
    assert_eq!(png_size(&a.screenshots[2].data), (400, 300));
    assert!(a.pdfs[0].starts_with(b"%PDF"));
    // 按 Enter 提交了表单，跳到了 /result。
    assert_eq!(page.final_url, format!("{base}/result?v=abc"));
    assert_eq!(page.status, Some(200));
    let shot = page.screenshot.unwrap();
    assert_eq!(shot.format, ImageFormat::Jpeg);
    pool.shutdown().await;
}

fn png_size(png: &[u8]) -> (u32, u32) {
    let be = |b: &[u8]| u32::from_be_bytes([b[0], b[1], b[2], b[3]]);
    (be(&png[16..20]), be(&png[20..24]))
}

#[tokio::test(flavor = "multi_thread")]
async fn rejects_disabled_javascript_and_bad_selectors() {
    require_browser!();
    let base = serve().await;
    let pool = BrowserPool::new(config(), allow_all()).await.unwrap();
    let js = RenderOptions {
        actions: vec![Action::ExecuteJavascript { script: "1".into() }],
        ..opts()
    };
    assert!(matches!(
        pool.render(&base, &js).await,
        Err(Error::ExecuteJavascriptDisabled)
    ));
    let click = RenderOptions {
        actions: vec![
            Action::Scrape,
            Action::Click {
                selector: "#nope".into(),
                all: false,
            },
        ],
        ..opts()
    };
    match pool.render(&format!("{base}/form"), &click).await {
        Err(Error::Action { index: 1, .. }) => {}
        other => panic!("unexpected {other:?}"),
    }
    pool.shutdown().await;
}

#[tokio::test(flavor = "multi_thread")]
async fn timeout_releases_the_tab() {
    require_browser!();
    let base = serve().await;
    let pool = BrowserPool::new(
        PoolConfig {
            max_tabs_per_browser: 1,
            ..config()
        },
        allow_all(),
    )
    .await
    .unwrap();
    let start = Instant::now();
    let r = pool
        .render(
            &format!("{base}/hang"),
            &RenderOptions {
                timeout: Duration::from_secs(2),
                ..opts()
            },
        )
        .await;
    assert!(matches!(r, Err(Error::Timeout)), "{r:?}");
    assert!(start.elapsed() < Duration::from_secs(10));
    // 唯一的标签页已经归还，下一个请求能拿到。
    let page = pool.render(&format!("{base}/spa"), &opts()).await.unwrap();
    assert_eq!(page.status, Some(200));
    assert_eq!(pool.stats().await[0].tabs_in_use, 0);
    pool.shutdown().await;
}

#[tokio::test(flavor = "multi_thread")]
async fn concurrency_stays_within_limits() {
    require_browser!();
    let base = serve().await;
    let pool = BrowserPool::new(
        PoolConfig {
            max_browsers: 2,
            max_tabs_per_browser: 2,
            ..config()
        },
        allow_all(),
    )
    .await
    .unwrap();
    let max_seen = Arc::new(std::sync::Mutex::new((0usize, 0usize)));
    let watcher = {
        let (pool, max_seen) = (pool.clone(), max_seen.clone());
        tokio::spawn(async move {
            loop {
                let s = pool.stats().await;
                let tabs = s.iter().map(|b| b.tabs_in_use).max().unwrap_or(0);
                {
                    let mut m = max_seen.lock().unwrap();
                    m.0 = m.0.max(s.len());
                    m.1 = m.1.max(tabs);
                }
                tokio::time::sleep(Duration::from_millis(5)).await;
            }
        })
    };
    let jobs: Vec<_> = (0..10)
        .map(|_| {
            let (pool, url) = (pool.clone(), format!("{base}/spa"));
            tokio::spawn(async move {
                pool.render(
                    &url,
                    &RenderOptions {
                        wait_for: Duration::from_millis(200),
                        ..opts()
                    },
                )
                .await
            })
        })
        .collect();
    for j in jobs {
        assert_eq!(j.await.unwrap().unwrap().status, Some(200));
    }
    watcher.abort();
    let (browsers, tabs) = *max_seen.lock().unwrap();
    assert!(
        browsers <= 2 && tabs <= 2,
        "browsers={browsers} tabs={tabs}"
    );
    pool.shutdown().await;
}

#[tokio::test(flavor = "multi_thread")]
async fn recycle_and_shutdown_leave_no_processes() {
    require_browser!();
    let base = serve().await;
    let pool = BrowserPool::new(
        PoolConfig {
            recycle_after_pages: 2,
            ..config()
        },
        allow_all(),
    )
    .await
    .unwrap();
    let url = format!("{base}/spa");
    pool.render(&url, &opts()).await.unwrap();
    let first = pool.stats().await[0].pid.unwrap();
    pool.render(&url, &opts()).await.unwrap();
    // 第二页之后到了回收上限，浏览器已经被整组清理。
    assert!(pool.stats().await.is_empty());
    assert!(
        group_gone(first),
        "browser {first} still has processes after recycling"
    );

    pool.render(&url, &opts()).await.unwrap();
    let second = pool.stats().await[0].pid.unwrap();
    assert_ne!(first, second);
    pool.shutdown().await;
    assert!(
        group_gone(second),
        "browser {second} still has processes after shutdown"
    );
    assert!(matches!(
        pool.render(&url, &opts()).await,
        Err(Error::PoolClosed)
    ));
}

#[tokio::test(flavor = "multi_thread")]
async fn restarts_after_crash() {
    require_browser!();
    let base = serve().await;
    let pool = BrowserPool::new(
        PoolConfig {
            min_browsers: 1,
            ..config()
        },
        allow_all(),
    )
    .await
    .unwrap();
    let stats = pool.stats().await;
    assert_eq!(stats.len(), 1, "min_browsers should prelaunch");
    let pid = stats[0].pid.unwrap();
    nix::sys::signal::kill(
        nix::unistd::Pid::from_raw(pid as i32),
        nix::sys::signal::Signal::SIGKILL,
    )
    .unwrap();
    tokio::time::sleep(Duration::from_millis(300)).await;
    pool.health_check().await;
    let stats = pool.stats().await;
    assert_eq!(stats.len(), 1);
    assert_ne!(stats[0].pid.unwrap(), pid);
    assert!(group_gone(pid));
    let page = pool.render(&format!("{base}/spa"), &opts()).await.unwrap();
    assert_eq!(page.status, Some(200));
    pool.shutdown().await;
}

#[tokio::test(flavor = "multi_thread")]
async fn connects_to_remote_endpoint() {
    require_browser!();
    let base = serve().await;
    // 用本地池起一个浏览器，再让第二个池通过 http:// 端点连过去。
    let local = BrowserPool::new(
        PoolConfig {
            min_browsers: 1,
            extra_args: vec![],
            ..config()
        },
        allow_all(),
    )
    .await
    .unwrap();
    let Some(port) = devtools_port(local.stats().await[0].pid.unwrap()) else {
        eprintln!("skipping remote test: cannot find DevTools port");
        local.shutdown().await;
        return;
    };
    let remote = BrowserPool::new(
        PoolConfig {
            source: BrowserSource::Remote(format!("http://127.0.0.1:{port}")),
            ..config()
        },
        allow_all(),
    )
    .await
    .unwrap();
    let page = remote
        .render(
            &format!("{base}/spa"),
            &RenderOptions {
                wait_for: Duration::from_millis(200),
                ..opts()
            },
        )
        .await
        .unwrap();
    assert!(page.html.contains("rendered by js"));
    remote.shutdown().await;
    // 远程模式不负责也不能杀掉对方的浏览器。
    let pid = local.stats().await[0].pid.unwrap();
    assert!(nix::sys::signal::kill(nix::unistd::Pid::from_raw(pid as i32), None).is_ok());
    local.shutdown().await;
}

/// 从浏览器命令行里的 `--user-data-dir` 读 `DevToolsActivePort`。
fn devtools_port(pid: u32) -> Option<u16> {
    let out = std::process::Command::new("ps")
        .args(["-o", "args=", "-p", &pid.to_string()])
        .output()
        .ok()?;
    let args = String::from_utf8_lossy(&out.stdout).into_owned();
    let dir = args
        .split_whitespace()
        .find_map(|a| a.strip_prefix("--user-data-dir="))?;
    let text =
        std::fs::read_to_string(std::path::Path::new(dir).join("DevToolsActivePort")).ok()?;
    text.lines().next()?.trim().parse().ok()
}
