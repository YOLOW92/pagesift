//! 粗略计时：`cargo run --release -p pagesift-content --example timing -- page.html...`

use std::time::Instant;

fn main() {
    for path in std::env::args().skip(1) {
        let html = std::fs::read_to_string(&path).expect("read file");
        let runs = 200;
        let t = Instant::now();
        for _ in 0..runs {
            std::hint::black_box(scraper::Html::parse_document(&html));
        }
        let parse = t.elapsed().as_secs_f64() * 1000.0 / runs as f64;
        let t = Instant::now();
        for _ in 0..runs {
            std::hint::black_box(pagesift_content::process(
                &html,
                &pagesift_content::Options::default(),
            ));
        }
        let total = t.elapsed().as_secs_f64() * 1000.0 / runs as f64;
        println!(
            "{path}: {} KB, parse {parse:.2} ms, total {total:.2} ms (extract+markdown {:.2} ms)",
            html.len() / 1024,
            total - parse
        );
    }
}
