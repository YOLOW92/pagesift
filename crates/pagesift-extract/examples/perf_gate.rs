//! 性能回归线：在约 100KB 的合成页面上测各功能的中位耗时（越低越好）。
//! CSS 提取、分块、BM25 超过 5ms 就以非零状态退出。
//! 运行：`cargo run --release -p pagesift-extract --example perf_gate`

use std::fmt::Write;
use std::hint::black_box;
use std::process::ExitCode;
use std::time::{Duration, Instant};

use pagesift_extract::{
    Bm25Options, BuiltinPattern, ChunkStrategy, JsonCssExtractor, JsonXPathExtractor,
    RegexExtractor, Schema, bm25_filter, chunk,
};
use pagesift_html::Html;

const RUNS: usize = 30;
const LIMIT: Duration = Duration::from_millis(5);
const TARGET: usize = 100 * 1024;

fn shop_page() -> String {
    let mut s =
        String::from("<!doctype html><html><head><title>Shop</title></head><body><nav><ul>");
    for i in 0..30 {
        let _ = write!(s, "<li><a href=\"/c/{i}\">Category {i}</a></li>");
    }
    s.push_str("</ul></nav><div class=\"grid\">");
    let mut i = 0;
    while s.len() < TARGET {
        let _ = write!(
            s,
            "<div class=\"product\" data-sku=\"SKU-{i}\"><a class=\"link\" href=\"/p/{i}\"><img src=\"/img/{i}.jpg\" alt=\"Item {i}\"></a>\
             <h3 class=\"name\">Product number {i}</h3><span class=\"price\">${i}.99</span>\
             <div class=\"rating\" title=\"4.{} out of 5\">stars</div><ul class=\"tags\"><li>tag-a</li><li>tag-b</li></ul>\
             <div class=\"seller\"><span class=\"seller-name\">Seller {i}</span></div></div>",
            i % 10
        );
        i += 1;
    }
    s.push_str("</div><footer>contact sales@shop.example</footer></body></html>");
    s
}

fn markdown() -> String {
    let mut s = String::from("# Guide\n\n");
    let mut i = 0;
    while s.len() < TARGET {
        if i % 10 == 0 {
            let _ = write!(s, "## Section {i}\n\n");
        }
        if i % 4 == 0 {
            let _ = write!(s, "### 小节 {i}\n\n");
        }
        if i % 13 == 0 {
            s.push_str("```rust\nfn main() {\n    println!(\"hi\");\n}\n```\n\n");
        }
        let _ = write!(
            s,
            "Paragraph {i} explains how the crawler fetches pages, e.g. with retries. Contact ops{i}@example.com \
             or visit https://example.com/docs/{i}. 这一段说明抓取引擎如何处理中文。它支持句号切分！\n\n"
        );
        i += 1;
    }
    s
}

fn median(mut f: impl FnMut()) -> Duration {
    f();
    let mut times: Vec<Duration> = (0..RUNS)
        .map(|_| {
            let t = Instant::now();
            f();
            t.elapsed()
        })
        .collect();
    times.sort();
    times[RUNS / 2]
}

fn main() -> ExitCode {
    let html = shop_page();
    let md = markdown();
    let css_schema: Schema = r#"{"baseSelector": "div.product",
        "baseFields": [{"name": "sku", "type": "attribute", "attribute": "data-sku"}],
        "fields": [
            {"name": "name", "selector": "h3.name", "type": "text"},
            {"name": "url", "selector": "a.link", "type": "attribute", "attribute": "href"},
            {"name": "price", "selector": ".price", "type": "regex", "pattern": "\\$([\\d.]+)"},
            {"name": "tags", "selector": "ul.tags li", "type": "list", "fields": [{"name": "t", "type": "text"}]},
            {"name": "seller", "selector": ".seller", "type": "nested", "fields": [{"name": "n", "selector": "span", "type": "text"}]}
        ]}"#
    .parse()
    .expect("valid schema");
    let xpath_schema: Schema = r#"{"baseSelector": "//div[@class='product']",
        "baseFields": [{"name": "sku", "type": "attribute", "attribute": "data-sku"}],
        "fields": [
            {"name": "name", "selector": "./h3", "type": "text"},
            {"name": "url", "selector": "./a/@href", "type": "text"},
            {"name": "price", "selector": "./span[@class='price']", "type": "regex", "pattern": "\\$([\\d.]+)"},
            {"name": "tags", "selector": "./ul/li", "type": "list", "fields": [{"name": "t", "type": "text"}]}
        ]}"#
    .parse()
    .expect("valid schema");
    let css = JsonCssExtractor::new(&css_schema).expect("valid css");
    let xpath = JsonXPathExtractor::new(&xpath_schema).expect("valid xpath");
    let regex = RegexExtractor::new::<&str, &str>(BuiltinPattern::ALL, &[]).expect("valid regex");
    let doc = Html::parse(&html);
    let items = css.extract_document(&doc).len();
    assert_eq!(
        items,
        xpath.extract_document(&doc).expect("xpath runs").len()
    );

    let paragraphs: Vec<String> = chunk(&md, &ChunkStrategy::Paragraphs)
        .expect("chunks")
        .into_iter()
        .map(|c| c.text)
        .collect();
    let bm25_opts = Bm25Options {
        top_k: Some(20),
        ..Default::default()
    };
    let strategies = [
        ("chunk headings", ChunkStrategy::Headings { max_level: 6 }),
        ("chunk paragraphs", ChunkStrategy::Paragraphs),
        ("chunk sentences", ChunkStrategy::Sentences),
        ("chunk fixed 256", ChunkStrategy::FixedTokens { size: 256 }),
        (
            "chunk sliding 256/128",
            ChunkStrategy::SlidingWindow {
                window: 256,
                step: 128,
            },
        ),
        (
            "chunk overlap 256/32",
            ChunkStrategy::Overlapping {
                window: 256,
                overlap: 32,
            },
        ),
    ];

    println!(
        "page {} KB ({items} products), markdown {} KB ({} blocks); median of {RUNS} runs, lower is better",
        html.len() / 1024,
        md.len() / 1024,
        paragraphs.len()
    );
    let mut failed = false;
    let mut report = |name: &str, gated: bool, d: Duration| {
        let over = gated && d > LIMIT;
        failed |= over;
        let gate = if gated {
            if over { "FAIL (>5ms)" } else { "ok (<5ms)" }
        } else {
            "-"
        };
        println!("{name:<28} {:>8.3} ms  {gate}", d.as_secs_f64() * 1e3);
    };
    report(
        "html parse (pagesift-html)",
        false,
        median(|| drop(black_box(Html::parse(&html)))),
    );
    report(
        "css extract",
        true,
        median(|| drop(black_box(css.extract_document(&doc)))),
    );
    report(
        "css parse+extract",
        false,
        median(|| drop(black_box(css.extract(&html)))),
    );
    report(
        "xpath extract",
        false,
        median(|| drop(black_box(xpath.extract_document(&doc)))),
    );
    report(
        "regex all builtins (md)",
        false,
        median(|| drop(black_box(regex.extract(&md)))),
    );
    for (name, s) in &strategies {
        report(name, true, median(|| drop(black_box(chunk(&md, s)))));
    }
    report(
        "bm25 top-20",
        true,
        median(|| {
            drop(black_box(bm25_filter(
                &paragraphs,
                "crawler 中文 retries",
                &bm25_opts,
            )))
        }),
    );
    if failed {
        ExitCode::FAILURE
    } else {
        ExitCode::SUCCESS
    }
}
