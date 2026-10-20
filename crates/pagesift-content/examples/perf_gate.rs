//! 性能回归线：在确定性生成的合成页面上测 `process` 的中位耗时，超过上限就以非零状态退出。
//! 上限留了 CI 机器波动的余量，用来抓平方级复杂度这类明显退化；
//! PRD 的性能目标在真实快照上用 `pagesift bench` 考核。
//! CI 里跑：`cargo run --release -p pagesift-content --example perf_gate`

use std::fmt::Write;
use std::process::ExitCode;
use std::time::Instant;

use pagesift_content::{Options, process};

const RUNS: usize = 50;

fn paragraph(i: usize) -> String {
    format!(
        "<p>Paragraph {i} explains the topic, with <a href=\"/ref/{i}\">a reference</a>, some \
         <strong>emphasis</strong>, <code>inline_code({i})</code>, commas, clauses, and enough \
         words to look like real prose that a reader would actually want to keep.</p>"
    )
}

fn chrome() -> String {
    let mut s = String::from("<header><nav><ul>");
    for i in 0..30 {
        let _ = write!(s, "<li><a href=\"/nav/{i}\">Section {i}</a></li>");
    }
    s.push_str("</ul></nav></header>");
    s
}

/// 博客式页面：导航、侧边栏、正文、评论、页脚。
fn blog(target_bytes: usize) -> String {
    let mut s = format!(
        "<!doctype html><html><head><title>Blog</title></head><body>{}",
        chrome()
    );
    s.push_str("<aside class=\"sidebar\"><ul>");
    for i in 0..40 {
        let _ = write!(s, "<li><a href=\"/popular/{i}\">Popular post {i}</a></li>");
    }
    s.push_str("</ul></aside><article><h1>Title</h1>");
    let mut i = 0;
    while s.len() < target_bytes {
        if i % 8 == 0 {
            let _ = write!(
                s,
                "<h2>Part {i}</h2><ul><li>point</li><li>another point</li></ul>"
            );
        }
        if i % 13 == 0 {
            s.push_str("<pre><code class=\"language-rust\">fn main() {\n    println!(\"hi\");\n}</code></pre>");
        }
        s.push_str(&paragraph(i));
        i += 1;
    }
    s.push_str("</article><footer>footer links</footer></body></html>");
    s
}

/// 维基式页面：层层嵌套的 section、大量引用链接和表格。
fn wiki(target_bytes: usize) -> String {
    let mut s = format!(
        "<!doctype html><html><head><title>Wiki</title></head><body>{}",
        chrome()
    );
    s.push_str("<main><div class=\"mw-parser-output\">");
    let mut i = 0;
    while s.len() < target_bytes {
        s.push_str("<section><h2>Heading</h2>");
        for depth in 0..6 {
            let _ = write!(s, "<section><h3>Sub {depth}</h3><div><div>");
            s.push_str(&paragraph(i));
            s.push_str("<table><tr><th>a</th><th>b</th></tr><tr><td>1</td><td>2</td></tr></table>");
            i += 1;
        }
        for _ in 0..6 {
            s.push_str("</div></div></section>");
        }
        s.push_str("</section>");
    }
    s.push_str("</div></main></body></html>");
    s
}

fn median_ms(html: &str) -> f64 {
    let opts = Options::default();
    let mut times: Vec<f64> = (0..RUNS)
        .map(|_| {
            let t = Instant::now();
            std::hint::black_box(process(html, &opts));
            t.elapsed().as_secs_f64() * 1000.0
        })
        .collect();
    times.sort_by(f64::total_cmp);
    times[RUNS / 2]
}

fn main() -> ExitCode {
    let cases = [
        ("blog 100KB", blog(100 * 1024), 8.0),
        ("wiki 250KB", wiki(250 * 1024), 30.0),
    ];
    let mut ok = true;
    for (name, html, limit) in &cases {
        let ms = median_ms(html);
        let verdict = if ms <= *limit { "ok" } else { "TOO SLOW" };
        println!("{name}: median {ms:.2} ms (limit {limit} ms) {verdict}");
        ok &= ms <= *limit;
    }
    if ok {
        ExitCode::SUCCESS
    } else {
        ExitCode::FAILURE
    }
}
