//! Benchmark driver for structured data extraction.
//! Reads file list from argument, outputs TSV: `<path>\t<elapsed_us>\t<json_ld_count>\t<microdata_count>\t<og_keys>`

use pagesift_extract::extract_structured;
use std::io::{BufRead, Write};
use std::time::Instant;

fn main() {
    let args: Vec<String> = std::env::args().collect();
    if args.len() < 2 {
        eprintln!("usage: bench_structured <file_list>");
        std::process::exit(1);
    }
    let list_file = &args[1];
    let file = std::fs::File::open(list_file).expect("open list");
    let out = std::io::stdout();
    let mut out = out.lock();

    for line in std::io::BufReader::new(file).lines() {
        let path = line.expect("read line");
        if path.trim().is_empty() {
            continue;
        }
        let Ok(bytes) = std::fs::read(&path) else {
            continue;
        };
        let html = String::from_utf8_lossy(&bytes);

        let t0 = Instant::now();
        let res = extract_structured(&html);
        let us = t0.elapsed().as_micros();

        writeln!(
            out,
            "{path}\t{us}\t{}\t{}\t{}",
            res.json_ld.len(),
            res.microdata.len(),
            res.opengraph.len()
        )
        .unwrap();
    }
}
