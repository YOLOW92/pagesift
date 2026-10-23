//! 0.60 P0 基准测试专用：流水线五大处理阶段独立切片测量
//!
//! 分别测量：
//! 1. 任务调度与去重 (Frontier & Scheduling)
//! 2. HTTP 网络传输与套接字 (Network Transport)
//! 3. HTML 流式解析与 DOM 构建 (HTML Parsing)
//! 4. 正文与元数据抽取 (Content Extraction)
//! 5. 结果聚合与 Hash 校验 (Output Sinks)
//!
//! 精确定位 80 核环境下的各阶段物理瓶颈。

use std::sync::Arc;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::time::Instant;

use clap::Parser;

#[derive(Parser, Debug)]
#[command(author, version, about = "PageSift P0 Pipeline Stages Benchmark")]
pub struct Opts {
    #[arg(short, long, default_value = "http://127.0.0.1:19800")]
    pub origin_url: String,
    #[arg(short, long, default_value_t = 100_000)]
    pub iterations: usize,
    #[arg(short, long, default_value_t = 80)]
    pub concurrency: usize,
}

#[tokio::main]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    let opts = Opts::parse();
    println!("============================================================");
    println!("  PageSift 0.50 Pipeline Stages Breakdown Benchmark");
    println!("  Iterations: {} | Concurrency: {}", opts.iterations, opts.concurrency);
    println!("============================================================");

    // 1. Stage 1: Frontier & Scheduling
    println!("\n[Stage 1/5] Measuring URL Normalization, Blake3 Hashing & Frontier Dispatch...");
    let s1_res = bench_stage1_scheduling(opts.iterations, opts.concurrency).await;
    println!("  -> Throughput: {:.2} ops/sec | Elapsed: {:.3}s", s1_res.qps, s1_res.elapsed);

    // 2. Stage 2: Network Transport (Raw HTTP fetch without extraction)
    println!("\n[Stage 2/5] Measuring Raw HTTP Socket I/O & Connection Reuse (Zero Parsing)...");
    let s2_res = bench_stage2_transport(&opts.origin_url, opts.iterations.min(50_000), opts.concurrency).await?;
    println!("  -> Throughput: {:.2} req/sec | Elapsed: {:.3}s | Bandwidth: {:.2} MB/s", s2_res.qps, s2_res.elapsed, s2_res.mb_per_sec);

    // 3. Stage 3: HTML Parsing & DOM Sanitization
    println!("\n[Stage 3/5] Measuring HTML Parsing & Tree Construction (Pure DOM Parsing)...");
    let s3_res = bench_stage3_html_parsing(opts.iterations, opts.concurrency).await;
    println!("  -> Throughput: {:.2} pages/sec | Elapsed: {:.3}s", s3_res.qps, s3_res.elapsed);

    // 4. Stage 4: Content Extraction & Markdown Distillation
    println!("\n[Stage 4/5] Measuring Full Content Distillation, Tables & Markdown Formatting...");
    let s4_res = bench_stage4_extraction(opts.iterations, opts.concurrency).await;
    println!("  -> Throughput: {:.2} pages/sec | Elapsed: {:.3}s", s4_res.qps, s4_res.elapsed);

    // 5. Stage 5: Output Sinks & Blake3 Verification
    println!("\n[Stage 5/5] Measuring Data Serialization, Blake3 Digest & Channel Sinks...");
    let s5_res = bench_stage5_output(opts.iterations, opts.concurrency).await;
    println!("  -> Throughput: {:.2} pages/sec | Elapsed: {:.3}s", s5_res.qps, s5_res.elapsed);

    println!("\n============================================================");
    println!("  PIPELINE STAGE CEILING SUMMARY (80 vCPUs AMD EPYC 9B45)");
    println!("============================================================");
    println!("  Stage 1 (Scheduling):       {:>10.2} ops/sec", s1_res.qps);
    println!("  Stage 2 (Network I/O):       {:>10.2} req/sec", s2_res.qps);
    println!("  Stage 3 (HTML Parsing):      {:>10.2} pages/sec", s3_res.qps);
    println!("  Stage 4 (Content Extract):   {:>10.2} pages/sec", s4_res.qps);
    println!("  Stage 5 (Output Sinks):      {:>10.2} pages/sec", s5_res.qps);
    println!("------------------------------------------------------------");
    let bottleneck = if s4_res.qps < s2_res.qps && s4_res.qps < s3_res.qps {
        "Stage 4 (Content Extraction & Distillation)"
    } else if s3_res.qps < s2_res.qps {
        "Stage 3 (HTML Parsing)"
    } else {
        "Stage 2 (Network Socket Transport)"
    };
    println!("  Identified Primary Pipeline Bottleneck: {}", bottleneck);
    println!("============================================================");

    Ok(())
}

struct StageResult {
    elapsed: f64,
    qps: f64,
    mb_per_sec: f64,
}

async fn bench_stage1_scheduling(iterations: usize, concurrency: usize) -> StageResult {
    let t0 = Instant::now();
    let per_worker = iterations / concurrency;
    let mut handles = Vec::with_capacity(concurrency);

    for w in 0..concurrency {
        handles.push(tokio::spawn(async move {
            let mut hasher = blake3::Hasher::new();
            for i in 0..per_worker {
                let url = format!("https://s{}.domain.org/p/article-item-{}-{}", (w * 13 + i) % 1000, w, i);
                let _norm = url.to_lowercase();
                hasher.update(_norm.as_bytes());
                let _hash = hasher.finalize();
                hasher.reset();
            }
        }));
    }

    for h in handles {
        let _ = h.await;
    }

    let elapsed = t0.elapsed().as_secs_f64();
    StageResult {
        elapsed,
        qps: iterations as f64 / elapsed,
        mb_per_sec: 0.0,
    }
}

async fn bench_stage2_transport(origin_url: &str, iterations: usize, concurrency: usize) -> Result<StageResult, Box<dyn std::error::Error>> {
    let client = reqwest::Client::builder()
        .pool_max_idle_per_host(concurrency * 2)
        .tcp_nodelay(true)
        .build()?;

    let t0 = Instant::now();
    let per_worker = iterations / concurrency;
    let total_bytes = Arc::new(AtomicUsize::new(0));
    let mut handles = Vec::with_capacity(concurrency);

    for w in 0..concurrency {
        let c = client.clone();
        let base = origin_url.to_string();
        let bytes_ref = total_bytes.clone();
        handles.push(tokio::spawn(async move {
            for i in 0..per_worker {
                let target_url = format!("{}/p/{}", base, (w * per_worker + i) % 100_000);
                if let Ok(resp) = c.get(&target_url).send().await {
                    if let Ok(body) = resp.bytes().await {
                        bytes_ref.fetch_add(body.len(), Ordering::Relaxed);
                    }
                }
            }
        }));
    }

    for h in handles {
        let _ = h.await;
    }

    let elapsed = t0.elapsed().as_secs_f64();
    let bytes = total_bytes.load(Ordering::Relaxed);
    Ok(StageResult {
        elapsed,
        qps: iterations as f64 / elapsed,
        mb_per_sec: (bytes as f64 / 1_048_576.0) / elapsed,
    })
}

async fn bench_stage3_html_parsing(iterations: usize, concurrency: usize) -> StageResult {
    let sample_html = r#"
        <!DOCTYPE html><html><head><title>Test Article</title></head>
        <body><header><nav><a href="/">Home</a></nav></header>
        <main><article>
            <h1>High Concurrency Stream Title</h1>
            <p class="lead">Introductory paragraph testing pure DOM tree construction speed.</p>
            <p>Second paragraph with some <b>bold</b> and <i>italic</i> formatting.</p>
            <table><tr><th>H1</th><th>H2</th></tr><tr><td>D1</td><td>D2</td></tr></table>
        </article></main>
        <footer><p>Footer notice</p></footer></body></html>
    "#;

    let t0 = Instant::now();
    let per_worker = iterations / concurrency;
    let mut handles = Vec::with_capacity(concurrency);

    for _ in 0..concurrency {
        handles.push(tokio::spawn(async move {
            for _ in 0..per_worker {
                let dom = pagesift_html::Html::parse(sample_html);
                let _nodes = dom.root().descendants().count();
            }
        }));
    }

    for h in handles {
        let _ = h.await;
    }

    let elapsed = t0.elapsed().as_secs_f64();
    StageResult {
        elapsed,
        qps: iterations as f64 / elapsed,
        mb_per_sec: 0.0,
    }
}

async fn bench_stage4_extraction(iterations: usize, concurrency: usize) -> StageResult {
    let sample_html = r#"
        <!DOCTYPE html><html><head><title>Full Extraction Article</title></head>
        <body><div class="sidebar">Ads and unrelated navigation links</div>
        <main id="main-content">
            <h1>Scalable Web Extraction with Bounded Memory</h1>
            <p class="author">By Systems Architect</p>
            <p>The primary advantage of streaming architecture is deterministic resource bounding.</p>
            <table border="1">
                <tr><th>Metric</th><th>Spider</th><th>PageSift</th></tr>
                <tr><td>Memory</td><td>9000 MB</td><td>188 MB</td></tr>
            </table>
            <pre><code>fn test_code() { println!("Hello"); }</code></pre>
        </main>
        <div id="footer">Copyright 2026 PageSift</div></body></html>
    "#;

    let t0 = Instant::now();
    let per_worker = iterations / concurrency;
    let mut handles = Vec::with_capacity(concurrency);

    for _ in 0..concurrency {
        handles.push(tokio::spawn(async move {
            let opts = pagesift_content::Options::default();
            for _ in 0..per_worker {
                let _doc = pagesift_content::process(sample_html, &opts);
            }
        }));
    }

    for h in handles {
        let _ = h.await;
    }

    let elapsed = t0.elapsed().as_secs_f64();
    StageResult {
        elapsed,
        qps: iterations as f64 / elapsed,
        mb_per_sec: 0.0,
    }
}

async fn bench_stage5_output(iterations: usize, concurrency: usize) -> StageResult {
    let sample_html = "<html><body><article><h1>Title</h1><p>Benchmark paragraph.</p></article></body></html>";
    let sample_doc = pagesift_content::process(sample_html, &pagesift_content::Options::default());

    let t0 = Instant::now();
    let per_worker = iterations / concurrency;
    let mut handles = Vec::with_capacity(concurrency);

    for _ in 0..concurrency {
        let doc = sample_doc.clone();
        handles.push(tokio::spawn(async move {
            let mut hasher = blake3::Hasher::new();
            for _ in 0..per_worker {
                hasher.update(doc.markdown.as_bytes());
                let _h = hasher.finalize();
                hasher.reset();
                let _json = serde_json::to_string(&serde_json::json!({
                    "title": doc.metadata.title,
                    "len": doc.markdown.len(),
                    "hash": _h.to_hex().as_str()
                })).unwrap();
            }
        }));
    }

    for h in handles {
        let _ = h.await;
    }

    let elapsed = t0.elapsed().as_secs_f64();
    StageResult {
        elapsed,
        qps: iterations as f64 / elapsed,
        mb_per_sec: 0.0,
    }
}
