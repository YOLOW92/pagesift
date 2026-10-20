//! 在真实页面上跑 CSS/XPath schema 提取，给 `perf stat -e instructions` 计数用（越低越好）：
//!   cargo build --release -p pagesift-extract --example extract_bench
//!   perf stat -e instructions target/release/examples/extract_bench <css|xpath|none> /path/to/inputs
//! `none` 只读文件，用来扣掉 I/O 的指令数。

use std::hint::black_box;

use pagesift_extract::{JsonCssExtractor, JsonXPathExtractor, Schema};

const CSS: &[&str] = &[
    r#"{"baseSelector": "a[href]", "fields": [
        {"name": "text", "type": "text"},
        {"name": "href", "type": "attribute", "attribute": "href"}]}"#,
    r#"{"baseSelector": "ul, ol", "fields": [
        {"name": "items", "selector": "> li", "type": "list", "fields": [{"name": "t", "type": "text"}]},
        {"name": "first", "selector": "li:first-child a", "type": "attribute", "attribute": "href"}]}"#,
    r#"{"baseSelector": "article, section, div.post, div.product, .card", "fields": [
        {"name": "title", "selector": "h1, h2, h3", "type": "text"},
        {"name": "img", "selector": "img[src]", "type": "attribute", "attribute": "src"},
        {"name": "price", "selector": "[class*=price]", "type": "regex", "pattern": "([\\d.,]+)"},
        {"name": "body", "selector": "p:not(:empty)", "type": "text"}]}"#,
    r#"{"baseSelector": "meta[property^='og:'], meta[name]", "fields": [
        {"name": "k", "type": "attribute", "attribute": "name"},
        {"name": "v", "type": "attribute", "attribute": "content"}]}"#,
    r#"{"baseSelector": "table tr", "fields": [
        {"name": "cells", "selector": "td", "type": "list", "fields": [{"name": "c", "type": "text"}]},
        {"name": "next", "source": "+ tr", "selector": "td", "type": "text"}]}"#,
];

const XPATH: &[&str] = &[
    r#"{"baseSelector": "//a[@href]", "fields": [
        {"name": "text", "type": "text"},
        {"name": "href", "selector": "@href", "type": "text"}]}"#,
    r#"{"baseSelector": "//ul|//ol", "fields": [
        {"name": "items", "selector": "./li", "type": "list", "fields": [{"name": "t", "type": "text"}]}]}"#,
    r#"{"baseSelector": "//article|//section", "fields": [
        {"name": "title", "selector": ".//h2", "type": "text"},
        {"name": "img", "selector": ".//img/@src", "type": "text"}]}"#,
    r#"{"baseSelector": "//meta[@name]", "fields": [
        {"name": "v", "selector": "@content", "type": "text"}]}"#,
];

fn main() {
    let args: Vec<String> = std::env::args().skip(1).collect();
    let (mode, dir) = match args.as_slice() {
        [m, d] => (m.as_str(), d.as_str()),
        _ => {
            eprintln!("usage: extract_bench <css|xpath|none> <dir>");
            std::process::exit(2);
        }
    };
    let mut files: Vec<_> = std::fs::read_dir(dir)
        .expect("input dir")
        .filter_map(|e| e.ok().map(|e| e.path()))
        .filter(|p| p.extension().is_some_and(|x| x == "html"))
        .collect();
    files.sort();
    let css: Vec<_> = CSS
        .iter()
        .map(|s| JsonCssExtractor::new(&s.parse::<Schema>().expect("schema")).expect("css"))
        .collect();
    let xpath: Vec<_> = XPATH
        .iter()
        .map(|s| JsonXPathExtractor::new(&s.parse::<Schema>().expect("schema")).expect("xpath"))
        .collect();
    let (mut items, mut bytes) = (0usize, 0usize);
    for f in &files {
        let html = String::from_utf8_lossy(&std::fs::read(f).expect("read")).into_owned();
        bytes += html.len();
        match mode {
            "css" => {
                for e in &css {
                    let v = e.extract(&html);
                    items += v.len();
                    black_box(v);
                }
            }
            "xpath" => {
                for e in &xpath {
                    // 嵌套过深的页面会返回 TooDeep，跳过
                    if let Ok(v) = e.extract(&html) {
                        items += v.len();
                        black_box(v);
                    }
                }
            }
            "none" => {
                black_box(&html);
            }
            _ => panic!("mode must be css, xpath or none"),
        }
    }
    println!(
        "{mode}: {} pages, {bytes} bytes, {items} items",
        files.len()
    );
}
