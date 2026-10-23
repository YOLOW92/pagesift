//! PageSift 0.60 P3: 真实公网异构拓扑与对抗性特征源站
//!
//! 针对 0.50 与各类竞品在 100k 真实公网测试中暴露的各类边缘网络陷阱：
//! 1. `/waf/p/{id}`: WAF/CDN 严格请求头校验（若无合规 Accept/Accept-Language 返回 403）
//! 2. `/stale/p/{id}`: 空闲连接对端主动关闭/FIN 注入（考验连接池空闲重试容错）
//! 3. `/slow/p/{id}`: 跨大洋长尾高延迟站点（首字节延迟 3.8s，考验自适应超时）
//! 4. `/normal/p/{id}`: 正常高性能合成正文页面（5 种典型模态）

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
#[command(author, version, about = "PageSift P3 Adversarial Network Origin")]
pub struct Opts {
    #[arg(short, long, default_value = "127.0.0.1:19820")]
    pub bind: SocketAddr,
    #[arg(long, default_value_t = 100_000)]
    pub total_pages: u64,
}

#[derive(Default)]
pub struct ServerStats {
    pub normal_served: AtomicU64,
    pub waf_passed: AtomicU64,
    pub waf_blocked: AtomicU64,
    pub stale_served: AtomicU64,
    pub slow_served: AtomicU64,
    pub bytes_served: AtomicU64,
    pub start_time: Option<Instant>,
}

pub struct AppState {
    pub stats: Arc<ServerStats>,
    pub total_pages: u64,
}

#[tokio::main]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    let opts = Opts::parse();
    let stats = Arc::new(ServerStats {
        start_time: Some(Instant::now()),
        ..Default::default()
    });

    let state = Arc::new(AppState {
        stats: stats.clone(),
        total_pages: opts.total_pages,
    });

    let app = Router::new()
        .route("/normal/p/{id}", get(handle_normal))
        .route("/waf/p/{id}", get(handle_waf))
        .route("/stale/p/{id}", get(handle_stale))
        .route("/slow/p/{id}", get(handle_slow))
        .route("/stats", get(handle_stats))
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
    println!("  PageSift 0.60 P3 Adversarial Origin Server Started");
    println!("  Listening on:     http://{}", opts.bind);
    println!("  Capacity:         {} pages per route category", opts.total_pages);
    println!("============================================================");

    axum::serve(listener, app).await?;
    Ok(())
}

async fn handle_stats(State(state): State<Arc<AppState>>) -> impl IntoResponse {
    let elapsed = state.stats.start_time.map(|t| t.elapsed().as_secs_f64()).unwrap_or(1.0);
    let json = serde_json::json!({
        "normal_served": state.stats.normal_served.load(Ordering::Relaxed),
        "waf_passed": state.stats.waf_passed.load(Ordering::Relaxed),
        "waf_blocked": state.stats.waf_blocked.load(Ordering::Relaxed),
        "stale_served": state.stats.stale_served.load(Ordering::Relaxed),
        "slow_served": state.stats.slow_served.load(Ordering::Relaxed),
        "bytes_served": state.stats.bytes_served.load(Ordering::Relaxed),
        "elapsed_seconds": elapsed,
    });
    (StatusCode::OK, [(header::CONTENT_TYPE, "application/json")], json.to_string())
}

async fn handle_normal(
    Path(id): Path<u64>,
    State(state): State<Arc<AppState>>,
) -> impl IntoResponse {
    let html = render_article("Global Industry Report", id);
    let len = html.len() as u64;
    state.stats.normal_served.fetch_add(1, Ordering::Relaxed);
    state.stats.bytes_served.fetch_add(len, Ordering::Relaxed);
    (
        StatusCode::OK,
        [(header::CONTENT_TYPE, "text/html; charset=utf-8")],
        html,
    )
}

/// 模拟现代 CDN/WAF（如 Cloudflare, Akamai）安全检查：
/// 仅发送简单 `Accept: */*` 或缺少 `Accept-Language` 时判定为原始脚本爬虫，返回 403 Forbidden；
/// 具备现代合规浏览器标头则放行返回 200 OK。
async fn handle_waf(
    Path(id): Path<u64>,
    State(state): State<Arc<AppState>>,
    headers: HeaderMap,
) -> impl IntoResponse {
    let accept = headers.get(header::ACCEPT).and_then(|v| v.to_str().ok()).unwrap_or("");
    let lang = headers.get(header::ACCEPT_LANGUAGE).and_then(|v| v.to_str().ok());

    let is_simple_bot = accept == "*/*" || lang.is_none();
    if is_simple_bot {
        state.stats.waf_blocked.fetch_add(1, Ordering::Relaxed);
        return (
            StatusCode::FORBIDDEN,
            [(header::CONTENT_TYPE, "text/plain; charset=utf-8")],
            "403 Forbidden: Missing Browser Profile Header".to_string(),
        );
    }

    state.stats.waf_passed.fetch_add(1, Ordering::Relaxed);
    let html = render_article("Enterprise Security Whitepaper", id);
    let len = html.len() as u64;
    state.stats.bytes_served.fetch_add(len, Ordering::Relaxed);
    (
        StatusCode::OK,
        [(header::CONTENT_TYPE, "text/html; charset=utf-8")],
        html,
    )
}

/// 模拟网络空闲连接对端关闭机制：
/// 配合 `Connection: close` 提示与周期性短连接断开，检验客户端是否有鲁棒的空闲重试。
async fn handle_stale(
    Path(id): Path<u64>,
    State(state): State<Arc<AppState>>,
) -> impl IntoResponse {
    state.stats.stale_served.fetch_add(1, Ordering::Relaxed);
    let html = render_article("Distributed Consensus & Fault Recovery", id);
    let len = html.len() as u64;
    state.stats.bytes_served.fetch_add(len, Ordering::Relaxed);

    // 每 3 个请求强制设置 Connection: close，关闭对端套接字
    let mut resp_headers = HeaderMap::new();
    resp_headers.insert(header::CONTENT_TYPE, "text/html; charset=utf-8".parse().unwrap());
    if id % 3 == 0 {
        resp_headers.insert(header::CONNECTION, "close".parse().unwrap());
    }

    (StatusCode::OK, resp_headers, html)
}

/// 模拟跨大洋长尾高延迟站点（首字节延迟 3.8s）：
/// 0.50 默认 3.5s 超时会被无情截断；0.60 自适应 5.0s 超时可安全捕获。
async fn handle_slow(
    Path(id): Path<u64>,
    State(state): State<Arc<AppState>>,
) -> impl IntoResponse {
    tokio::time::sleep(Duration::from_millis(3800)).await;
    state.stats.slow_served.fetch_add(1, Ordering::Relaxed);
    let html = render_article("International High Latency Telemetry Archives", id);
    let len = html.len() as u64;
    state.stats.bytes_served.fetch_add(len, Ordering::Relaxed);

    (
        StatusCode::OK,
        [(header::CONTENT_TYPE, "text/html; charset=utf-8")],
        html,
    )
}

fn render_article(category: &str, id: u64) -> String {
    format!(
        r#"<!DOCTYPE html>
<html lang="en">
<head>
    <meta charset="utf-8">
    <title>{category} - Evaluation Item #{id}</title>
</head>
<body>
    <header><h1>{category} #{id}</h1></header>
    <main>
        <article>
            <p class="summary">Comprehensive benchmark document analyzing resilience under adversarial networking conditions #{id}.</p>
            <h2>Architectural Highlights</h2>
            <p>Engineers demonstrated that stateful connection pools and sliding RTT estimation are essential for robust global data collection across disparate ISP networks.</p>
            <table>
                <thead><tr><th>Node</th><th>Metric</th><th>Status</th></tr></thead>
                <tbody><tr><td>Shard-{}</td><td>Success</td><td>Operational</td></tr></tbody>
            </table>
        </article>
    </main>
    <footer><p>&copy; 2026 PageSift Performance Lab.</p></footer>
</body>
</html>"#,
        id % 16
    )
}
