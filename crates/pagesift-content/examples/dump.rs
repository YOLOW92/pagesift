//! 把评测输入清单里的每页转成 Markdown，写成 `pagesift bench --external` 能读的 JSON lines。
//! `cargo run --release -p pagesift-content --example dump -- bench/data/inputs/manifest.jsonl out.jsonl [full]`
//! 第三个参数是 `full` 时关掉只取正文，用来看整页能拿到的上限。

use std::io::{BufRead, Write};

fn main() {
    let mut args = std::env::args().skip(1);
    let manifest = args.next().expect("usage: dump <manifest> <out> [full]");
    let out = args.next().expect("usage: dump <manifest> <out> [full]");
    let full = args.next().as_deref() == Some("full");
    let dir = std::path::Path::new(&manifest)
        .parent()
        .unwrap()
        .to_path_buf();
    let mut w = std::io::BufWriter::new(std::fs::File::create(out).unwrap());
    for line in std::io::BufReader::new(std::fs::File::open(&manifest).unwrap()).lines() {
        let v: serde_json::Value = serde_json::from_str(&line.unwrap()).unwrap();
        let name = std::path::Path::new(v["path"].as_str().unwrap())
            .file_name()
            .unwrap();
        let path = dir.join(name);
        let html = String::from_utf8_lossy(&std::fs::read(path).unwrap()).into_owned();
        let opts = pagesift_content::Options {
            base_url: v["base_url"].as_str().and_then(|u| url::Url::parse(u).ok()),
            only_main_content: !full,
            ..Default::default()
        };
        let doc = pagesift_content::process(&html, &opts);
        let row = serde_json::json!({
            "id": v["id"],
            "markdown": doc.markdown,
            "title": doc.metadata.title,
            "description": doc.metadata.description,
        });
        serde_json::to_writer(&mut w, &row).unwrap();
        w.write_all(b"\n").unwrap();
    }
}
