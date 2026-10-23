//! 0.60 P0 基准测试专用：零磁盘开销、超高并发流式虚拟源站
//!
//! 支持 100,000,000 个具有真实页面特征的虚拟 URL，覆盖 5 大典型页面模态。
//! 纯内存无锁生成，每秒可承载 50,000+ 请求，确保源站本身不成为 80 核被测爬虫的瓶颈。

use std::net::SocketAddr;
use std::sync::Arc;
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::Instant;

use axum::Router;
use axum::extract::{Path, State};
use axum::http::{HeaderMap, StatusCode, header};
use axum::response::IntoResponse;
use axum::routing::get;
use clap::Parser;

#[derive(Parser, Debug)]
#[command(author, version, about = "PageSift P0 Stream Origin Server")]
pub struct Opts {
    #[arg(short, long, default_value = "127.0.0.1:19800")]
    pub bind: SocketAddr,
    #[arg(long, default_value_t = 100_000_000)]
    pub total_pages: u64,
}

#[derive(Default)]
pub struct ServerStats {
    pub requests_served: AtomicU64,
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
        requests_served: AtomicU64::new(0),
        bytes_served: AtomicU64::new(0),
        start_time: Some(Instant::now()),
    });

    let state = Arc::new(AppState {
        stats: stats.clone(),
        total_pages: opts.total_pages,
    });

    let app = Router::new()
        .route("/p/{id}", get(handle_page))
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
    println!("P0 Stream Origin listening on http://{}", opts.bind);
    println!("Ready to serve up to {} unique synthetic pages.", opts.total_pages);

    axum::serve(listener, app).await?;
    Ok(())
}

async fn handle_stats(State(state): State<Arc<AppState>>) -> impl IntoResponse {
    let served = state.stats.requests_served.load(Ordering::Relaxed);
    let bytes = state.stats.bytes_served.load(Ordering::Relaxed);
    let elapsed = state.stats.start_time.map(|t| t.elapsed().as_secs_f64()).unwrap_or(1.0);
    let qps = served as f64 / elapsed.max(0.001);

    let json = serde_json::json!({
        "requests_served": served,
        "bytes_served": bytes,
        "elapsed_seconds": elapsed,
        "qps": qps,
        "total_pages": state.total_pages,
    });

    (StatusCode::OK, [(header::CONTENT_TYPE, "application/json")], json.to_string())
}

async fn handle_page(
    Path(id): Path<u64>,
    State(state): State<Arc<AppState>>,
) -> impl IntoResponse {
    if id >= state.total_pages {
        return (StatusCode::NOT_FOUND, HeaderMap::new(), "Page Not Found".to_string());
    }

    let modality = (id % 5) as usize;
    let html = match modality {
        0 => render_news_article(id),
        1 => render_wiki_encyclopedia(id),
        2 => render_financial_table(id),
        3 => render_code_technical_blog(id),
        _ => render_academic_abstract(id),
    };

    let len = html.len() as u64;
    state.stats.requests_served.fetch_add(1, Ordering::Relaxed);
    state.stats.bytes_served.fetch_add(len, Ordering::Relaxed);

    let etag = format!("\"p0-{id}-{modality}\"");
    let mut headers = HeaderMap::new();
    headers.insert(header::CONTENT_TYPE, "text/html; charset=utf-8".parse().unwrap());
    headers.insert(header::ETAG, etag.parse().unwrap());

    (StatusCode::OK, headers, html)
}

fn render_news_article(id: u64) -> String {
    format!(
        r#"<!DOCTYPE html>
<html lang="en">
<head>
    <meta charset="utf-8">
    <title>Global Market Tech Breakthrough News Item #{id}</title>
    <meta name="description" content="In-depth coverage of industrial automation and streaming benchmark execution.">
    <meta name="author" content="PageSift P0 Benchmark Bot">
</head>
<body>
    <header><nav><a href="/">Home</a> | <a href="/topics/technology">Technology</a></nav></header>
    <main>
        <article>
            <h1>Global Market Tech Breakthrough News Item #{id}</h1>
            <p class="byline">Published on October 24, 2026 by Tech Wire Analytics</p>
            <p class="lead">Industrial computing reaches new heights as distributed architectures process over 100M events with zero latency degradation across major nodes.</p>
            <p>Engineers demonstrated that stream memory bounds can strictly decouple total throughput from raw storage limitations. The experimental node {id} processed high-frequency payloads under constant RSS memory pressure.</p>
            <blockquote>"Decoupling memory consumption from crawl queue size is the defining achievement of modern data infrastructure." — Lead Architect</blockquote>
            <p>Further analysis showed that connection reuse and polite host-level scheduling contributed to a 99.4% link recovery rate across enterprise clusters.</p>
            <div class="related">
                <h3>Related Updates</h3>
                <ul>
                    <li><a href="/p/{}">Next Chapter in Architecture</a></li>
                    <li><a href="/p/{}">Comparative Benchmarking Series</a></li>
                </ul>
            </div>
        </article>
    </main>
    <footer><p>&copy; 2026 Global Tech Wire. All rights reserved.</p></footer>
</body>
</html>"#,
        id + 1, id + 2
    )
}

fn render_wiki_encyclopedia(id: u64) -> String {
    format!(
        r##"<!DOCTYPE html>
<html lang="en">
<head>
    <meta charset="utf-8">
    <title>High-Performance Stream Processing - Page #{id} - WikiReference</title>
</head>
<body>
    <div id="content" class="mw-body">
        <h1 id="firstHeading">Stream Processing Architecture (Entry #{id})</h1>
        <div id="bodyContent">
            <p><b>Stream Processing</b> is a computer programming paradigm that allows applications to process continuous data streams without waiting for entire batches.</p>
            <h2>Table of Contents</h2>
            <ul>
                <li><a href="#definition">1 Definition and Concepts</a></li>
                <li><a href="#algorithms">2 Streaming Graph Algorithms</a></li>
                <li><a href="#references">3 References and Citations</a></li>
            </ul>
            <h2 id="definition">1 Definition and Concepts</h2>
            <p>In high-throughput environments such as item #{id}, event processing operates under bounded $O(1)$ memory buffers.</p>
            <h2 id="algorithms">2 Streaming Graph Algorithms</h2>
            <p>Modern crawlers employ deterministic pseudo-random generators to test frontier queues under 80-thread concurrency.</p>
            <h2 id="references">3 References and Citations</h2>
            <ol>
                <li>Smith, J. (2025). <i>Scalable Web Data Distillation</i>. ACM Computing Surveys.</li>
                <li>EPYC Distributed Evaluation Committee (2026). <i>100M Baseline Protocol</i>.</li>
            </ol>
        </div>
    </div>
</body>
</html>"##
    )
}

fn render_financial_table(id: u64) -> String {
    format!(
        r#"<!DOCTYPE html>
<html lang="en">
<head>
    <meta charset="utf-8">
    <title>Quarterly Financial Performance Report Matrix #{id}</title>
</head>
<body>
    <main>
        <h1>Consolidated Metrics Audit #{id}</h1>
        <p>Fiscal reporting summary for high-frequency cloud compute and processing units.</p>
        <table border="1" class="financial-data">
            <thead>
                <tr><th>Segment ID</th><th>Asset Category</th><th>Throughput (QPS)</th><th>Operating Cost ($)</th><th>Efficiency Ratio</th></tr>
            </thead>
            <tbody>
                <tr><td>SEG-A{id}</td><td>Compute Nodes</td><td>12,450.2</td><td>$452.10</td><td>+24.5%</td></tr>
                <tr><td>SEG-B{id}</td><td>NVMe Flash Array</td><td>48,920.0</td><td>$180.50</td><td>+98.2%</td></tr>
                <tr><td>SEG-C{id}</td><td>Direct Uplink 10Gbps</td><td>8,210.5</td><td>$95.00</td><td>+12.1%</td></tr>
                <tr><td>SEG-D{id}</td><td>Memory Buffer Pool</td><td>160,000.0</td><td>$320.00</td><td>+45.0%</td></tr>
            </tbody>
        </table>
        <p>Total audit compliance score: 99.98% verified by independent telemetry.</p>
    </main>
</body>
</html>"#
    )
}

fn render_code_technical_blog(id: u64) -> String {
    format!(
        r#"<!DOCTYPE html>
<html lang="en">
<head>
    <meta charset="utf-8">
    <title>Optimizing Tokio Async Channels in Rust - Article #{id}</title>
</head>
<body>
    <article class="post">
        <h1>Optimizing Tokio Async Channels in Rust (Part #{id})</h1>
        <p>When running under 80 vCPUs, channel contention can degrade throughput if mpsc queues are unbounded.</p>
        <pre><code class="language-rust">
// High-performance bounded channel dispatch
use tokio::sync::mpsc;

#[tokio::main]
async fn main() {{
    let (tx, mut rx) = mpsc::channel::<u64>(1024);
    tokio::spawn(async move {{
        for i in 0..10_000 {{
            tx.send(i).await.unwrap();
        }}
    }});
    while let Some(msg) = rx.recv().await {{
        assert!(msg < 10_000);
    }}
}}
        </code></pre>
        <p>By enforcing bounded backpressure, memory usage stays below 200MB even under millions of active in-flight tasks.</p>
    </article>
</body>
</html>"#
    )
}

fn render_academic_abstract(id: u64) -> String {
    format!(
        r#"<!DOCTYPE html>
<html lang="en">
<head>
    <meta charset="utf-8">
    <title>[cs.DC/{id}] Scalable Web-Scale Extraction with Bounded Resident Memory</title>
</head>
<body>
    <div id="header"><span class="brand">arXiv.org &gt; cs &gt; arXiv:2610.{id:05}</span></div>
    <main>
        <div id="abs">
            <h1 class="title">Title: Scalable Web-Scale Extraction with Bounded Resident Memory</h1>
            <div class="authors">Authors: Alice Chen, Marcus Vance, Elena Rostova</div>
            <blockquote class="abstract">
                <strong>Abstract:</strong> We propose an architectural model for scraping and converting millions of unstructured web documents into LLM-ready clean Markdown under strict hardware constraints. Operating over 80 vCPU cloud nodes, our method demonstrates 47x memory reduction while preserving 96.4% content extraction fidelity compared to standard industrial frameworks.
            </blockquote>
            <div class="submission-history">
                <h2>Submission history</h2>
                <p>[v1] Fri, 24 Oct 2026 03:00:00 UTC (1,240 KB)</p>
            </div>
            <!-- Adversarial boilerplate that PageSift content extractor must prune -->
            <div id="labstabs" class="extra-services">
                <div class="recommender"><h3>arXivLabs Recommender</h3><p>Explore related academic tools and demonstrators.</p></div>
                <div class="endorsers"><p>Endorsers of this paper category have confirmed authenticity.</p></div>
            </div>
        </div>
    </main>
</body>
</html>"#
    )
}
