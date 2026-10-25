//! D5 RAG 分块 + BM25 检索评测。
//!
//! 对每个问题：用指定分块策略切目标语料，BM25 打分，取 top-k 块，
//! 判断答案字符区间是否被某个 top-k 块覆盖 —— 命中即算 recall@k 成功。
//!
//! usage: pagesift-d5eval <questions.jsonl> <corpora_dir> <strategy> <k> <out.json>
//!   strategy: recursive|paragraphs|sentences|headings|fixed|sliding|overlapping|whole

use std::collections::HashMap;
use std::io::{BufRead, Write};

use pagesift_extract::bm25::{bm25_scores, Bm25Options};
use pagesift_extract::chunk::{chunk, ChunkStrategy};

fn strategy_of(name: &str) -> Option<Option<ChunkStrategy>> {
    Some(match name {
        "whole" => None, // 不切块，整篇一个块
        "recursive" => Some(ChunkStrategy::Recursive { size: 512 }),
        "paragraphs" => Some(ChunkStrategy::Paragraphs),
        "sentences" => Some(ChunkStrategy::Sentences),
        "headings" => Some(ChunkStrategy::Headings { max_level: 6 }),
        "fixed" => Some(ChunkStrategy::FixedTokens { size: 512 }),
        "sliding" => Some(ChunkStrategy::SlidingWindow { window: 512, step: 256 }),
        "overlapping" => Some(ChunkStrategy::Overlapping { window: 512, overlap: 128 }),
        _ => return None,
    })
}

fn main() -> Result<(), Box<dyn std::error::Error>> {
    let mut a = std::env::args().skip(1);
    let qfile = a.next().ok_or("usage: <questions.jsonl> <corpora_dir> <strategy> <k> <out.json>")?;
    let cdir = a.next().ok_or("missing corpora dir")?;
    let sname = a.next().ok_or("missing strategy")?;
    let k: usize = a.next().and_then(|s| s.parse().ok()).ok_or("missing k")?;
    let out = a.next().ok_or("missing out")?;

    let strat = strategy_of(&sname).ok_or_else(|| format!("unknown strategy {sname}"))?;

    // 语料缓存：corpus_id -> (原文, 块列表)
    let mut cache: HashMap<String, (String, Vec<pagesift_extract::chunk::Chunk>)> = HashMap::new();
    let mut hits = 0usize;
    let mut total = 0usize;
    let mut by_corpus: HashMap<String, (usize, usize)> = HashMap::new();
    let mut total_chunks = 0usize;
    let mut total_tokens = 0usize;
    let t0 = std::time::Instant::now();
    let mut detail = std::fs::File::create(&out)?;

    for line in std::io::BufReader::new(std::fs::File::open(&qfile)?).lines() {
        let line = line?;
        if line.trim().is_empty() {
            continue;
        }
        let q: serde_json::Value = serde_json::from_str(&line)?;
        let cid = q["corpus_id"].as_str().unwrap_or("").to_string();
        let question = q["question"].as_str().unwrap_or("");
        let refs: Vec<(usize, usize)> = q["references"]
            .as_array()
            .map(|v| {
                v.iter()
                    .map(|r| {
                        (
                            r["start_index"].as_u64().unwrap_or(0) as usize,
                            r["end_index"].as_u64().unwrap_or(0) as usize,
                        )
                    })
                    .collect()
            })
            .unwrap_or_default();
        if refs.is_empty() {
            continue;
        }

        let entry = cache.entry(cid.clone()).or_insert_with(|| {
            let p = format!("{cdir}/{cid}.md");
            let text = std::fs::read_to_string(&p).unwrap_or_default();
            let chunks = match &strat {
                None => vec![pagesift_extract::chunk::Chunk {
                    index: 0,
                    text: text.clone(),
                    heading_path: Vec::new(),
                    start: 0,
                    end: text.len(),
                    tokens: 0,
                }],
                Some(s) => chunk(&text, s).unwrap_or_default(),
            };
            (text, chunks)
        });

        let (text, chunks) = entry;
        if chunks.is_empty() {
            continue;
        }
        total_chunks += chunks.len();
        total_tokens += chunks.iter().map(|c| c.tokens).sum::<usize>();

        // BM25 检索：用块的文本打分
        let texts: Vec<&str> = chunks.iter().map(|c| c.text.as_str()).collect();
        let scores = bm25_scores(&texts, question, &Bm25Options::default());
        let mut idx: Vec<usize> = (0..chunks.len()).collect();
        idx.sort_by(|&x, &y| {
            scores[y]
                .partial_cmp(&scores[x])
                .unwrap_or(std::cmp::Ordering::Equal)
        });
        let top: Vec<usize> = idx.into_iter().take(k).collect();

        // 命中判定：某个 top-k 块覆盖了任一答案区间
        let mut ok = false;
        for &ci in &top {
            let c = &chunks[ci];
            for &(s, e) in &refs {
                if c.start <= s && c.end >= e {
                    ok = true;
                    break;
                }
                // 块与答案区间有实质重叠（≥80% 的答案被覆盖）也算命中
                let lo = c.start.max(s);
                let hi = c.end.min(e);
                if hi > lo && (hi - lo) as f64 >= (e - s) as f64 * 0.8 {
                    ok = true;
                    break;
                }
            }
            if ok {
                break;
            }
        }

        total += 1;
        if ok {
            hits += 1;
        }
        let e = by_corpus.entry(cid.clone()).or_insert((0, 0));
        e.1 += 1;
        if ok {
            e.0 += 1;
        }
        let _ = text.len();
        writeln!(
            detail,
            "{}",
            serde_json::json!({"q": question, "corpus": cid, "ok": ok, "chunks": chunks.len()})
        )?;
    }

    let wall = t0.elapsed().as_secs_f64();
    let summary = serde_json::json!({
        "strategy": sname, "k": k,
        "questions": total, "hits": hits,
        "recall_at_k": if total > 0 { hits as f64 / total as f64 } else { 0.0 },
        "total_chunks": total_chunks,
        "avg_chunks_per_corpus": total_chunks as f64 / cache.len().max(1) as f64,
        "avg_tokens_per_chunk": total_tokens as f64 / total_chunks.max(1) as f64,
        "wall_s": (wall * 1000.0).round() / 1000.0,
        "by_corpus": by_corpus.iter().map(|(c, (h, t))| {
            serde_json::json!({"corpus": c, "hits": h, "total": t, "recall": *h as f64 / (*t).max(1) as f64})
        }).collect::<Vec<_>>(),
    });
    std::fs::write(&out, serde_json::to_string_pretty(&summary)?)?;
    eprintln!("{summary}");
    Ok(())
}
