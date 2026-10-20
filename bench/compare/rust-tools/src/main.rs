//! 在 pagesift bench 的输入上跑 Rust 同类库，输出 `pagesift bench --external` 用的 JSON lines。
//!
//!     cargo run --release -- crw|dom_smoothie <manifest.jsonl> <out.jsonl>
//!
//! crw-extract 的调用方式照搬它自带的 examples/offline_corpus.rs（only_main_content 打开）。
//! dom_smoothie 用 Markdown 文本模式，标题按 Readability 的惯例单独给出，这里拼在正文前面。

use std::fs::File;
use std::io::{BufRead, BufReader, BufWriter, Write};
use std::time::Instant;

use crw_core::types::OutputFormat;
use crw_extract::ExtractOptions;

fn crw(html: &str, url: &str) -> String {
    let formats = [OutputFormat::Markdown];
    crw_extract::extract(ExtractOptions {
        raw_html: html,
        content_type: None,
        source_url: url,
        status_code: 200,
        rendered_with: Some("http".into()),
        elapsed_ms: 0,
        render_decision: None,
        credit_cost: 0,
        warnings: Vec::new(),
        formats: &formats,
        only_main_content: true,
        include_tags: &[],
        exclude_tags: &[],
        css_selector: None,
        xpath: None,
        chunk_strategy: None,
        query: None,
        filter_mode: None,
        top_k: None,
        domain_selectors: None,
        captured_responses: &[],
        llm_fallback: None,
        debug: false,
        debug_sink: None,
        normalize_tables: false,
    })
    .ok()
    .and_then(|d| d.markdown)
    .unwrap_or_default()
}

fn dom_smoothie(html: &str, url: &str) -> String {
    let cfg = dom_smoothie::Config {
        text_mode: dom_smoothie::TextMode::Markdown,
        ..Default::default()
    };
    let Ok(mut r) = dom_smoothie::Readability::new(html, Some(url), Some(cfg)) else {
        return String::new();
    };
    match r.parse() {
        Ok(a) if a.title.trim().is_empty() => a.text_content.to_string(),
        Ok(a) => format!("# {}\n\n{}", a.title.trim(), a.text_content),
        Err(_) => String::new(),
    }
}

fn main() {
    let args: Vec<String> = std::env::args().collect();
    let tool: fn(&str, &str) -> String = match args.get(1).map(String::as_str) {
        Some("crw") => crw,
        Some("dom_smoothie") => dom_smoothie,
        _ => panic!("usage: <crw|dom_smoothie> <manifest.jsonl> <out.jsonl>"),
    };
    let manifest = BufReader::new(File::open(&args[2]).expect("open manifest"));
    let mut out = BufWriter::new(File::create(&args[3]).expect("create output"));
    for line in manifest.lines() {
        let line = line.expect("read line");
        if line.trim().is_empty() {
            continue;
        }
        let row: serde_json::Value = serde_json::from_str(&line).expect("parse manifest row");
        let html = std::fs::read_to_string(row["path"].as_str().expect("path")).expect("read html");
        let url = row["base_url"].as_str().or(row["url"].as_str()).unwrap_or("");
        let t = Instant::now();
        let markdown = tool(&html, url);
        let ms = t.elapsed().as_secs_f64() * 1000.0;
        let rec = serde_json::json!({ "id": row["id"], "markdown": markdown, "ms": ms });
        writeln!(out, "{rec}").expect("write output");
    }
}
