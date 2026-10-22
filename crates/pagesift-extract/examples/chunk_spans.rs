//! 分块评测的 pagesift 一侧：把每个语料按给定策略分块，输出每块的字符区间（JSON）。
//! `chunk_spans <策略 JSON> <输出文件> <语料.md>...`，输出 `{"语料名": [[起, 止], ...]}`，下标按字符算。
use std::collections::BTreeMap;
use std::time::Instant;

use pagesift_extract::{ChunkStrategy, chunk};

fn main() {
    let mut args = std::env::args().skip(1);
    let strategy: ChunkStrategy =
        serde_json::from_str(&args.next().expect("strategy")).expect("strategy JSON");
    let out = args.next().expect("output");
    let inputs: Vec<(String, String)> = args
        .map(|path| {
            let name = std::path::Path::new(&path)
                .file_stem()
                .unwrap()
                .to_string_lossy()
                .into_owned();
            let text = std::fs::read_to_string(&path).unwrap();
            (name, text)
        })
        .collect();
    let t = Instant::now();
    let chunked: Vec<Vec<pagesift_extract::Chunk>> = std::thread::scope(|s| {
        let handles: Vec<_> = inputs
            .iter()
            .map(|(_, text)| {
                let strategy = &strategy;
                s.spawn(move || chunk(text, strategy).unwrap())
            })
            .collect();
        handles.into_iter().map(|h| h.join().unwrap()).collect()
    });
    let secs = t.elapsed().as_secs_f64();
    let all: BTreeMap<String, Vec<[usize; 2]>> = std::thread::scope(|s| {
        let handles: Vec<_> = inputs
            .iter()
            .zip(&chunked)
            .map(|((name, text), chunks)| {
                s.spawn(move || {
                    let mut char_at = vec![0usize; text.len() + 1];
                    let mut n = 0;
                    for (i, _) in text.char_indices() {
                        char_at[i] = n;
                        n += 1;
                    }
                    char_at[text.len()] = n;
                    let spans: Vec<[usize; 2]> = chunks
                        .iter()
                        .map(|c| [char_at[c.start], char_at[c.end]])
                        .collect();
                    (name.clone(), spans)
                })
            })
            .collect();
        handles.into_iter().map(|h| h.join().unwrap()).collect()
    });
    std::fs::write(&out, serde_json::to_string(&all).unwrap()).unwrap();
    eprintln!("chunking took {:.2} ms in total", secs * 1000.0);
}
