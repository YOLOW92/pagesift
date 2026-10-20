//! 性能剖析负载：把一个目录下的所有页面单线程反复处理，给 perf 采样用。
//! `cargo run --release -p pagesift-content --example profile -- bench/data/inputs 3`

use std::time::Instant;

fn main() {
    let mut args = std::env::args().skip(1);
    let dir = args.next().expect("usage: profile <dir> [passes]");
    let passes: usize = args.next().map_or(3, |p| p.parse().expect("passes"));
    let mut pages: Vec<String> = std::fs::read_dir(&dir)
        .expect("read dir")
        .filter_map(|e| std::fs::read(e.ok()?.path()).ok())
        .map(|b| String::from_utf8_lossy(&b).into_owned())
        .collect();
    pages.sort_by_key(String::len);
    let bytes: usize = pages.iter().map(String::len).sum();
    let opts = pagesift_content::Options::default();
    let t = Instant::now();
    for _ in 0..passes {
        for html in &pages {
            std::hint::black_box(pagesift_content::process(html, &opts));
        }
    }
    let secs = t.elapsed().as_secs_f64();
    let n = pages.len() * passes;
    println!(
        "{n} pages, {:.1} MB/pass: {:.2} ms/page mean, {:.0} pages/s, {:.1} MB/s (1 thread)",
        bytes as f64 / 1e6,
        secs * 1000.0 / n as f64,
        n as f64 / secs,
        (bytes * passes) as f64 / 1e6 / secs
    );
}
