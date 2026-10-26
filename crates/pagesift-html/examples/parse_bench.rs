//! 只测 HTML 解析：页面全部预载入内存，单线程解析 `轮数` 遍，报告每遍耗时（越低越好）和最好一遍。
//! `cargo run --release -p pagesift-html --example parse_bench -- <manifest.jsonl> [轮数]`
//! manifest 每行是带 `path` 字段的 JSON。
use std::time::Instant;

fn main() {
    let mut args = std::env::args().skip(1);
    let manifest = args
        .next()
        .expect("usage: parse_bench <manifest.jsonl> [rounds]");
    let rounds: usize = args.next().and_then(|s| s.parse().ok()).unwrap_or(5);
    let pages: Vec<String> = std::fs::read_to_string(&manifest)
        .unwrap()
        .lines()
        .filter_map(|l| {
            let i = l.find("\"path\"")?;
            let rest = &l[i + 6..];
            let s = rest.find('"')? + 1;
            let e = s + rest[s..].find('"')?;
            std::fs::read(&rest[s..e]).ok()
        })
        .map(|b| String::from_utf8_lossy(&b).into_owned())
        .collect();
    let mut best = f64::MAX;
    let mut nodes = 0;
    for _ in 0..rounds {
        let t = Instant::now();
        nodes = 0;
        for p in &pages {
            nodes += std::hint::black_box(pagesift_html::Html::parse(p)).len();
        }
        let ms = t.elapsed().as_secs_f64() * 1e3;
        best = best.min(ms);
        print!("{ms:.1} ");
    }
    println!("| pages={} nodes={nodes} best_ms={best:.1}", pages.len());
}
