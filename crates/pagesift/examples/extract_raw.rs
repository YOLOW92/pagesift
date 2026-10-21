//! 编码鲁棒性测试用的 pagesift 适配器：输入是网站返回的原始字节（不一定是 UTF-8），和正常抓取一样
//! 先用 pagesift 的编码识别（BOM、`<meta>`、按顶级域提示的启发式检测）解码，再抽正文。
//! 输出和其他适配器一样是 `{id, markdown, ms}`，耗时含解码。
//! `cargo run --release -p pagesift --example extract_raw -- <manifest.jsonl> <out.jsonl>`
use std::io::Write;
use std::time::Instant;

use pagesift_content::{Options, process};

fn main() {
    let mut args = std::env::args().skip(1);
    let manifest = args
        .next()
        .expect("usage: extract_raw <manifest.jsonl> <out.jsonl>");
    let out = args
        .next()
        .expect("usage: extract_raw <manifest.jsonl> <out.jsonl>");
    let mut w = std::io::BufWriter::new(std::fs::File::create(&out).unwrap());
    for line in std::fs::read_to_string(&manifest).unwrap().lines() {
        if line.trim().is_empty() {
            continue;
        }
        let row: serde_json::Value = serde_json::from_str(line).unwrap();
        let bytes = std::fs::read(row["path"].as_str().unwrap()).unwrap();
        let url = row["base_url"]
            .as_str()
            .or(row["url"].as_str())
            .and_then(|u| url::Url::parse(u).ok());
        let tld = url.as_ref().and_then(|u| {
            u.host_str()
                .map(|h| h.rsplit('.').next().unwrap_or("").to_owned())
        });
        let t = Instant::now();
        let decoded = pagesift_net::decode(&bytes, None, tld.as_deref().map(str::as_bytes));
        let opts = Options {
            base_url: url,
            ..Options::default()
        };
        let doc = process(&decoded.text, &opts);
        let ms = t.elapsed().as_secs_f64() * 1000.0;
        let rec = serde_json::json!({"id": row["id"], "markdown": doc.markdown, "ms": ms});
        writeln!(w, "{rec}").unwrap();
    }
}
