//! 在 pagesift bench 的输入上跑 Rust 同类库，输出 `pagesift bench --external` 用的 JSON lines，
//! 或支持 `--md-dir <tool> <html_dir> <out_dir>` 用于 Markdown 保真度评测。
//!
//!     pagesift-compare-rust <crw|dom_smoothie|htmd|html-to-markdown-rs> <manifest.jsonl> <out.jsonl>
//!     pagesift-compare-rust --md-dir <htmd|html-to-markdown-rs|crw|dom_smoothie> <html_dir> <out_dir>

use std::fs::File;
use std::io::{BufRead, BufReader, BufWriter, Write};
use std::path::PathBuf;
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

fn htmd_convert(html: &str, _url: &str) -> String {
    let converter = htmd::HtmlToMarkdown::builder()
        .skip_tags(vec!["script", "style", "head", "noscript", "svg"])
        .build();
    converter.convert(html).unwrap_or_default()
}

fn html_to_markdown_rs_convert(html: &str, url: &str) -> String {
    let opts = html_to_markdown_rs::ConversionOptions {
        base_url: if url.is_empty() { None } else { Some(url.to_string()) },
        extract_metadata: false,
        ..Default::default()
    };
    html_to_markdown_rs::convert(html, Some(opts))
        .ok()
        .and_then(|r| r.content)
        .unwrap_or_default()
}

fn pagesift_native(html: &str, url: &str) -> String {
    let mut opts = pagesift_content::Options::default();
    if let Ok(u) = url::Url::parse(url) {
        opts.base_url = Some(u);
    }
    pagesift_content::process(html, &opts).markdown
}

fn rs_trafilatura_convert(html: &str, _url: &str) -> String {
    let opts = rs_trafilatura::Options {
        output_markdown: true,
        ..Default::default()
    };
    rs_trafilatura::extract_with_options(html, &opts)
        .ok()
        .and_then(|r| r.content_markdown.or(Some(r.content_text)))
        .unwrap_or_default()
}

fn select_tool(name: &str) -> fn(&str, &str) -> String {
    match name {
        "pagesift" | "pagesift-native" => pagesift_native,
        "crw" => crw,
        "dom_smoothie" => dom_smoothie,
        "htmd" => htmd_convert,
        "html-to-markdown-rs" => html_to_markdown_rs_convert,
        "rs-trafilatura" | "rs_trafilatura" => rs_trafilatura_convert,
        _ => panic!("usage: <pagesift|crw|dom_smoothie|htmd|html-to-markdown-rs|rs-trafilatura> <manifest.jsonl> <out.jsonl>"),
    }
}

fn run_md_dir(tool_name: &str, src: &str, out: &str) {
    let tool = select_tool(tool_name);
    let out_dir = PathBuf::from(out).join(tool_name);
    std::fs::create_dir_all(&out_dir).expect("create out dir");
    let mut files: Vec<PathBuf> = std::fs::read_dir(src)
        .expect("read src dir")
        .filter_map(|e| Some(e.ok()?.path()))
        .filter(|p| p.extension().is_some_and(|e| e == "html"))
        .collect();
    files.sort();
    let mut total = 0.0;
    for f in &files {
        let html = String::from_utf8_lossy(&std::fs::read(f).expect("read html")).into_owned();
        let t = Instant::now();
        let md = tool(&html, "");
        total += t.elapsed().as_secs_f64();
        let stem = f.file_stem().unwrap().to_string_lossy();
        std::fs::write(out_dir.join(format!("{stem}.md")), md).expect("write md");
    }
    println!("{tool_name}: {:.2} ms/page", total * 1000.0 / files.len().max(1) as f64);
}

fn main() {
    let args: Vec<String> = std::env::args().collect();
    if args.get(1).map(String::as_str) == Some("--md-dir") {
        run_md_dir(&args[2], &args[3], &args[4]);
        return;
    }
    let tool = select_tool(args.get(1).map(String::as_str).unwrap_or(""));
    let manifest = BufReader::new(File::open(&args[2]).expect("open manifest"));
    let lines: Vec<String> = manifest
        .lines()
        .map(|l| l.expect("read line"))
        .filter(|l| !l.trim().is_empty())
        .collect();
    let n_threads = if std::env::var("PAGESIFT_ONE_CORE").as_deref() == Ok("1") {
        1
    } else {
        std::thread::available_parallelism().map_or(8, |n| n.get())
    };
    let chunk_size = lines.len().div_ceil(n_threads.max(1)).max(1);
    let results: Vec<String> = std::thread::scope(|s| {
        let handles: Vec<_> = lines
            .chunks(chunk_size)
            .map(|chunk| {
                s.spawn(move || {
                    chunk
                        .iter()
                        .map(|line| {
                            let row: serde_json::Value =
                                serde_json::from_str(line).expect("parse manifest row");
                            let bytes = std::fs::read(row["path"].as_str().expect("path"))
                                .expect("read html");
                            let html = String::from_utf8_lossy(&bytes).into_owned();
                            let url = row["base_url"]
                                .as_str()
                                .or(row["url"].as_str())
                                .unwrap_or("");
                            let t = Instant::now();
                            let markdown = tool(&html, url);
                            let ms = t.elapsed().as_secs_f64() * 1000.0;
                            serde_json::json!({ "id": row["id"], "markdown": markdown, "ms": ms })
                                .to_string()
                        })
                        .collect::<Vec<_>>()
                })
            })
            .collect();
        handles.into_iter().flat_map(|h| h.join().unwrap()).collect()
    });
    let mut out = BufWriter::new(File::create(&args[3]).expect("create output"));
    for rec in results {
        writeln!(out, "{rec}").expect("write output");
    }
}
