use std::time::Instant;
use pagesift_extract::{ChunkStrategy, chunk};

fn main() {
    let text = "PageSift delivers ultra-fast RAG chunking and semantic boundary detection. ".repeat(20000);
    let bytes = text.len();
    println!("Document size: {:.2} MB", bytes as f64 / 1_000_000.0);

    let strategy = ChunkStrategy::SlidingWindow { window: 200, step: 100 };
    // Warmup
    let _ = chunk(&text, &strategy).unwrap();

    let iters = 20;
    let t0 = Instant::now();
    for _ in 0..iters {
        let chunks = chunk(&text, &strategy).unwrap();
        std::hint::black_box(chunks);
    }
    let elapsed = t0.elapsed().as_secs_f64() / iters as f64;
    let mb_per_sec = (bytes as f64 / 1_000_000.0) / elapsed;

    println!("Sliding window latency: {:.2} ms", elapsed * 1000.0);
    println!("Sliding window throughput: {:.2} MB/s", mb_per_sec);

    let fixed_strategy = ChunkStrategy::FixedTokens { size: 200 };
    let t0 = Instant::now();
    for _ in 0..iters {
        let chunks = chunk(&text, &fixed_strategy).unwrap();
        std::hint::black_box(chunks);
    }
    let elapsed = t0.elapsed().as_secs_f64() / iters as f64;
    let mb_per_sec = (bytes as f64 / 1_000_000.0) / elapsed;

    println!("Fixed tokens latency: {:.2} ms", elapsed * 1000.0);
    println!("Fixed tokens throughput: {:.2} MB/s", mb_per_sec);
}
