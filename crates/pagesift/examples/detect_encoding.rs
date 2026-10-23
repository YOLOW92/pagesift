//! 给一个文件清单，逐个输出 pagesift 探测出的字符集编码名称。
//! 用法: `cargo run --release -p pagesift --example detect_encoding -- <清单文件>`
//! 输出格式 (TSV): `<文件路径>\t<探测出的编码>\t<耗时微秒>`

use std::io::{BufRead, Write};
use std::time::Instant;

fn main() {
    let list_path = std::env::args()
        .nth(1)
        .expect("usage: detect_encoding <list_file>");
    let out = std::io::stdout();
    let mut out = out.lock();

    let file = std::fs::File::open(list_path).expect("open list");
    for line in std::io::BufReader::new(file).lines() {
        let path_str = line.expect("read line");
        if path_str.trim().is_empty() {
            continue;
        }
        let Ok(bytes) = std::fs::read(&path_str) else {
            continue;
        };

        let t = Instant::now();
        let decoded = pagesift_net::decode(&bytes, None, None);
        let us = t.elapsed().as_micros();

        let enc_name = decoded.encoding_name;
        writeln!(out, "{path_str}\t{enc_name}\t{us}").unwrap();
    }
}
