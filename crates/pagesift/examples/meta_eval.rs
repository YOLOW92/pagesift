//! 元数据评测的 pagesift 一侧：逐个读 HTML 文件，输出提取到的元数据和处理耗时（JSON 行）。
//! `cargo run --release -p pagesift --example meta_eval -- <文件清单>`，清单每行一个路径，
//! 可选用制表符跟一个 URL（给了就作为页面地址，用于 URL 里的日期等线索）。

use std::io::{BufRead, Write};
use std::time::Instant;

use pagesift_content::{Options, extract_metadata};

fn main() {
    let list = std::env::args().nth(1).expect("usage: meta_eval <list>");
    let out = std::io::stdout();
    let mut out = out.lock();
    for line in std::io::BufReader::new(std::fs::File::open(list).unwrap()).lines() {
        let line = line.unwrap();
        let (path, url) = match line.split_once('\t') {
            Some((p, u)) => (p.to_string(), Some(u.to_string())),
            None => (line.clone(), None),
        };
        let bytes = std::fs::read(&path).unwrap();
        let html = pagesift_net::decode(&bytes, None, None).text;
        let opts = Options {
            base_url: url.as_deref().and_then(|u| url::Url::parse(u).ok()),
            ..Options::default()
        };
        let t = Instant::now();
        let m = extract_metadata(&html, &opts);
        let ms = t.elapsed().as_secs_f64() * 1000.0;
        let rec = serde_json::json!({
            "file": path,
            "published": m.published_time,
            "modified": m.modified_time,
            "title": m.title,
            "author": m.author,
            "language": m.language,
            "ms": ms,
        });
        writeln!(out, "{rec}").unwrap();
    }
}
