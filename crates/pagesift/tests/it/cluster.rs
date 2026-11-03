//! 多服务器模式端到端：一个进程里起网关 + 2 个工人（本机回环端口、h2c、真实帧协议），
//! 经 HTTP API 提交批量抓取和小爬取，结果与单主机一致；杀掉负责该站点的工人后任务仍全部完成。
//!
//! 每个工人跑在自己的运行时里，"杀掉"就是丢掉它的运行时（连接、在执行的抓取一起消失，相当于进程被杀）。

use std::collections::{BTreeMap, HashMap};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use axum::Router;
use axum::body::Body;
use axum::extract::Path;
use axum::http::Request;
use axum::response::Html;
use axum::routing::get;
use http_body_util::BodyExt;
use pagesift::cluster::{ClusterEngine, Dispatch, Link, WorkerOptions};
use pagesift::{Scraper, ScraperConfig};
use pagesift_cluster::{
    Coordinator, GatewayConfig, Journal, Ledger, LedgerConfig, MonoClock, Owner, SiteKey,
    TokenStore, WorkerId,
};
use serde_json::{Value, json};
use tower::ServiceExt;

const PAGES: usize = 20;

type Hits = Arc<Mutex<HashMap<usize, usize>>>;

/// 第 i 页链接到 2i+1、2i+2；每次请求慢一点，好在中途杀工人。
/// `ip`：每个测试用自己的回环地址（127.0.0.0/8），一个测试释放的端口不会被另一个测试占到同一个地址上。
/// 第 `n` 个测试用的回环地址。Linux 上整个 127.0.0.0/8 都是回环，各测试用不同地址互不占端口；
/// macOS、Windows 默认只有 127.0.0.1，都用它（端口由系统分配）。
/// 非 Linux 上各测试共用 127.0.0.1：一个测试放掉的端口可能被另一个测试接走（连到别人的网关），
/// 所以这些测试在那里排队一个个跑；Linux 上各用各的地址，照常并行。
fn serial() -> Option<std::sync::MutexGuard<'static, ()>> {
    static LOCK: std::sync::Mutex<()> = std::sync::Mutex::new(());
    (!cfg!(target_os = "linux")).then(|| LOCK.lock().unwrap_or_else(|e| e.into_inner()))
}

fn loopback(n: u8) -> &'static str {
    if cfg!(target_os = "linux") {
        ["127.0.0.1", "127.0.0.2", "127.0.0.3", "127.0.0.4"][usize::from(n - 1)]
    } else {
        "127.0.0.1"
    }
}

fn site(rt: &tokio::runtime::Runtime, hits: Hits, ip: &str) -> String {
    let app = Router::new().route(
        "/p/{i}",
        get(move |Path(i): Path<usize>| {
            let hits = hits.clone();
            async move {
                *hits.lock().unwrap().entry(i).or_default() += 1;
                tokio::time::sleep(Duration::from_millis(20)).await;
                let links: String = [2 * i + 1, 2 * i + 2]
                    .iter()
                    .filter(|c| **c < PAGES)
                    .map(|c| format!("<p>Next: <a href=\"/p/{c}\">part {c}</a></p>"))
                    .collect();
                Html(format!(
                    "<html><head><title>Part {i}</title></head><body><article><h1>Part {i}</h1>\
                     <p>Part {i} has enough text to be the main content of the page.</p>{links}</article></body></html>"
                ))
            }
        }),
    );
    let listener = rt.block_on(tokio::net::TcpListener::bind((ip, 0))).unwrap();
    let addr = listener.local_addr().unwrap();
    rt.spawn(async move { axum::serve(listener, app).await.unwrap() });
    format!("http://{addr}")
}

fn scraper() -> Arc<Scraper> {
    let mut cfg = ScraperConfig::default();
    cfg.fetcher.ssrf.allow_private_network = true;
    cfg.policy.rate.default_interval = Duration::from_millis(1);
    cfg.policy.rate.per_host_concurrency = 4;
    Arc::new(Scraper::new(cfg).unwrap())
}

fn runtime() -> tokio::runtime::Runtime {
    tokio::runtime::Builder::new_multi_thread()
        .worker_threads(2)
        .enable_all()
        .build()
        .unwrap()
}

fn token(id: &str) -> String {
    format!("token-of-{id}")
}

struct Gateway {
    app: Router,
    dispatch: Arc<Dispatch>,
    workers_addr: String,
}

fn gateway(rt: &tokio::runtime::Runtime) -> Gateway {
    rt.block_on(async {
        let ledger = Arc::new(Ledger::open(LedgerConfig::new(MonoClock::new(), false)).unwrap());
        let tokens = TokenStore::new();
        for id in ["a-w0", "a-w1"] {
            tokens.add(WorkerId::new(id), &token(id));
        }
        let d = Dispatch::new(
            ledger,
            tokens,
            GatewayConfig::default(),
            Duration::from_secs(30),
        );
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let workers_addr = listener.local_addr().unwrap().to_string();
        let gw = d.coordinator().clone();
        tokio::spawn(async move { gw.run_ticker().await });
        let gw = d.coordinator().clone();
        tokio::spawn(pagesift::cluster::serve_workers(gw, listener, None));
        let engine = Arc::new(ClusterEngine::new(scraper(), d.clone()));
        Gateway {
            app: pagesift::api::router(engine, Default::default()),
            dispatch: d,
            workers_addr,
        }
    })
}

/// 在自己的运行时里起一个工人。
fn worker(id: &str, gateway: &str) -> tokio::runtime::Runtime {
    worker_of(id, &[gateway])
}

fn worker_of(id: &str, gateways: &[&str]) -> tokio::runtime::Runtime {
    worker_with(id, gateways, scraper)
}

fn worker_with(id: &str, gateways: &[&str], make: fn() -> Arc<Scraper>) -> tokio::runtime::Runtime {
    let rt = runtime();
    let opts = WorkerOptions {
        gateways: gateways.iter().map(|g| g.to_string()).collect(),
        link: Link {
            id: WorkerId::new(id),
            token: token(id),
            tls: None,
        },
        task_prior: Duration::from_secs(30),
    };
    rt.spawn(
        async move { pagesift::cluster::worker::run(make(), opts, std::future::pending()).await },
    );
    rt
}

async fn call(app: &Router, method: &str, uri: &str, body: Option<Value>) -> Value {
    let req = Request::builder()
        .method(method)
        .uri(uri)
        .header("content-type", "application/json")
        .body(body.map_or(Body::empty(), |b| Body::from(b.to_string())))
        .unwrap();
    let resp = app.clone().oneshot(req).await.unwrap();
    serde_json::from_slice(&resp.into_body().collect().await.unwrap().to_bytes()).unwrap()
}

/// 提交一个任务，返回状态地址。
async fn start(app: &Router, kind: &str, base: &str) -> String {
    let (path, body) = match kind {
        "batch" => (
            "/v2/batch/scrape",
            json!({
                "urls": (0..PAGES).map(|i| format!("{base}/p/{i}")).collect::<Vec<_>>(),
                "formats": ["markdown"],
                "engine": "http",
                "timeout": 5000,
            }),
        ),
        _ => (
            "/v2/crawl",
            json!({
                "url": format!("{base}/p/0"),
                "limit": PAGES,
                "sitemap": "skip",
                "crawlEntireDomain": true,
                "scrapeOptions": {"engine": "http", "formats": ["markdown"], "timeout": 5000},
            }),
        ),
    };
    let v = call(app, "POST", path, Some(body)).await;
    let id = v["id"]
        .as_str()
        .unwrap_or_else(|| panic!("{v}"))
        .to_string();
    format!("{path}/{id}")
}

async fn completed(app: &Router, status: &str) -> u64 {
    call(app, "GET", status, None).await["completed"]
        .as_u64()
        .unwrap()
}

/// 等任务完成，返回 sourceURL → (标题, Markdown)。
async fn finish(app: &Router, status: &str) -> BTreeMap<String, (String, String)> {
    let mut v = Value::Null;
    for _ in 0..6000 {
        v = call(app, "GET", status, None).await;
        if v["status"] == "completed" {
            break;
        }
        tokio::time::sleep(Duration::from_millis(5)).await;
    }
    assert_eq!(v["status"], "completed", "{v}");
    let mut out = BTreeMap::new();
    let mut skip = 0;
    loop {
        let page = call(app, "GET", &format!("{status}?skip={skip}"), None).await;
        let data = page["data"].as_array().unwrap().clone();
        if data.is_empty() {
            break;
        }
        skip += data.len();
        for d in data {
            let src = d["metadata"]["sourceURL"].as_str().unwrap().to_string();
            let title = d["metadata"]["title"]
                .as_str()
                .unwrap_or_default()
                .to_string();
            let md = d["markdown"].as_str().unwrap_or_default().to_string();
            assert!(
                out.insert(src.clone(), (title, md)).is_none(),
                "{src} twice"
            );
        }
    }
    out
}

#[test]
fn gateway_with_two_workers_matches_standalone_and_survives_a_killed_worker() {
    let _serial = serial();
    let site_rt = runtime();
    let hits: Hits = Arc::default();
    let base = site(&site_rt, hits.clone(), "127.0.0.1");

    let rt = runtime();
    // 单主机的基准
    let (batch_ref, crawl_ref) = rt.block_on(async {
        let app = pagesift::api::router(scraper(), Default::default());
        let b = start(&app, "batch", &base).await;
        let b = finish(&app, &b).await;
        let c = start(&app, "crawl", &base).await;
        let c = finish(&app, &c).await;
        (b, c)
    });
    assert_eq!(batch_ref.len(), PAGES);
    assert_eq!(crawl_ref.len(), PAGES);

    let gw = gateway(&rt);
    // 两个测试的工人 id、令牌各不相同：一个测试释放的端口被另一个测试重新占用时，连错的工人会被拒绝
    let workers: Vec<(String, tokio::runtime::Runtime)> = ["a-w0", "a-w1"]
        .iter()
        .map(|id| (id.to_string(), worker(id, &gw.workers_addr)))
        .collect();
    let each = pagesift::api::Engine::admitted_concurrency(&*scraper()).unwrap();
    let (batch, crawl) = rt.block_on(async {
        // 等两个工人都连上（并行跑测试时连接可能晚于提交）
        for i in 0.. {
            if gw.dispatch.capacity() == 2 * each {
                break;
            }
            assert!(i < 4000, "workers did not connect");
            tokio::time::sleep(Duration::from_millis(5)).await;
        }
        let b = start(&gw.app, "batch", &base).await;
        let b = finish(&gw.app, &b).await;
        let c = start(&gw.app, "crawl", &base).await;
        let c = finish(&gw.app, &c).await;
        (b, c)
    });
    assert_eq!(batch, batch_ref, "batch scrape through the cluster differs");
    // 网关的准入容量 = 在线工人报告的准入值之和
    assert_eq!(gw.dispatch.capacity(), 2 * each);
    assert_eq!(crawl, crawl_ref, "crawl through the cluster differs");

    // 杀掉负责这个站点的工人：先让批量任务跑起来，再丢掉它的运行时
    let site = SiteKey::of_domain(&pagesift_content::site_key("127.0.0.1"));
    let owner = match gw.dispatch.coordinator().owner_of(site) {
        Owner::Worker(w) => w.to_string(),
        o => panic!("site has no owner: {o:?}"),
    };
    let status = rt.block_on(async {
        let s = start(&gw.app, "batch", &base).await;
        for i in 0.. {
            if completed(&gw.app, &s).await >= 3 {
                break;
            }
            assert!(i < 4000, "batch made no progress");
            tokio::time::sleep(Duration::from_millis(5)).await;
        }
        s
    });
    let mut survivors = Vec::new();
    for (id, w) in workers {
        if id == owner {
            drop(w);
        } else {
            survivors.push(w);
        }
    }
    assert_eq!(survivors.len(), 1);
    let after = rt.block_on(async {
        let a = finish(&gw.app, &status).await;
        // 接手的工人照常工作：再爬一遍也和单主机一致
        let c = start(&gw.app, "crawl", &base).await;
        (a, finish(&gw.app, &c).await)
    });
    assert_eq!(after.0, batch_ref, "batch after the kill differs");
    assert_eq!(after.1, crawl_ref, "crawl after the kill differs");
    assert!(matches!(
        gw.dispatch.coordinator().owner_of(site),
        Owner::Worker(w) if w.as_str() != owner
    ));
    // 掉线的工人从容量里减掉
    assert_eq!(gw.dispatch.capacity(), each);
    drop(survivors);
    drop(rt);
}

fn store(dir: &std::path::Path) -> pagesift_store::Store {
    let mut sc = pagesift_store::Config::new(dir);
    sc.background = false;
    pagesift_store::Store::open(sc).unwrap()
}

fn tokens(ids: &[&str]) -> TokenStore {
    let t = TokenStore::new();
    for &id in ids {
        t.add(WorkerId::new(id), &token(id));
    }
    t
}

/// 网关的 HTTP 服务：账本日志、任务存储和任务复制都在 `store` 上（与 `serve --role gateway` 相同的接法）。
fn cluster_app(
    ledger: Arc<Ledger>,
    store: pagesift_store::Store,
    mirror: Arc<pagesift::api::JobMirror>,
    listener: tokio::net::TcpListener,
    ids: &[&str],
) -> Router {
    let d = Dispatch::new(
        ledger.clone(),
        tokens(ids),
        GatewayConfig::default(),
        Duration::from_secs(30),
    );
    let gw = d.coordinator().clone();
    tokio::spawn(async move { gw.run_ticker().await });
    tokio::spawn(pagesift::cluster::serve_workers(
        d.coordinator().clone(),
        listener,
        None,
    ));
    let engine = Arc::new(ClusterEngine::new(scraper(), d));
    pagesift::api::router(
        engine,
        pagesift::api::Config {
            store: Some(store),
            ledger: Some(ledger),
            job_mirror: Some(mirror),
            ..Default::default()
        },
    )
}

/// 主网关在爬取中途失联：温备接管，`/v2/crawl/{id}` 在温备上能查到，任务从断点继续并完成，结果与单主机一致。
#[test]
fn standby_takes_over_a_running_crawl() {
    let _serial = serial();
    takeover(false, "b");
}

/// 温备在主网关做过第一次快照、截掉了日志开头之后才连上（从快照重新同步），之后同样接管并完成爬取。
#[test]
fn standby_joining_after_the_first_snapshot_takes_over() {
    let _serial = serial();
    takeover(true, "c");
}

fn takeover(late: bool, tag: &str) {
    let (worker_id, sb_id) = (format!("{tag}-w0"), format!("{tag}-sb"));
    let ids: Vec<String> = vec![worker_id.clone(), sb_id.clone()];
    let site_rt = runtime();
    let hits: Hits = Arc::default();
    let ip = if late { loopback(3) } else { loopback(2) };
    let base = site(&site_rt, hits.clone(), ip);
    let rt = runtime();
    let crawl_ref = rt.block_on(async {
        let app = pagesift::api::router(scraper(), Default::default());
        let c = start(&app, "crawl", &base).await;
        finish(&app, &c).await
    });
    let (pd, sd) = (tempfile::tempdir().unwrap(), tempfile::tempdir().unwrap());

    // 主网关
    let gw_rt = runtime();
    let ids_ref: Vec<&str> = ids.iter().map(String::as_str).collect();
    let (primary, primary_ledger, gw_addr) = gw_rt.block_on(async {
        let s = store(pd.path());
        let ledger = Arc::new(
            Ledger::open(
                LedgerConfig::new(MonoClock::new(), false).with_journal(Journal::new(s.clone())),
            )
            .unwrap(),
        );
        let l = tokio::net::TcpListener::bind((ip, 0)).await.unwrap();
        let addr = l.local_addr().unwrap().to_string();
        let mirror = pagesift::cluster::replicate_jobs(&ledger, &s);
        (
            cluster_app(ledger.clone(), s, mirror, l, &ids_ref),
            ledger,
            addr,
        )
    });

    // 温备：先占好接工人的端口（系统分配），跟随主网关，接管后用同一个账本和自己的存储对外服务
    let sb_rt = runtime();
    let sb_listener = sb_rt
        .block_on(tokio::net::TcpListener::bind((ip, 0)))
        .unwrap();
    let sb_addr = sb_listener.local_addr().unwrap().to_string();
    // 账本日志和任务记录共用温备自己的一个存储（与 serve 相同）
    let sb_store = store(sd.path());
    let standby_ledger = Arc::new(
        Ledger::follower(
            LedgerConfig::new(MonoClock::new(), false).with_journal(Journal::new(sb_store.clone())),
        )
        .unwrap(),
    );
    let start_standby = |listener: tokio::net::TcpListener| {
        let ledger = standby_ledger.clone();
        let s = sb_store.clone();
        let opts = pagesift::cluster::StandbyOptions {
            primary: gw_addr.clone(),
            link: Link {
                id: WorkerId::new(sb_id.as_str()),
                token: token(&sb_id),
                tls: None,
            },
        };
        let ids = ids.clone();
        let (tx, rx) = tokio::sync::oneshot::channel();
        sb_rt.spawn(async move {
            // 复制通道在跟随之前接好（serve 的温备分支同样先接），接管后同一个镜像交给 HTTP 服务
            let mirror = pagesift::cluster::replicate_jobs(&ledger, &s);
            let term =
                pagesift::cluster::follow_until_lost(ledger.clone(), &opts, std::future::pending())
                    .await
                    .unwrap();
            assert!(term.is_some());
            let ids: Vec<&str> = ids.iter().map(String::as_str).collect();
            let _ = tx.send(cluster_app(ledger, s, mirror, listener, &ids));
        });
        rx
    };
    let mut early = None;
    let mut sb_listener = Some(sb_listener);
    if !late {
        early = Some(start_standby(sb_listener.take().unwrap()));
    }
    let w = worker_of(&worker_id, &[&gw_addr, &sb_addr]);

    let status = gw_rt.block_on(async {
        let c = start(&primary, "crawl", &base).await;
        for i in 0.. {
            let trimmed = primary_ledger.journal().unwrap().head() > 0;
            if completed(&primary, &c).await >= 5 && (!late || trimmed) {
                break;
            }
            assert!(i < 4000, "crawl made no progress");
            tokio::time::sleep(Duration::from_millis(5)).await;
        }
        c
    });
    let sb_app = match early {
        Some(rx) => rx,
        None => {
            let rx = start_standby(sb_listener.take().unwrap());
            // 等温备追上主网关（从快照重新同步）再丢主网关
            let want = primary_ledger.journal().unwrap().len();
            sb_rt.block_on(async {
                for i in 0.. {
                    if standby_ledger.shipped_len() >= want {
                        break;
                    }
                    assert!(i < 4000, "standby did not catch up from the snapshot");
                    tokio::time::sleep(Duration::from_millis(5)).await;
                }
            });
            rx
        }
    };
    drop(primary);
    drop(primary_ledger);
    drop(gw_rt);

    let app = sb_rt.block_on(async { sb_app.await.unwrap() });
    let after = sb_rt.block_on(async { finish(&app, &status).await });
    assert_eq!(after, crawl_ref, "crawl resumed on the standby differs");
    drop(w);
}

/// 要靠脚本补出正文的页面：HTTP 只拿到空壳，渲染后才有正文（"需要渲染"）。
const APP_PAGE: &str = "<html><head><title>App</title></head><body><div id=app></div><script>\
document.getElementById('app').innerHTML = '<article><h1>Rendered</h1><p>' + \
'This paragraph only exists after the script runs. '.repeat(80) + '</p></article>';</script></body></html>";

fn learning_site(rt: &tokio::runtime::Runtime, ip: &str) -> String {
    let app = Router::new()
        .route(
            "/robots.txt",
            get(|| async { "User-agent: *\nDisallow: /private\n" }),
        )
        .route(
            "/private/{i}",
            get(|Path(i): Path<usize>| async move {
                Html(format!(
                    "<html><head><title>Private {i}</title></head><body><article><h1>Private {i}</h1>\
                     <p>Private page {i} has enough text to be the main content of the page.</p></article></body></html>"
                ))
            }),
        )
        .route("/app/{i}", get(|| async { Html(APP_PAGE) }));
    let listener = rt.block_on(tokio::net::TcpListener::bind((ip, 0))).unwrap();
    let addr = listener.local_addr().unwrap();
    rt.spawn(async move { axum::serve(listener, app).await.unwrap() });
    format!("http://{addr}")
}

/// 工人的抓取器：本机设置不遵守 robots（证明工人只按任务里网关判定的模式执行），审计记录随结果交给网关。
fn worker_scraper() -> Arc<Scraper> {
    let mut cfg = ScraperConfig::default();
    cfg.fetcher.ssrf.allow_private_network = true;
    cfg.policy.rate.default_interval = Duration::from_millis(1);
    cfg.policy.respect_robots = false;
    cfg.audit = Some(Arc::new(pagesift::audit::Audit::collect_only("worker")));
    Arc::new(Scraper::new(cfg).unwrap())
}

fn render_step(doc: &Value) -> Value {
    doc["metadata"]["pagesift"]["plan"]["steps"]
        .as_array()
        .and_then(|s| s.iter().find(|s| s["action"] == "render").cloned())
        .unwrap_or_else(|| panic!("no render step: {doc}"))
}

/// 统计汇聚、robots 判定、审计与监控：
/// - 工人 A 在站点上学到"需要渲染"后被杀，站点换到工人 B，B 第一次就按网关下发的先验决策（`learned`）；
/// - robots 由网关判定：工人本机不遵守 robots，网关说遵守时照样被 robots 挡住；请求忽略 robots 时才抓；
/// - 工人的审计记录（含 robotsIgnored）由网关统一追加，带工人 id；
/// - 网关的 `/metrics` 按工人加标签。
#[test]
fn priors_audit_and_metrics_go_through_the_gateway() {
    let _serial = serial();
    let ip = loopback(4);
    let site_rt = runtime();
    let base = learning_site(&site_rt, ip);
    let rt = runtime();
    let audit_dir = tempfile::tempdir().unwrap();
    let audit_path = audit_dir.path().join("audit.jsonl");
    let ids = ["d-w0", "d-w1"];
    let (app, d, addr, gw_audit) = rt.block_on(async {
        let ledger = Arc::new(Ledger::open(LedgerConfig::new(MonoClock::new(), false)).unwrap());
        let d = Dispatch::new(
            ledger,
            tokens(&ids),
            GatewayConfig::default(),
            Duration::from_secs(30),
        );
        let l = tokio::net::TcpListener::bind((ip, 0)).await.unwrap();
        let addr = l.local_addr().unwrap().to_string();
        let gw = d.coordinator().clone();
        tokio::spawn(async move { gw.run_ticker().await });
        tokio::spawn(pagesift::cluster::serve_workers(
            d.coordinator().clone(),
            l,
            None,
        ));
        let mut cfg = ScraperConfig::default();
        cfg.fetcher.ssrf.allow_private_network = true;
        let mut ac = pagesift::audit::AuditConfig::new(&audit_path);
        ac.snapshot_dir = None;
        let gw_audit = Arc::new(pagesift::audit::Audit::open(ac).unwrap());
        cfg.audit = Some(gw_audit.clone());
        let engine = Arc::new(ClusterEngine::new(
            Arc::new(Scraper::new(cfg).unwrap()),
            d.clone(),
        ));
        let m = d.clone();
        let app = pagesift::api::router(
            engine,
            pagesift::api::Config {
                extra_metrics: Some(Arc::new(move || m.metrics())),
                ..Default::default()
            },
        );
        (app, d, addr, gw_audit)
    });
    let each = pagesift::api::Engine::admitted_concurrency(&*worker_scraper()).unwrap();
    let workers: Vec<(String, tokio::runtime::Runtime)> = ids
        .iter()
        .map(|id| (id.to_string(), worker_with(id, &[&addr], worker_scraper)))
        .collect();
    let scrape = |body: Value| {
        let app = app.clone();
        async move { call(&app, "POST", "/v2/scrape", Some(body)).await }
    };
    let first = rt.block_on(async {
        for i in 0.. {
            if d.capacity() == 2 * each {
                break;
            }
            assert!(i < 4000, "workers did not connect");
            tokio::time::sleep(Duration::from_millis(5)).await;
        }
        // robots：网关判定遵守，工人本机设置不遵守也照样被挡
        let blocked = scrape(json!({"url": format!("{base}/private/1"), "formats": ["markdown"]})).await;
        assert_eq!(blocked["code"], "ROBOTS_DISALLOWED", "{blocked}");
        let allowed = scrape(json!({"url": format!("{base}/private/1"), "formats": ["markdown"], "ignoreRobotsTxt": true})).await;
        assert_eq!(allowed["success"], true, "{allowed}");
        // 第一次：还没有学到的数据，按冷启动规则渲染
        let v = scrape(json!({"url": format!("{base}/app/1"), "formats": ["markdown"], "explain": true})).await;
        assert_eq!(v["success"], true, "{v}");
        v
    });
    let step = render_step(&first["data"]);
    assert_eq!(step["learned"], false, "{step}");
    assert!(
        first["data"]["markdown"]
            .as_str()
            .unwrap_or_default()
            .contains("only exists after the script"),
        "the page was not rendered: {first}"
    );

    // 杀掉学到的那个工人：站点换给另一个，它第一次就按网关下发的先验决策
    let site = SiteKey::of_domain(&pagesift_content::site_key(ip));
    let owner = match d.coordinator().owner_of(site) {
        Owner::Worker(w) => w.to_string(),
        o => panic!("site has no owner: {o:?}"),
    };
    let mut survivors = Vec::new();
    for (id, w) in workers {
        if id == owner {
            drop(w);
        } else {
            survivors.push(w);
        }
    }
    let second = rt.block_on(async {
        scrape(json!({"url": format!("{base}/app/2"), "formats": ["markdown"], "explain": true}))
            .await
    });
    assert_eq!(second["success"], true, "{second}");
    assert_eq!(render_step(&second["data"])["learned"], true, "{second}");
    assert!(matches!(d.coordinator().owner_of(site), Owner::Worker(w) if w.as_str() != owner));

    // 审计：工人的记录由网关追加，带工人 id；忽略 robots 的那次带 robotsIgnored
    let audit = rt.block_on(async {
        let metrics = {
            let req = Request::get("/metrics").body(Body::empty()).unwrap();
            let resp = app.clone().oneshot(req).await.unwrap();
            String::from_utf8(
                resp.into_body()
                    .collect()
                    .await
                    .unwrap()
                    .to_bytes()
                    .to_vec(),
            )
            .unwrap()
        };
        for id in ids {
            assert!(
                metrics.contains(&format!(
                    "pagesift_cluster_worker_pages_total{{worker=\"{id}\"}}"
                )),
                "{metrics}"
            );
        }
        assert!(
            metrics.contains("pagesift_cluster_worker_capacity{worker="),
            "{metrics}"
        );
        gw_audit.flush();
        std::fs::read_to_string(&audit_path).unwrap_or_default()
    });
    let recs: Vec<Value> = audit
        .lines()
        .filter_map(|l| serde_json::from_str(l).ok())
        .collect();
    let from_workers: Vec<&Value> = recs.iter().filter(|r| r["worker"].is_string()).collect();
    assert!(from_workers.len() >= 4, "{audit}");
    assert!(
        from_workers.iter().any(|r| r["robotsIgnored"].is_string()
            && r["url"].as_str().is_some_and(|u| u.contains("/private/1"))),
        "{audit}"
    );
    drop(survivors);
}
