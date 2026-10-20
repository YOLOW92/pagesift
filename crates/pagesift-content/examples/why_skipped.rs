//! 诊断：只取正文时删掉了哪些块、为什么。
//! `cargo run --release -p pagesift-content --example why_skipped -- page.html [最少字符数]`

fn main() {
    let mut args = std::env::args().skip(1);
    let path = args
        .next()
        .expect("usage: why_skipped <page.html> [min-chars]");
    let min: usize = args.next().map_or(0, |v| v.parse().unwrap());
    let html = String::from_utf8_lossy(&std::fs::read(&path).unwrap()).into_owned();
    let (relaxed, blocks) = pagesift_content::explain_skips(&html);
    println!("relaxed: {relaxed}");
    for b in blocks.iter().filter(|b| b.chars >= min) {
        println!(
            "{:>6}  {:<18} {}\n        {}",
            b.chars, b.reason, b.element, b.snippet
        );
    }
}
