//! BM25 相关性过滤，对应 Crawl4AI 的 `BM25ContentFilter`，输入是 Markdown 块。

use serde::Serialize;

use crate::chunk::{ChunkStrategy, chunk, is_cjk};

#[derive(Debug, Clone, PartialEq)]
pub struct Bm25Options {
    pub k1: f32,
    pub b: f32,
    /// 只保留分数最高的 k 块。
    pub top_k: Option<usize>,
    /// 只保留分数不低于阈值的块（Crawl4AI 的 `bm25_threshold`，默认 1.0）。
    pub threshold: Option<f32>,
    /// 去掉英文停用词。
    pub stopwords: bool,
    /// 按 Markdown 块类型加权，对应 Crawl4AI 的 `priority_tags`（h1 ×5、h2 ×4、h3 ×3、引用 ×2、代码 ×1.5）。
    pub block_weights: bool,
}

impl Default for Bm25Options {
    fn default() -> Self {
        Bm25Options {
            k1: 1.5,
            b: 0.75,
            top_k: None,
            threshold: Some(1.0),
            stopwords: true,
            block_weights: true,
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Serialize)]
pub struct ScoredBlock {
    pub index: usize,
    pub score: f32,
}

/// 分词：拉丁文按字母数字连续串切分并转小写；中日韩文字按字 bigram（单字时用 unigram）。
pub fn tokenize(text: &str, stopwords: bool) -> Vec<String> {
    let mut out = Vec::new();
    for_each_token(text, stopwords, &mut String::new(), |t| {
        out.push(t.to_owned())
    });
    out
}

pub(crate) fn for_each_token(
    text: &str,
    stopwords: bool,
    buf: &mut String,
    mut f: impl FnMut(&str),
) {
    let b = text.as_bytes();
    let mut pos = 0;
    while pos < b.len() {
        let byte = b[pos];
        if byte < 128 {
            if !byte.is_ascii_alphanumeric() {
                pos += 1;
                continue;
            }
            let start = pos;
            let mut has_upper = byte.is_ascii_uppercase();
            pos += 1;
            while pos < b.len() && b[pos].is_ascii_alphanumeric() {
                has_upper |= b[pos].is_ascii_uppercase();
                pos += 1;
            }
            if pos >= b.len() || b[pos] < 128 {
                if !has_upper {
                    let word = &text[start..pos];
                    if !(stopwords && is_stopword(word)) {
                        f(word);
                    }
                } else {
                    buf.clear();
                    for &c in &b[start..pos] {
                        buf.push((c as char).to_ascii_lowercase());
                    }
                    if !(stopwords && is_stopword(buf)) {
                        f(buf);
                    }
                }
                continue;
            }
            buf.clear();
            for &c in &b[start..pos] {
                buf.push((c as char).to_ascii_lowercase());
            }
            while pos < b.len() {
                let Some(n) = text[pos..].chars().next() else {
                    break;
                };
                if n.is_ascii_alphanumeric() {
                    buf.push(n.to_ascii_lowercase());
                } else if n.is_alphanumeric() && !is_cjk(n) {
                    push_lower(buf, n);
                } else {
                    break;
                }
                pos += n.len_utf8();
            }
            if !(stopwords && is_stopword(buf)) {
                f(buf);
            }
            continue;
        }
        let Some(c) = text[pos..].chars().next() else {
            break;
        };
        pos += c.len_utf8();
        if is_cjk(c) && c.is_alphabetic() {
            let mut prev = c;
            let mut emitted = false;
            while pos < b.len() && b[pos] >= 128 {
                let Some(n) = text[pos..].chars().next() else {
                    break;
                };
                if !(is_cjk(n) && n.is_alphabetic()) {
                    break;
                }
                buf.clear();
                buf.push(prev);
                buf.push(n);
                f(buf);
                emitted = true;
                prev = n;
                pos += n.len_utf8();
            }
            if !emitted {
                buf.clear();
                buf.push(c);
                f(buf);
            }
        } else if c.is_alphanumeric() {
            buf.clear();
            push_lower(buf, c);
            while pos < b.len() {
                let Some(n) = text[pos..].chars().next() else {
                    break;
                };
                if n.is_ascii_alphanumeric() {
                    buf.push(n.to_ascii_lowercase());
                } else if n.is_alphanumeric() && !is_cjk(n) {
                    push_lower(buf, n);
                } else {
                    break;
                }
                pos += n.len_utf8();
            }
            if !(stopwords && is_stopword(buf)) {
                f(buf);
            }
        }
    }
}

fn push_lower(buf: &mut String, c: char) {
    if c.is_ascii() {
        buf.push(c.to_ascii_lowercase());
    } else {
        buf.extend(c.to_lowercase());
    }
}

/// 每块的 BM25 分数（未加权、未过滤），和 `blocks` 一一对应。
pub fn bm25_scores<S: AsRef<str>>(blocks: &[S], query: &str, opts: &Bm25Options) -> Vec<f32> {
    let query_tokens = tokenize(query, opts.stopwords);
    // 查询词通常很少，线性查找比哈希快
    let mut terms: Vec<&str> = Vec::new();
    for t in &query_tokens {
        if !terms.contains(&t.as_str()) {
            terms.push(t);
        }
    }
    let q = terms.len();
    if q == 0 || blocks.is_empty() {
        return vec![0.0; blocks.len()];
    }
    // 只统计查询词的词频，其他 token 只计入长度，避免为每个 token 分配内存
    let mut tf = vec![0u32; blocks.len() * q];
    let mut lens = Vec::with_capacity(blocks.len());
    let mut buf = String::new();
    for (i, b) in blocks.iter().enumerate() {
        let row = &mut tf[i * q..(i + 1) * q];
        let mut len = 0u32;
        for_each_token(b.as_ref(), opts.stopwords, &mut buf, |t| {
            len += 1;
            if let Some(j) = terms.iter().position(|q| *q == t) {
                row[j] += 1;
            }
        });
        lens.push(len as f32);
    }
    let n = blocks.len() as f32;
    let avg = (lens.iter().sum::<f32>() / n).max(1.0);
    let idf: Vec<f32> = (0..q)
        .map(|j| {
            let df = (0..blocks.len()).filter(|&i| tf[i * q + j] > 0).count() as f32;
            (1.0 + (n - df + 0.5) / (df + 0.5)).ln()
        })
        .collect();
    let mut qf = vec![0f32; q];
    for t in &query_tokens {
        if let Some(j) = terms.iter().position(|q| q == t) {
            qf[j] += 1.0;
        }
    }
    (0..blocks.len())
        .map(|i| {
            let norm = opts.k1 * (1.0 - opts.b + opts.b * lens[i] / avg);
            (0..q)
                .map(|j| {
                    let f = tf[i * q + j] as f32;
                    qf[j] * idf[j] * f * (opts.k1 + 1.0) / (f + norm)
                })
                .sum()
        })
        .collect()
}

fn block_weight(block: &str) -> f32 {
    let t = block.trim_start();
    if t.starts_with("# ") {
        5.0
    } else if t.starts_with("## ") {
        4.0
    } else if t.starts_with("### ") {
        3.0
    } else if t.starts_with('>') {
        2.0
    } else if t.starts_with("```") || t.starts_with("~~~") {
        1.5
    } else {
        1.0
    }
}

/// 打分后按 `top_k` 和 `threshold` 选块，结果按原文顺序返回；内容相同的块只保留第一个。
pub fn bm25_filter<S: AsRef<str>>(
    blocks: &[S],
    query: &str,
    opts: &Bm25Options,
) -> Vec<ScoredBlock> {
    let raw = bm25_scores(blocks, query, opts);
    let mut seen = std::collections::HashSet::new();
    let mut scored: Vec<ScoredBlock> = raw
        .into_iter()
        .enumerate()
        .filter(|(i, _)| seen.insert(blocks[*i].as_ref().trim()))
        .map(|(index, s)| ScoredBlock {
            index,
            score: if opts.block_weights {
                s * block_weight(blocks[index].as_ref())
            } else {
                s
            },
        })
        .filter(|s| s.score > 0.0 && opts.threshold.is_none_or(|t| s.score >= t))
        .collect();
    if let Some(k) = opts.top_k
        && scored.len() > k
    {
        scored.sort_by(|a, b| b.score.total_cmp(&a.score).then(a.index.cmp(&b.index)));
        scored.truncate(k);
    }
    scored.sort_by_key(|s| s.index);
    scored
}

/// 按段落切开 Markdown，只保留和 query 相关的块，用空行重新拼起来（query 版 fit_markdown）。
pub fn filter_markdown(markdown: &str, query: &str, opts: &Bm25Options) -> String {
    let chunks = chunk(markdown, &ChunkStrategy::Paragraphs).unwrap_or_default();
    let blocks: Vec<&str> = chunks.iter().map(|c| c.text.as_str()).collect();
    let kept = bm25_filter(&blocks, query, opts);
    let mut out = String::new();
    for s in kept {
        if !out.is_empty() {
            out.push_str("\n\n");
        }
        out.push_str(blocks[s.index]);
    }
    if !out.is_empty() {
        out.push('\n');
    }
    out
}

fn is_stopword(w: &str) -> bool {
    STOPWORDS.binary_search(&w).is_ok()
}

// 保持字典序，用二分查找
const STOPWORDS: &[&str] = &[
    "a",
    "about",
    "above",
    "after",
    "again",
    "against",
    "all",
    "am",
    "an",
    "and",
    "any",
    "are",
    "as",
    "at",
    "be",
    "because",
    "been",
    "before",
    "being",
    "below",
    "between",
    "both",
    "but",
    "by",
    "can",
    "could",
    "did",
    "do",
    "does",
    "doing",
    "down",
    "during",
    "each",
    "few",
    "for",
    "from",
    "further",
    "had",
    "has",
    "have",
    "having",
    "he",
    "her",
    "here",
    "hers",
    "herself",
    "him",
    "himself",
    "his",
    "how",
    "i",
    "if",
    "in",
    "into",
    "is",
    "it",
    "its",
    "itself",
    "just",
    "me",
    "more",
    "most",
    "my",
    "myself",
    "no",
    "nor",
    "not",
    "now",
    "of",
    "off",
    "on",
    "once",
    "only",
    "or",
    "other",
    "our",
    "ours",
    "ourselves",
    "out",
    "over",
    "own",
    "s",
    "same",
    "she",
    "should",
    "so",
    "some",
    "such",
    "t",
    "than",
    "that",
    "the",
    "their",
    "theirs",
    "them",
    "themselves",
    "then",
    "there",
    "these",
    "they",
    "this",
    "those",
    "through",
    "to",
    "too",
    "under",
    "until",
    "up",
    "very",
    "was",
    "we",
    "were",
    "what",
    "when",
    "where",
    "which",
    "while",
    "who",
    "whom",
    "why",
    "will",
    "with",
    "would",
    "you",
    "your",
    "yours",
    "yourself",
    "yourselves",
];

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn stopwords_sorted() {
        assert!(STOPWORDS.windows(2).all(|w| w[0] < w[1]));
    }

    #[test]
    fn tokenizer() {
        assert_eq!(
            tokenize("The Rust-Crawler's API v2", true),
            ["rust", "crawler", "api", "v2"]
        );
        assert_eq!(tokenize("The end", false), ["the", "end"]);
        assert_eq!(
            tokenize("网页抓取 is 快", true),
            ["网页", "页抓", "抓取", "快"]
        );
        assert_eq!(tokenize("カタカナとひらがな", true).len(), 8);
        assert_eq!(tokenize("Ünïcode ÉCOLE", true), ["ünïcode", "école"]);
    }

    #[test]
    fn ranks_relevant_blocks() {
        let blocks = [
            "Pagesift is a fast web crawler written in Rust.",
            "The weather today is sunny with light wind.",
            "Install the crawler with cargo install pagesift.",
            "Pagesift is a fast web crawler written in Rust.",
            "网页抓取引擎支持中文分词。",
        ];
        let s = bm25_scores(&blocks, "rust crawler", &Bm25Options::default());
        assert!(s[0] > s[2] && s[2] > 0.0 && s[1] == 0.0);
        let opts = Bm25Options {
            threshold: None,
            top_k: Some(1),
            ..Default::default()
        };
        let top = bm25_filter(&blocks, "rust crawler", &opts);
        assert_eq!(top.len(), 1);
        assert_eq!(top[0].index, 0);
        let all = bm25_filter(
            &blocks,
            "rust crawler",
            &Bm25Options {
                threshold: Some(0.1),
                ..Default::default()
            },
        );
        assert_eq!(all.iter().map(|s| s.index).collect::<Vec<_>>(), [0, 2]);
        let zh = bm25_filter(
            &blocks,
            "中文抓取",
            &Bm25Options {
                threshold: None,
                ..Default::default()
            },
        );
        assert_eq!(zh.iter().map(|s| s.index).collect::<Vec<_>>(), [4]);
        assert!(bm25_filter(&blocks, "the", &Bm25Options::default()).is_empty());
    }

    #[test]
    fn headings_weigh_more_and_markdown_filter() {
        let md =
            "# Rust crawler\n\nSome intro about cooking.\n\nA paragraph on rust crawler speed.\n";
        let out = filter_markdown(
            md,
            "rust crawler",
            &Bm25Options {
                threshold: Some(0.01),
                ..Default::default()
            },
        );
        assert_eq!(
            out,
            "# Rust crawler\n\nA paragraph on rust crawler speed.\n"
        );
        let blocks = ["# Rust crawler", "Rust crawler"];
        let s = bm25_filter(
            &blocks,
            "rust",
            &Bm25Options {
                threshold: None,
                ..Default::default()
            },
        );
        assert!(s[0].score > s[1].score);
    }
}
