//! 批量转换：把目录里的 *.html 转成同名 .md，打印每页平均耗时。`--full` 时整页转换（不只正文）。
//! `cargo run --release -p pagesift-content --example md_convert -- <html 目录> <输出目录> [--full]`
use std::time::Instant;

use pagesift_content::{Options, process};

fn main() {
    let mut args = std::env::args().skip(1);
    let src = args.next().expect("usage: md_convert <src> <out> [--full]");
    let out = args.next().expect("usage: md_convert <src> <out> [--full]");
    let full = args.next().as_deref() == Some("--full");
    std::fs::create_dir_all(&out).unwrap();
    let opts = Options {
        only_main_content: !full,
        ..Options::default()
    };
    let mut files: Vec<_> = std::fs::read_dir(&src)
        .unwrap()
        .filter_map(|e| Some(e.ok()?.path()))
        .filter(|p| p.extension().is_some_and(|e| e == "html"))
        .collect();
    files.sort();
    let mut total = 0.0;
    for f in &files {
        let html = String::from_utf8_lossy(&std::fs::read(f).unwrap()).into_owned();
        let t = Instant::now();
        let doc = process(&html, &opts);
        total += t.elapsed().as_secs_f64();
        let name = f.file_stem().unwrap().to_string_lossy();
        std::fs::write(format!("{out}/{name}.md"), doc.markdown).unwrap();
    }
    println!(
        "pagesift: {:.2} ms/page",
        total * 1000.0 / files.len() as f64
    );
}
