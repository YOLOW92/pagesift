//! PageSift 0.60 Phase P4: WILD-X 1 极限挖掘战 — 混沌对抗超级虚拟源站
//!
//! 在 128 GB 内存隔离边界下，为 PageSift 0.60 提供五大极限对抗流量：
//! 1. `/chaos/deep_dom/{id}`: 150~300 层深层嵌套病态 DOM 与未闭合标签
//! 2. `/chaos/spider_trap/{id}`: 无限循环日历/分页陷阱链接
//! 3. `/chaos/backpressure/{id}`: 突发 503 / 429 背压风暴与 Retry-After 退避
//! 4. `/chaos/slowloris/{id}`: 慢速响应与套接字长时间占用
//! 5. `/chaos/normal/{id}`: 高密度富文本合成页面

use std::net::SocketAddr;
use std::sync::Arc;
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::{Duration, Instant};

use axum::Router;
use axum::extract::{Path, State};
use axum::http::{HeaderMap, StatusCode, header};
use axum::response::IntoResponse;
use axum::routing::get;
use clap::Parser;

#[derive(Parser, Debug)]
#[command(author, version, about = "PageSift WILD-X 1 Chaos Origin Server")]
pub struct Opts {
    #[arg(short, long, default_value = "127.0.0.1:19840")]
    pub bind: SocketAddr,
    #[arg(long, default_value_t = 100_000)]
    pub total_tasks: u64,
}

#[derive(Default)]
pub struct ChaosStats {
    pub deep_dom_served: AtomicU64,
    pub spider_traps_served: AtomicU64,
    pub backpressure_503: AtomicU64,
    pub backpressure_429: AtomicU64,
    pub backpressure_recovered: AtomicU64,
    pub slowloris_served: AtomicU64,
    pub normal_served: AtomicU64,
    pub total_bytes: AtomicU64,
    pub start_time: Option<Instant>,
}

pub struct AppState {
    pub stats: Arc<ChaosStats>,
    pub total_tasks: u64,
}

#[tokio::main]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    let opts = Opts::parse();
    let stats = Arc::new(ChaosStats {
        start_time: Some(Instant::now()),
        ..Default::default()
    });

    let state = Arc::new(AppState {
        stats: stats.clone(),
        total_tasks: opts.total_tasks,
    });

    let app = Router::new()
        .route("/chaos/deep_dom/{id}", get(handle_deep_dom))
        .route("/chaos/spider_trap/{id}", get(handle_spider_trap))
        .route("/chaos/backpressure/{id}", get(handle_backpressure))
        .route("/chaos/slowloris/{id}", get(handle_slowloris))
        .route("/chaos/normal/{id}", get(handle_normal))
        .route("/__metrics", get(handle_metrics))
        .with_state(state);

    let socket = socket2::Socket::new(
        socket2::Domain::IPV4,
        socket2::Type::STREAM,
        Some(socket2::Protocol::TCP),
    )?;
    socket.set_reuse_address(true)?;
    #[cfg(unix)]
    let _ = socket.set_reuse_port(true);
    socket.set_nonblocking(true)?;
    socket.bind(&opts.bind.into())?;
    socket.listen(8192)?;
    let std_listener: std::net::TcpListener = socket.into();
    let listener = tokio::net::TcpListener::from_std(std_listener)?;

    println!("============================================================");
    println!("  PageSift WILD-X 1 Chaos Adversarial Origin Running");
    println!("  Listening:   http://{}", opts.bind);
    println!("  Tasks Cap:   {} adversarial tasks", opts.total_tasks);
    println!("============================================================");

    axum::serve(listener, app).await?;
    Ok(())
}

async fn handle_metrics(State(state): State<Arc<AppState>>) -> impl IntoResponse {
    let elapsed = state.stats.start_time.map(|t| t.elapsed().as_secs_f64()).unwrap_or(1.0);
    let json = serde_json::json!({
        "deep_dom_served": state.stats.deep_dom_served.load(Ordering::Relaxed),
        "spider_traps_served": state.stats.spider_traps_served.load(Ordering::Relaxed),
        "backpressure_503": state.stats.backpressure_503.load(Ordering::Relaxed),
        "backpressure_429": state.stats.backpressure_429.load(Ordering::Relaxed),
        "backpressure_recovered": state.stats.backpressure_recovered.load(Ordering::Relaxed),
        "slowloris_served": state.stats.slowloris_served.load(Ordering::Relaxed),
        "normal_served": state.stats.normal_served.load(Ordering::Relaxed),
        "total_bytes_mb": state.stats.total_bytes.load(Ordering::Relaxed) as f64 / 1_048_576.0,
        "elapsed_seconds": elapsed,
    });
    (StatusCode::OK, [(header::CONTENT_TYPE, "application/json")], json.to_string())
}

/// 1. 病态深层嵌套 DOM（150 ~ 250 层 div + 未闭合段落 + 嵌套表格）
async fn handle_deep_dom(
    Path(id): Path<u64>,
    State(state): State<Arc<AppState>>,
) -> impl IntoResponse {
    let depth = 150 + ((id % 100) as usize);
    let mut html = String::with_capacity(32 * 1024);
    html.push_str("<!DOCTYPE html><html><head><title>Pathological DOM Stress #");
    html.push_str(&id.to_string());
    html.push_str("</title></head><body><header><h1>Stress Node ");
    html.push_str(&id.to_string());
    html.push_str("</h1></header><main>");

    // 疯狂嵌套 div
    for i in 0..depth {
        html.push_str(&format!(r#"<div id="layer-{}" class="nested-wrapper overflow-stress">"#, i));
    }
    html.push_str("<article><h2>Critical Payload Core</h2><p>This deep text payload tests recursion safety and stream buffer containment under maximum stack depth.</p>");
    html.push_str("<table><thead><tr><th>Metric</th><th>Reading</th></tr></thead><tbody><tr><td>DeepLevel</td><td>");
    html.push_str(&depth.to_string());
    html.push_str("</td></tr></tbody></table></article>");
    for _ in 0..depth {
        html.push_str("</div>");
    }
    html.push_str("</main></body></html>");

    let len = html.len() as u64;
    state.stats.deep_dom_served.fetch_add(1, Ordering::Relaxed);
    state.stats.total_bytes.fetch_add(len, Ordering::Relaxed);
    (StatusCode::OK, [(header::CONTENT_TYPE, "text/html; charset=utf-8")], html)
}

/// 2. 爬虫陷阱与无限循环链接
async fn handle_spider_trap(
    Path(id): Path<u64>,
    State(state): State<Arc<AppState>>,
) -> impl IntoResponse {
    let mut html = String::with_capacity(4096);
    html.push_str("<!DOCTYPE html><html><head><title>Spider Trap Calendar Loop</title></head><body>");
    html.push_str(&format!("<h1>Calendar Archive Node #{}</h1><nav><ul>", id));
    html.push_str(&format!(r#"<li><a href="/chaos/spider_trap/{}?session=sess_{}">Next Month</a></li>"#, id + 1, id));
    html.push_str(&format!(r#"<li><a href="/chaos/spider_trap/{}?page={}">Next Page</a></li>"#, id, id + 1));
    html.push_str(&format!(r#"<li><a href="/chaos/spider_trap/{}/more/more/more">Pathological Loop</a></li>"#, id));
    html.push_str(r#"</ul></nav><article><p>Valid anchor article describing trap mitigation.</p></article></body></html>"#);

    let len = html.len() as u64;
    state.stats.spider_traps_served.fetch_add(1, Ordering::Relaxed);
    state.stats.total_bytes.fetch_add(len, Ordering::Relaxed);
    (StatusCode::OK, [(header::CONTENT_TYPE, "text/html; charset=utf-8")], html)
}

/// 3. 突发 503 / 429 背压风暴（考验 AIMD 降速退避与重试恢复）
async fn handle_backpressure(
    Path(id): Path<u64>,
    State(state): State<Arc<AppState>>,
) -> impl IntoResponse {
    let seed = id % 10;
    if seed < 2 {
        state.stats.backpressure_503.fetch_add(1, Ordering::Relaxed);
        let mut headers = HeaderMap::new();
        headers.insert(header::RETRY_AFTER, "1".parse().unwrap());
        return (StatusCode::SERVICE_UNAVAILABLE, headers, "503 Service Temporarily Overloaded (Chaos Storm)".to_string());
    } else if seed == 2 {
        state.stats.backpressure_429.fetch_add(1, Ordering::Relaxed);
        let mut headers = HeaderMap::new();
        headers.insert(header::RETRY_AFTER, "1".parse().unwrap());
        return (StatusCode::TOO_MANY_REQUESTS, headers, "429 Too Many Requests (Rate Limit Backpressure)".to_string());
    }

    state.stats.backpressure_recovered.fetch_add(1, Ordering::Relaxed);
    let html = format!(
        "<!DOCTYPE html><html><head><title>Backpressure Handled #{}</title></head><body><article><h1>Recovered Node #{}</h1><p>Backpressure window cleared successfully, processing normal payload.</p></article></body></html>",
        id, id
    );
    let len = html.len() as u64;
    state.stats.total_bytes.fetch_add(len, Ordering::Relaxed);
    (StatusCode::OK, HeaderMap::new(), html)
}

/// 4. 慢速响应（延迟 2.0s 响应，考验套接字占用解构）
async fn handle_slowloris(
    Path(id): Path<u64>,
    State(state): State<Arc<AppState>>,
) -> impl IntoResponse {
    tokio::time::sleep(Duration::from_millis(2000)).await;
    state.stats.slowloris_served.fetch_add(1, Ordering::Relaxed);
    let html = format!(
        "<!DOCTYPE html><html><head><title>Slowloris Tolerated #{}</title></head><body><article><h1>Slow Node #{}</h1><p>Processed under delayed connection window without pool exhaustion.</p></article></body></html>",
        id, id
    );
    let len = html.len() as u64;
    state.stats.total_bytes.fetch_add(len, Ordering::Relaxed);
    (StatusCode::OK, [(header::CONTENT_TYPE, "text/html; charset=utf-8")], html)
}

/// 5. 正常高密度多模态正文页面
async fn handle_normal(
    Path(id): Path<u64>,
    State(state): State<Arc<AppState>>,
) -> impl IntoResponse {
    state.stats.normal_served.fetch_add(1, Ordering::Relaxed);
    let html = format!(
        r#"<!DOCTYPE html><html><head><title>WILD-X 1 Ground Truth Item #{}</title></head><body><header><h1>Autonomous Computing Article #{}</h1></header><main><article><p class="lead">Engineers executed high-velocity extraction across chaotic endpoints.</p><table><thead><tr><th>Metric</th><th>Value</th></tr></thead><tbody><tr><td>Status</td><td>Verified</td></tr><tr><td>Checksum</td><td>OK</td></tr></tbody></table><p>Full spectrum analysis guarantees zero memory growth across 128GB capacity.</p></article></main></body></html>"#,
        id, id
    );
    let len = html.len() as u64;
    state.stats.total_bytes.fetch_add(len, Ordering::Relaxed);
    (StatusCode::OK, [(header::CONTENT_TYPE, "text/html; charset=utf-8")], html)
}
