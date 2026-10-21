//! queryFilter 评测的 pagesift 一侧。对每个问题，在它所属的语料里：
//! - 按单位（段落、递归分块）切分，用 pagesift 的 BM25（带标题路径）打分，输出按分数排好的区间；
//! - 输出默认 queryFilter（段落、阈值 1.0）实际保留的区间。
//!
//! `qf_ranks <语料目录> <questions.csv 转成的 JSON 行> <输出 JSON>`；下标按字符算。
use std::collections::BTreeMap;
use std::io::BufRead;

use pagesift_extract::{
    Bm25Options, ChunkStrategy, QueryFilterOptions, bm25_scores, chunk, filter_markdown_by_query,
};

fn char_index(text: &str) -> Vec<usize> {
    let mut at = vec![0usize; text.len() + 1];
    let mut n = 0;
    for (i, _) in text.char_indices() {
        at[i] = n;
        n += 1;
    }
    at[text.len()] = n;
    at
}

fn main() {
    let mut args = std::env::args().skip(1);
    let dir = args.next().unwrap();
    let qpath = args.next().unwrap();
    let out = args.next().unwrap();
    let mut texts = BTreeMap::new();
    for e in std::fs::read_dir(&dir).unwrap().flatten() {
        let p = e.path();
        texts.insert(
            p.file_stem().unwrap().to_string_lossy().into_owned(),
            std::fs::read_to_string(&p).unwrap(),
        );
    }
    let modes: Vec<(&str, ChunkStrategy)> = vec![
        ("para", ChunkStrategy::Paragraphs),
        ("rec50", ChunkStrategy::Recursive { size: 50 }),
        ("rec100", ChunkStrategy::Recursive { size: 100 }),
        ("rec200", ChunkStrategy::Recursive { size: 200 }),
        ("rec100ctx", ChunkStrategy::Recursive { size: 100 }),
    ];
    // 每份语料、每种单位只切一次
    let mut units: BTreeMap<(String, &str), Vec<pagesift_extract::Chunk>> = BTreeMap::new();
    for (name, text) in &texts {
        for (m, st) in &modes {
            units.insert((name.clone(), *m), chunk(text, st).unwrap());
        }
    }
    let idx: BTreeMap<&String, Vec<usize>> =
        texts.iter().map(|(k, v)| (k, char_index(v))).collect();
    let mut result: BTreeMap<String, BTreeMap<String, Vec<[usize; 2]>>> = BTreeMap::new();
    let (mut filter_ms, mut nq) = (0.0f64, 0usize);
    for line in std::io::BufReader::new(std::fs::File::open(qpath).unwrap()).lines() {
        let q: serde_json::Value = serde_json::from_str(&line.unwrap()).unwrap();
        let (qid, question, corpus) = (
            q["id"].as_str().unwrap(),
            q["question"].as_str().unwrap(),
            q["corpus"].as_str().unwrap(),
        );
        let ci = &idx[&corpus.to_string()];
        // 新的 queryFilter（默认参数）实际保留的区间
        let t0 = std::time::Instant::now();
        let f = filter_markdown_by_query(&texts[corpus], question, &QueryFilterOptions::default());
        filter_ms += t0.elapsed().as_secs_f64() * 1000.0;
        nq += 1;
        result.entry("pagesift-default".into()).or_default().insert(
            qid.to_string(),
            f.ranges.iter().map(|r| [ci[r.start], ci[r.end]]).collect(),
        );
        for (m, _) in &modes {
            let us = &units[&(corpus.to_string(), *m)];
            let mut docs: Vec<String> = us
                .iter()
                .map(|c| {
                    if c.heading_path.is_empty() {
                        c.text.clone()
                    } else {
                        format!("{}\n{}", c.heading_path.join(" "), c.text)
                    }
                })
                .collect();
            if m.ends_with("ctx") {
                // 打分时带上前后相邻的单位
                docs = (0..us.len())
                    .map(|i| {
                        let a = i.saturating_sub(1);
                        let b = (i + 2).min(us.len());
                        docs[a..b].join("\n")
                    })
                    .collect();
            }
            let opts = Bm25Options {
                block_weights: false,
                threshold: None,
                ..Default::default()
            };
            let scores = bm25_scores(&docs, question, &opts);
            let mut order: Vec<usize> = (0..us.len()).filter(|&i| scores[i] > 0.0).collect();
            order.sort_by(|&a, &b| scores[b].total_cmp(&scores[a]).then(a.cmp(&b)));
            let ranked = order
                .iter()
                .map(|&i| [ci[us[i].start], ci[us[i].end]])
                .collect();
            result
                .entry(format!("pagesift-{m}"))
                .or_default()
                .insert(qid.to_string(), ranked);
            if *m == "rec100" {
                let top = order.first().map_or(0.0, |&i| scores[i]);
                let mut emit = |name: String, mut kept: Vec<usize>| {
                    kept.sort();
                    result.entry(name).or_default().insert(
                        qid.to_string(),
                        kept.iter()
                            .map(|&i| [ci[us[i].start], ci[us[i].end]])
                            .collect(),
                    );
                };
                emit(
                    "rec100-abs1.0-default".into(),
                    order
                        .iter()
                        .copied()
                        .filter(|&i| scores[i] >= 1.0)
                        .collect(),
                );
                for r in [0.3f32, 0.5, 0.7] {
                    emit(
                        format!("rec100-rel{r}-default"),
                        order
                            .iter()
                            .copied()
                            .filter(|&i| scores[i] >= r * top)
                            .collect(),
                    );
                }
                // 相对阈值 0.5，另外保底：不足 floor 个 token 时按分数继续补到 floor
                for floor in [300usize, 500, 800, 1000, 1300] {
                    let mut used = 0usize;
                    let kept: Vec<usize> = order
                        .iter()
                        .copied()
                        .filter(|&i| {
                            let keep = scores[i] >= 0.5 * top || used < floor;
                            if keep {
                                used += us[i].tokens;
                            }
                            keep
                        })
                        .collect();
                    emit(format!("rec100-floor{floor}-default"), kept);
                }
                for k in [3usize, 5, 10] {
                    emit(
                        format!("rec100-top{k}-default"),
                        order.iter().copied().take(k).collect(),
                    );
                }
            }
            if *m == "para" {
                // 默认 queryFilter：分数 >= 1.0 的段落，按原文顺序
                let mut kept: Vec<usize> = (0..us.len()).filter(|&i| scores[i] >= 1.0).collect();
                kept.sort();
                result
                    .entry("pagesift-old-default".into())
                    .or_default()
                    .insert(
                        qid.to_string(),
                        kept.iter()
                            .map(|&i| [ci[us[i].start], ci[us[i].end]])
                            .collect(),
                    );
            }
        }
    }
    std::fs::write(out, serde_json::to_string(&result).unwrap()).unwrap();
    eprintln!(
        "queryFilter: {:.2} ms/question (whole document each time)",
        filter_ms / nq as f64
    );
}
