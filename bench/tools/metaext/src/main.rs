//! D7 结构化数据：把 PageSift 从 JSON-LD / microdata / meta 里抽出的字段打成 JSONL。
use std::io::{BufRead, BufReader, Write};

fn main() -> Result<(), Box<dyn std::error::Error>> {
    let mut args = std::env::args().skip(1);
    let man = args.next().ok_or("usage: <manifest> <out>")?;
    let out = args.next().ok_or("usage: <manifest> <out>")?;
    let mut w = std::io::BufWriter::new(std::fs::File::create(&out)?);
    let mut n = 0usize;
    for line in BufReader::new(std::fs::File::open(&man)?).lines() {
        let line = line?;
        if line.trim().is_empty() { continue; }
        let row: serde_json::Value = serde_json::from_str(&line)?;
        let path = row["path"].as_str().unwrap_or("");
        let url = row["url"].as_str().unwrap_or("");
        let html = std::fs::read_to_string(path).unwrap_or_default();
        let opts = pagesift_content::Options {
            base_url: url::Url::parse(url).ok(),
            ..Default::default()
        };
        let doc = pagesift_content::process(&html, &opts);
        let m = &doc.metadata;
        writeln!(w, "{}", serde_json::json!({
            "id": row["id"],
            "author": m.author,
            "published": m.published_time,
            "modified": m.modified_time,
            "title": m.title,
            "canonical": m.canonical_url,
            "is_article": m.is_article,
        }))?;
        n += 1;
    }
    eprintln!("rows={n}");
    Ok(())
}
