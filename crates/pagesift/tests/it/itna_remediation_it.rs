//! 1.6.1 ITNA remediation integration tests covering the ITNA audit matrix
//! (`ITNA-01`, `ITNA-03`..`ITNA-07`, `ITNA-09`, `ITNA-10`).
//! (`ITNA-02` is verified in `context_remediation_it.rs`; `ITNA-08` is unit-tested in `pagesift-policy`.)

use std::net::SocketAddr;
use std::path::PathBuf;
use std::sync::Arc;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::time::{Duration, Instant};

use axum::Router;
use axum::http::{HeaderMap, StatusCode};
use axum::response::{Html, Redirect};
use axum::routing::get;
use pagesift::{Scraper, ScraperConfig};
use pagesift_api::ErrorCode;
use pagesift_api::types::{Format, ScrapeOptions};
use pagesift_cdp::{BrowserSource, EngineKind};
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use url::Url;

fn runtime() -> tokio::runtime::Runtime {
    tokio::runtime::Builder::new_multi_thread()
        .worker_threads(2)
        .enable_all()
        .build()
        .unwrap()
}

fn itna_scraper_cfg() -> ScraperConfig {
    let mut cfg = ScraperConfig {
        itna: true,
        ..Default::default()
    };
    cfg.fetcher.env_proxy = false;
    cfg.fetcher.ssrf.allow_private_network = true;
    cfg.policy.rate.default_interval = Duration::ZERO;
    cfg.policy.rate.per_host_concurrency = 8;
    cfg
}

async fn spawn_http_proxy(
    origin_addr: SocketAddr,
    target_host: &'static str,
    hits: Arc<AtomicUsize>,
) -> SocketAddr {
    let proxy = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let proxy_addr = proxy.local_addr().unwrap();
    tokio::spawn(async move {
        while let Ok((mut client, _)) = proxy.accept().await {
            let hits = hits.clone();
            tokio::spawn(async move {
                let mut head = Vec::new();
                loop {
                    if head.len() >= 16_384 {
                        return;
                    }
                    match client.read_u8().await {
                        Ok(b) => head.push(b),
                        Err(_) => return,
                    }
                    if head.ends_with(b"\r\n\r\n") {
                        break;
                    }
                }
                let Some(end) = head.windows(2).position(|w| w == b"\r\n") else {
                    return;
                };
                let line = String::from_utf8_lossy(&head[..end]);
                let mut words = line.split_whitespace();
                let (Some("GET"), Some(target), Some(version)) =
                    (words.next(), words.next(), words.next())
                else {
                    return;
                };
                let Ok(url) = Url::parse(target) else {
                    return;
                };
                if url.host_str() != Some(target_host) {
                    return;
                }
                hits.fetch_add(1, Ordering::SeqCst);
                let Ok(mut upstream) = tokio::net::TcpStream::connect(origin_addr).await else {
                    return;
                };
                let mut path = url.path().to_owned();
                if let Some(q) = url.query() {
                    path.push('?');
                    path.push_str(q);
                }
                let request = format!("GET {path} {version}\r\n");
                if upstream.write_all(request.as_bytes()).await.is_err()
                    || upstream.write_all(&head[end + 2..]).await.is_err()
                {
                    return;
                }
                let _ = tokio::io::copy_bidirectional(&mut client, &mut upstream).await;
            });
        }
    });
    proxy_addr
}

#[test]
fn itna_01_discover_scripts_shares_network_budget() {
    let rt = runtime();
    rt.block_on(async {
        let total_hits = Arc::new(AtomicUsize::new(0));
        let h_main = total_hits.clone();
        let h_js = total_hits.clone();
        let h_redir = total_hits.clone();
        let app = Router::new()
            .route(
                "/",
                get(move || {
                    let h_main = h_main.clone();
                    async move {
                        h_main.fetch_add(1, Ordering::SeqCst);
                        let mut scripts = String::new();
                        scripts.push_str("<script src=\"/redir.js\"></script>");
                        for i in 0..32 {
                            scripts.push_str(&format!("<script src=\"/s/{i}.js\"></script>"));
                        }
                        Html(format!(
                            "<html><head>{scripts}</head><body><article><h1>Script Heavy</h1><p>Body with many external scripts for budget enforcement.</p></article></body></html>"
                        ))
                    }
                }),
            )
            .route(
                "/redir.js",
                get(move || {
                    let h_redir = h_redir.clone();
                    async move {
                        h_redir.fetch_add(1, Ordering::SeqCst);
                        Redirect::temporary("/s/extra.js")
                    }
                }),
            )
            .route(
                "/s/{name}",
                get(move || {
                    let h_js = h_js.clone();
                    async move {
                        h_js.fetch_add(1, Ordering::SeqCst);
                        "window.__loaded = 1;"
                    }
                }),
            );
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = listener.local_addr().unwrap();
        tokio::spawn(async move { axum::serve(listener, app).await.unwrap() });

        let scraper = Scraper::new(itna_scraper_cfg()).unwrap();
        let opts = ScrapeOptions {
            engine: Some("http".into()),
            formats: vec![Format::new("markdown"), Format::new("links")],
            discover_scripts: true,
            ..Default::default()
        };
        let doc = scraper
            .scrape(&format!("http://{addr}/"), &opts)
            .await
            .unwrap();
        let hits = total_hits.load(Ordering::SeqCst);
        assert!(
            hits <= 16,
            "discoverScripts under ITNA must share the 16-navigation NetworkBudget (got {hits} hits)"
        );
        let warns = doc
            .metadata
            .pagesift
            .as_ref()
            .map(|p| p.warnings.as_slice())
            .unwrap_or(&[]);
        assert!(
            warns.iter().any(|w| w.contains("script_discovery_incomplete")),
            "expected script_discovery_incomplete warning, got warnings={warns:?}"
        );
    });
}

#[test]
fn itna_03_fallback_failure_preserves_access_barrier_and_never_bypasses_dead_proxy() {
    let rt = runtime();
    rt.block_on(async {
        let origin_hits = Arc::new(AtomicUsize::new(0));
        let oh = origin_hits.clone();
        let app = Router::new().route(
            "/",
            get(move || {
                let oh = oh.clone();
                async move {
                    oh.fetch_add(1, Ordering::SeqCst);
                    (
                        StatusCode::FORBIDDEN,
                        [("set-cookie", "probe=1; Path=/")],
                        Html("<html><body>Access denied</body></html>"),
                    )
                }
            }),
        );
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = listener.local_addr().unwrap();
        tokio::spawn(async move { axum::serve(listener, app).await.unwrap() });

        // Case A: First attempt hits 403 AccessBarrier, browser executable is missing -> preserve AccessBarrier.
        let mut cfg = itna_scraper_cfg();
        cfg.browser.source =
            BrowserSource::Executable(PathBuf::from("/nonexistent/pagesift-no-chrome"));
        cfg.browser.engine = Some(EngineKind::Chrome);
        let scraper = Scraper::new(cfg).unwrap();
        let opts = ScrapeOptions {
            timeout: Some(5_000),
            ..Default::default()
        };
        let err = scraper
            .scrape(&format!("http://{addr}/"), &opts)
            .await
            .expect_err("expected AccessBarrier");
        assert_eq!(
            err.code,
            ErrorCode::AccessBarrier,
            "fallback failure must preserve primary AccessBarrier error, got: {err:?}"
        );
        let details = err.details.expect("error details");
        let itna = &details["itna"];
        assert_eq!(itna["attempts"].as_array().unwrap().len(), 2);
        assert_eq!(itna["fallbackError"]["code"], "UPSTREAM_ERROR");

        // Case B: Named egress with unreachable proxy -> 0 direct hits to origin, no silent direct fallback.
        let dead_port = {
            let tmp = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
            tmp.local_addr().unwrap().port()
        };
        origin_hits.store(0, Ordering::SeqCst);
        let mut cfg2 = itna_scraper_cfg();
        cfg2.egress = vec![("dead-isp".into(), format!("http://127.0.0.1:{dead_port}"))];
        let scraper2 = Scraper::new(cfg2).unwrap();
        let opts2 = ScrapeOptions {
            egress: Some("dead-isp".into()),
            ignore_robots_txt: true,
            timeout: Some(5_000),
            ..Default::default()
        };
        let err2 = scraper2
            .scrape(&format!("http://{addr}/"), &opts2)
            .await
            .expect_err("dead proxy must fail without bypassing to direct");
        assert_eq!(
            origin_hits.load(Ordering::SeqCst),
            0,
            "origin must receive zero direct hits when configured proxy is dead"
        );
        assert_eq!(
            err2.details
                .as_ref()
                .and_then(|d| d["itna"]["egress"].as_str()),
            Some("dead-isp")
        );
    });
}

#[test]
fn itna_04_browser_fallback_proxy_used_matches_named_and_default_proxy() {
    if pagesift_cdp::detect_browser().is_none() || pagesift_cdp::running_as_root() {
        eprintln!("skipping: Chrome unavailable or running as root");
        return;
    }
    let rt = runtime();
    rt.block_on(async {
        let app = Router::new().route(
            "/",
            get(|headers: HeaderMap| async move {
                let ua = headers
                    .get("user-agent")
                    .and_then(|v| v.to_str().ok())
                    .unwrap_or("");
                if ua.contains("pagesift/") {
                    (
                        StatusCode::FORBIDDEN,
                        [("set-cookie", "probe=ok; Path=/")],
                        Html("<html><body>Access denied</body></html>"),
                    )
                } else if headers
                    .get("cookie")
                    .and_then(|v| v.to_str().ok())
                    .unwrap_or("")
                    .contains("probe=ok")
                {
                    (
                        StatusCode::OK,
                        [("set-cookie", "unused=1")],
                        Html("<html><body><article><h1>Proxy Label Verified</h1><p>Content rendered cleanly through the selected proxy egress.</p></article></body></html>"),
                    )
                } else {
                    (
                        StatusCode::FORBIDDEN,
                        [("set-cookie", "unused=1")],
                        Html("<html><body>Access denied</body></html>"),
                    )
                }
            }),
        );
        let origin = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let origin_addr = origin.local_addr().unwrap();
        tokio::spawn(async move { axum::serve(origin, app).await.unwrap() });

        let proxy_hits = Arc::new(AtomicUsize::new(0));
        let proxy_addr = spawn_http_proxy(origin_addr, "93.184.216.34", proxy_hits.clone()).await;
        let opts = ScrapeOptions {
            timeout: Some(20_000),
            ..Default::default()
        };

        // Subcase A: Default proxy -> proxy_used == "default_proxy" (not "basic").
        let mut cfg_def = ScraperConfig {
            itna: true,
            ..Default::default()
        };
        cfg_def.fetcher.env_proxy = false;
        cfg_def.fetcher.proxy = Some(format!("http://{proxy_addr}"));
        cfg_def.policy.rate.default_interval = Duration::ZERO;
        let scraper_def = Scraper::new(cfg_def).unwrap();
        let doc_def = scraper_def
            .scrape("http://93.184.216.34:8080/", &opts)
            .await
            .unwrap();
        assert_eq!(doc_def.metadata.proxy_used, "default_proxy");
        assert_eq!(
            doc_def.metadata.pagesift.as_ref().unwrap().itna.as_ref().unwrap()["egress"],
            "default_proxy"
        );
        assert!(proxy_hits.load(Ordering::SeqCst) >= 2);

        // Subcase B: Named egress pool -> proxy_used == "isp-eu-1" (not "basic").
        proxy_hits.store(0, Ordering::SeqCst);
        let mut cfg_named = ScraperConfig {
            itna: true,
            ..Default::default()
        };
        cfg_named.fetcher.env_proxy = false;
        cfg_named.egress = vec![("isp-eu-1".into(), format!("http://{proxy_addr}"))];
        cfg_named.policy.rate.default_interval = Duration::ZERO;
        let scraper_named = Scraper::new(cfg_named).unwrap();
        let opts_named = ScrapeOptions {
            egress: Some("isp-eu-1".into()),
            ignore_robots_txt: true,
            timeout: Some(20_000),
            ..Default::default()
        };
        let doc_named = scraper_named
            .scrape("http://93.184.216.34:8080/", &opts_named)
            .await
            .unwrap();
        assert_eq!(doc_named.metadata.proxy_used, "isp-eu-1");
        assert_eq!(
            doc_named.metadata.pagesift.as_ref().unwrap().itna.as_ref().unwrap()["egress"],
            "isp-eu-1"
        );
        assert!(proxy_hits.load(Ordering::SeqCst) >= 2);
    });
}

#[test]
fn itna_05_lightpanda_only_renders_normal_pages_and_refuses_barrier_recovery() {
    let lp_path = pagesift_cdp::detect_engines()
        .into_iter()
        .find(|(k, _)| *k == EngineKind::Lightpanda)
        .map(|(_, p)| p);
    let Some(lp_path) = lp_path else {
        eprintln!("skipping: Lightpanda binary unavailable");
        return;
    };
    let rt = runtime();
    rt.block_on(async {
        let app = Router::new()
            .route(
                "/js",
                get(|| async {
                    Html(
                        "<html><body><article><h1 id=\"out\">Pending</h1><p>Initial paragraph for normal JS page rendering.</p></article><script>document.getElementById('out').textContent='LP_RENDERED_OK';</script></body></html>",
                    )
                }),
            )
            .route(
                "/barrier",
                get(|| async {
                    (
                        StatusCode::FORBIDDEN,
                        [("set-cookie", "probe=1; Path=/")],
                        Html("<html><body>Access denied</body></html>"),
                    )
                }),
            );
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = listener.local_addr().unwrap();
        tokio::spawn(async move { axum::serve(listener, app).await.unwrap() });

        let mut cfg = itna_scraper_cfg();
        cfg.browser.source = BrowserSource::Executable(lp_path);
        cfg.browser.engine = Some(EngineKind::Lightpanda);
        // Pre-populate Lightpanda launch args so `engine_chain_with_allow` stays strictly Lightpanda-only
        // even on a host that also has Chrome installed.
        EngineKind::Lightpanda.configure(&mut cfg.browser, true);
        let scraper = Scraper::new(cfg).unwrap();

        // Normal JS page under ITNA + Lightpanda-only succeeds and emits explicit subrequest-budget warning.
        let doc = scraper
            .scrape(
                &format!("http://{addr}/js"),
                &ScrapeOptions {
                    engine: Some("browser".into()),
                    timeout: Some(15_000),
                    ..Default::default()
                },
            )
            .await
            .expect("Lightpanda-only deployment must still render non-challenge pages under ITNA");
        assert!(
            doc.markdown.as_deref().unwrap_or("").contains("LP_RENDERED_OK"),
            "expected JS rendered output from Lightpanda: {:?}",
            doc.markdown
        );
        let warns = doc
            .metadata
            .pagesift
            .as_ref()
            .map(|p| p.warnings.as_slice())
            .unwrap_or(&[]);
        assert!(
            warns
                .iter()
                .any(|w| w.contains("itna_non_chrome_no_subrequest_budget")),
            "expected itna_non_chrome_no_subrequest_budget warning, got: {warns:?}"
        );

        // Actual 403 barrier recovery requires Chrome (cookie bridge + CDP subrequest interception).
        let err = scraper
            .scrape(
                &format!("http://{addr}/barrier"),
                &ScrapeOptions {
                    timeout: Some(15_000),
                    ..Default::default()
                },
            )
            .await
            .expect_err("Lightpanda-only must refuse ITNA barrier recovery");
        assert_eq!(err.code, ErrorCode::AccessBarrier);
        let fb = &err.details.unwrap()["itna"]["fallbackError"];
        assert_eq!(fb["code"], "UNSUPPORTED_OPTION");
        assert!(
            fb["message"].as_str().unwrap_or("").contains("Chrome"),
            "fallback error should explain Chrome requirement: {fb}"
        );
    });
}

#[test]
fn itna_06_single_entry_deadline_bounds_total_elapsed() {
    let rt = runtime();
    rt.block_on(async {
        let app = Router::new().route(
            "/slow",
            get(|| async {
                tokio::time::sleep(Duration::from_secs(5)).await;
                Html("<html><body><p>Too slow</p></body></html>")
            }),
        );
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = listener.local_addr().unwrap();
        tokio::spawn(async move { axum::serve(listener, app).await.unwrap() });

        let scraper = Scraper::new(itna_scraper_cfg()).unwrap();
        // wall-clock: verifying ITNA single entry deadline bounds real timeout elapsed
        let t0 = Instant::now();
        let err = scraper
            .scrape(
                &format!("http://{addr}/slow"),
                &ScrapeOptions {
                    engine: Some("http".into()),
                    timeout: Some(300),
                    ..Default::default()
                },
            )
            .await
            .expect_err("slow origin must time out");
        let elapsed = t0.elapsed();
        assert_eq!(err.code, ErrorCode::Timeout);
        assert!(
            elapsed < Duration::from_secs(2),
            "single deadline should fire promptly around 300ms, took {elapsed:?}"
        );
    });
}

#[test]
fn itna_07_engine_auto_never_double_invokes_browser_on_thin_js_or_bare_403() {
    if pagesift_cdp::detect_browser().is_none() || pagesift_cdp::running_as_root() {
        eprintln!("skipping: Chrome unavailable or running as root");
        return;
    }
    let rt = runtime();
    rt.block_on(async {
        let thin_hits = Arc::new(AtomicUsize::new(0));
        let bare403_hits = Arc::new(AtomicUsize::new(0));
        let th = thin_hits.clone();
        let bh = bare403_hits.clone();
        let app = Router::new()
            .route(
                "/static",
                get(|| async {
                    Html("<html><body><article><h1>Static Article</h1><p>This static document contains rich prose that requires zero browser rendering so ITNA stays purely on the HTTP path.</p></article></body></html>")
                }),
            )
            .route(
                "/thin-js",
                get(move || {
                    let th = th.clone();
                    async move {
                        th.fetch_add(1, Ordering::SeqCst);
                        Html("<html><body><div id=\"root\"></div><script>document.getElementById('root').innerHTML='<article><h1>Upgraded SPA</h1><p>Client rendered article content produced cleanly by browser auto upgrade.</p></article>';</script></body></html>")
                    }
                }),
            )
            .route(
                "/bare-403",
                get(move || {
                    let bh = bh.clone();
                    async move {
                        bh.fetch_add(1, Ordering::SeqCst);
                        (
                            StatusCode::FORBIDDEN,
                            Html("<html><body><div id=\"msg\">Denied</div></body></html>"),
                        )
                    }
                }),
            );
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = listener.local_addr().unwrap();
        tokio::spawn(async move { axum::serve(listener, app).await.unwrap() });

        let scraper = Scraper::new(itna_scraper_cfg()).unwrap();
        let opts = ScrapeOptions {
            timeout: Some(20_000),
            ..Default::default()
        };

        // 1. Static page: 0 browser executions.
        let doc_static = scraper
            .scrape(&format!("http://{addr}/static"), &opts)
            .await
            .unwrap();
        let trace_static = doc_static.metadata.pagesift.unwrap().itna.unwrap();
        assert_eq!(trace_static["browserExecutions"], 0);
        assert_eq!(trace_static["attempts"].as_array().unwrap().len(), 1);

        // 2. Thin JS shell: auto-upgrades to browser ONCE (never 2 browser executions).
        let doc_thin = scraper
            .scrape(&format!("http://{addr}/thin-js"), &opts)
            .await
            .unwrap();
        assert!(
            doc_thin
                .markdown
                .as_deref()
                .unwrap_or("")
                .contains("Upgraded SPA")
        );
        let trace_thin = doc_thin.metadata.pagesift.unwrap().itna.unwrap();
        assert_eq!(
            trace_thin["browserExecutions"], 1,
            "thin JS auto upgrade must execute browser exactly once: {trace_thin}"
        );
        assert_eq!(
            trace_thin["attempts"].as_array().unwrap().len(),
            2,
            "trace must record both HTTP probe and browser auto-upgrade: {trace_thin}"
        );

        // 3. Bare 403 thin page: must NOT trigger internal auto-upgrade AND ITNA browser fallback (max 1 browser).
        let err_403 = scraper
            .scrape(&format!("http://{addr}/bare-403"), &opts)
            .await
            .expect_err("bare 403 should report AccessBarrier");
        assert_eq!(err_403.code, ErrorCode::AccessBarrier);
        let trace_403 = &err_403.details.expect("403 details")["itna"];
        let browser_runs = trace_403["browserExecutions"].as_u64().unwrap_or(99);
        assert!(
            browser_runs <= 1,
            "bare 403 must never invoke browser twice (got {browser_runs}): {trace_403}"
        );
    });
}

#[test]
fn itna_09_cdp_intercepts_iframe_redirect_worker_and_blocks_cross_port_ws() {
    if pagesift_cdp::detect_browser().is_none() || pagesift_cdp::running_as_root() {
        eprintln!("skipping: Chrome unavailable or running as root");
        return;
    }
    let rt = runtime();
    rt.block_on(async {
        let other_ws_hits = Arc::new(AtomicUsize::new(0));
        let ow = other_ws_hits.clone();
        let other_srv = Router::new().route(
            "/ws",
            get(move || {
                let ow = ow.clone();
                async move {
                    ow.fetch_add(1, Ordering::SeqCst);
                    StatusCode::FORBIDDEN
                }
            }),
        );
        let other_listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let other_addr = other_listener.local_addr().unwrap();
        tokio::spawn(async move { axum::serve(other_listener, other_srv).await.unwrap() });

        let sub_hits = Arc::new(AtomicUsize::new(0));
        let sf = sub_hits.clone();
        let sr = sub_hits.clone();
        let sw = sub_hits.clone();
        let main_srv = Router::new()
            .route(
                "/",
                get(move |headers: HeaderMap| async move {
                    let ua = headers.get("user-agent").and_then(|v| v.to_str().ok()).unwrap_or("");
                    if ua.contains("pagesift/") {
                        (
                            StatusCode::FORBIDDEN,
                            [("set-cookie", "secret=tok; Path=/; HttpOnly")],
                            Html("<html><body>Access denied</body></html>".to_owned()),
                        )
                    } else {
                        (
                            StatusCode::OK,
                            [("set-cookie", "unused=1")],
                            Html(format!(
                                "<html><body><article><h1>Subrequest Audit</h1><p>Main article rendered cleanly.</p></article>\
                                <iframe src=\"/frame\"></iframe>\
                                <script src=\"/redir-js\"></script>\
                                <script>\
                                try {{ const w = new Worker('/worker.js'); }} catch(e) {{}}\
                                try {{ const ws = new WebSocket('ws://{other_addr}/ws'); }} catch(e) {{}}\
                                </script></body></html>"
                            )),
                        )
                    }
                }),
            )
            .route(
                "/frame",
                get(move || {
                    let sf = sf.clone();
                    async move {
                        sf.fetch_add(1, Ordering::SeqCst);
                        Html("<html><body><p>Embedded iframe text.</p></body></html>")
                    }
                }),
            )
            .route(
                "/redir-js",
                get(move || {
                    let sr = sr.clone();
                    async move {
                        sr.fetch_add(1, Ordering::SeqCst);
                        Redirect::temporary("/real.js")
                    }
                }),
            )
            .route(
                "/real.js",
                get(|| async { "window.__r = 1;" }),
            )
            .route(
                "/worker.js",
                get(move || {
                    let sw = sw.clone();
                    async move {
                        sw.fetch_add(1, Ordering::SeqCst);
                        format!(
                            "fetch('/real.js').catch(()=>{{}}); try {{ new WebSocket('ws://{other_addr}/ws'); }} catch(e) {{}}"
                        )
                    }
                }),
            );
        let main_listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let main_addr = main_listener.local_addr().unwrap();
        tokio::spawn(async move { axum::serve(main_listener, main_srv).await.unwrap() });

        let scraper = Scraper::new(itna_scraper_cfg()).unwrap();
        let doc = scraper
            .scrape(
                &format!("http://{main_addr}/"),
                &ScrapeOptions {
                    timeout: Some(20_000),
                    ..Default::default()
                },
            )
            .await
            .unwrap();
        assert!(
            doc.markdown
                .as_deref()
                .unwrap_or("")
                .contains("Subrequest Audit")
        );
        assert!(
            sub_hits.load(Ordering::SeqCst) >= 2,
            "same-origin iframe/redirect/worker subrequests should proceed under budget"
        );
        assert_eq!(
            other_ws_hits.load(Ordering::SeqCst),
            0,
            "cross-port WebSocket under cookie bridge must be blocked"
        );
    });
}

#[test]
fn itna_10_multi_egress_isolates_cookies_across_egresses() {
    let rt = runtime();
    rt.block_on(async {
        let origin_srv = Router::new()
            .route(
                "/set",
                get(|| async {
                    (
                        StatusCode::OK,
                        [("set-cookie", "egsess=ONLY_FIRST_EGRESS; Path=/")],
                        Html("<html><body><article><h1>Set</h1><p>Cookie issued.</p></article></body></html>"),
                    )
                }),
            )
            .route(
                "/echo",
                get(|headers: HeaderMap| async move {
                    let c = headers
                        .get("cookie")
                        .and_then(|v| v.to_str().ok())
                        .unwrap_or("NONE");
                    Html(format!(
                        "<html><body><article><h1>Echo</h1><p>COOKIE={c}</p></article></body></html>"
                    ))
                }),
            );
        let origin = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let origin_addr = origin.local_addr().unwrap();
        tokio::spawn(async move { axum::serve(origin, origin_srv).await.unwrap() });

        let hits_a = Arc::new(AtomicUsize::new(0));
        let hits_b = Arc::new(AtomicUsize::new(0));
        let proxy_a = spawn_http_proxy(origin_addr, "93.184.216.34", hits_a.clone()).await;
        let proxy_b = spawn_http_proxy(origin_addr, "93.184.216.34", hits_b.clone()).await;

        // Single Scraper with two named egresses sharing one IdentityPool.
        let mut cfg = ScraperConfig {
            itna: true,
            ..Default::default()
        };
        cfg.fetcher.env_proxy = false;
        cfg.egress = vec![
            ("egress-a".into(), format!("http://{proxy_a}")),
            ("egress-b".into(), format!("http://{proxy_b}")),
        ];
        cfg.policy.rate.default_interval = Duration::ZERO;
        let scraper = Scraper::new(cfg).unwrap();
        let opts = ScrapeOptions {
            engine: Some("http".into()),
            ignore_robots_txt: true,
            owner: Some("acct:itna10".into()),
            ..Default::default()
        };

        let d_set = scraper
            .scrape("http://93.184.216.34:8080/set", &opts)
            .await
            .unwrap();
        let first_egress = d_set.metadata.proxy_used.clone();
        assert!(first_egress == "egress-a" || first_egress == "egress-b");

        // Probe `/echo` across repeated requests so the pool exercises both egresses;
        // every request routed via the other egress MUST NOT receive the first egress's cookie.
        let mut saw_first = false;
        let mut saw_other = false;
        for i in 0..24 {
            let doc = scraper
                .scrape(&format!("http://93.184.216.34:8080/echo?i={i}"), &opts)
                .await
                .unwrap();
            let used = doc.metadata.proxy_used.as_str();
            let md = doc.markdown.as_deref().unwrap_or("");
            if used == first_egress {
                saw_first = true;
                assert!(
                    md.contains("ONLY_FIRST_EGRESS"),
                    "same egress {used} should reuse its own session cookie: {md}"
                );
            } else {
                saw_other = true;
                assert!(
                    !md.contains("ONLY_FIRST_EGRESS"),
                    "cookie from {first_egress} leaked onto different egress {used}: {md}"
                );
            }
            if saw_first && saw_other {
                break;
            }
        }
        assert!(
            saw_first && saw_other,
            "expected multi-egress pool to exercise both egresses (saw_first={saw_first}, saw_other={saw_other})"
        );
    });
}
