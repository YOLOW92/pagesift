//! 查询相关性：爬虫给链接打分、自适应爬取算覆盖度用的查询词匹配，
//! 以及按查询过滤正文（BM25 选段落，保留它们所在的标题层级）。

use std::collections::HashSet;

use crate::bm25::{Bm25Options, bm25_filter, for_each_token, tokenize};
use crate::chunk::{ChunkStrategy, atx_heading, chunk};

/// 最多跟踪多少个查询词：命中情况用一个 `u64` 位图记录。
pub const MAX_QUERY_TERMS: usize = 64;

/// 分好词、做过简单词干化的查询。匹配时英文按词干前缀比较（`pricing` 能匹配 `price`），
/// 中日韩文字按字 bigram 精确比较。
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Query {
    terms: Vec<String>,
}

impl Query {
    pub fn new(query: &str) -> Self {
        let mut terms: Vec<String> = Vec::new();
        let mut buf = String::new();
        for stopwords in [true, false] {
            for_each_token(query, stopwords, &mut buf, |t| {
                let s = stem(t);
                if terms.len() < MAX_QUERY_TERMS && !terms.iter().any(|x| x == s) {
                    terms.push(s.to_owned());
                }
            });
            // 查询全是停用词（如 "how to"）时，不去停用词再分一次
            if !terms.is_empty() {
                break;
            }
        }
        Self { terms }
    }

    pub fn terms(&self) -> &[String] {
        &self.terms
    }

    pub fn len(&self) -> usize {
        self.terms.len()
    }

    pub fn is_empty(&self) -> bool {
        self.terms.is_empty()
    }

    fn full_mask(&self) -> u64 {
        match self.terms.len() {
            64 => u64::MAX,
            n => (1u64 << n) - 1,
        }
    }

    /// 文本里出现了哪些查询词：第 i 位对应第 i 个词。全部命中后不再往下扫。
    pub fn hit_mask(&self, text: &str) -> u64 {
        let full = self.full_mask();
        let mut mask = 0u64;
        if full == 0 {
            return 0;
        }
        let mut buf = String::new();
        for_each_token(text, false, &mut buf, |t| {
            if mask == full {
                return;
            }
            let s = stem(t);
            for (i, term) in self.terms.iter().enumerate() {
                if mask & (1 << i) == 0 && term_matches(s, term) {
                    mask |= 1 << i;
                }
            }
        });
        mask
    }

    /// 文本覆盖了多大比例的查询词，0 到 1；查询为空时是 0。
    pub fn coverage(&self, text: &str) -> f32 {
        if self.terms.is_empty() {
            return 0.0;
        }
        self.hit_mask(text).count_ones() as f32 / self.terms.len() as f32
    }
}

/// 很粗的英文词干化：去掉常见词尾，只切片不分配。非 ASCII 的词原样返回。
fn stem(t: &str) -> &str {
    const SUFFIXES: &[&str] = &[
        "ations", "ation", "ments", "ment", "ings", "ing", "ions", "ion", "ers", "er", "es", "ed",
        "ly", "s",
    ];
    if !t.is_ascii() {
        return t;
    }
    for s in SUFFIXES {
        if let Some(rest) = t.strip_suffix(s)
            && rest.len() >= 3
        {
            return rest;
        }
    }
    t
}

/// 词干相同，或者都是不短于 4 个字母的 ASCII 词、共同前缀至少 4 个字母且最多差一个字母
/// （`polici`/`policy`、`pric`/`price`）。短词只认完全相同，避免 `car` 匹配 `cart`。
fn term_matches(a: &str, b: &str) -> bool {
    if a == b {
        return true;
    }
    let m = a.len().min(b.len());
    if m < 4 || !a.is_ascii() || !b.is_ascii() {
        return false;
    }
    let common = a.bytes().zip(b.bytes()).take_while(|(x, y)| x == y).count();
    common >= 4 && common + 1 >= m
}

/// [`filter_markdown_by_query`] 的参数。
#[derive(Debug, Clone, PartialEq)]
pub struct QueryFilterOptions {
    /// BM25 分数不低于它的段落才保留（同 Crawl4AI 的 `bm25_threshold`）。
    pub threshold: f32,
    /// 最多保留分数最高的几段。
    pub top_k: Option<usize>,
    /// 在保留的段落前补上它所在的各级标题。
    pub headings: bool,
}

impl Default for QueryFilterOptions {
    fn default() -> Self {
        Self {
            threshold: 1.0,
            top_k: None,
            headings: true,
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct FilteredMarkdown {
    pub markdown: String,
    /// 保留的正文段落数（不含补上的标题）。
    pub kept: usize,
    /// 正文段落总数（不含标题）。
    pub total: usize,
}

fn heading_level(block: &str) -> Option<u8> {
    if block.contains('\n') {
        return None;
    }
    atx_heading(block).map(|(level, _)| level)
}

/// 只保留和查询相关的段落。每段打分时带上它的标题路径，所以标题命中的小节整节都会得分；
/// 输出按原文顺序，每段前补上还没输出过的各级标题，读者仍然知道这段属于哪一节。
/// 查询为空（或只有停用词）时原样返回。
pub fn filter_markdown_by_query(
    markdown: &str,
    query: &str,
    opts: &QueryFilterOptions,
) -> FilteredMarkdown {
    let chunks = chunk(markdown, &ChunkStrategy::Paragraphs).unwrap_or_default();
    let levels: Vec<Option<u8>> = chunks.iter().map(|c| heading_level(&c.text)).collect();
    let body: Vec<usize> = (0..chunks.len()).filter(|&i| levels[i].is_none()).collect();
    // 和 BM25 打分用同样的分词：查询只剩停用词时没法打分，原样返回
    if tokenize(query, true).is_empty() {
        return FilteredMarkdown {
            markdown: markdown.to_owned(),
            kept: body.len(),
            total: body.len(),
        };
    }
    let docs: Vec<String> = body
        .iter()
        .map(|&i| {
            let c = &chunks[i];
            if c.heading_path.is_empty() {
                c.text.clone()
            } else {
                format!("{}\n{}", c.heading_path.join(" "), c.text)
            }
        })
        .collect();
    let bm25 = Bm25Options {
        top_k: opts.top_k,
        threshold: Some(opts.threshold),
        block_weights: false,
        ..Default::default()
    };
    let keep: HashSet<usize> = bm25_filter(&docs, query, &bm25)
        .into_iter()
        .map(|s| body[s.index])
        .collect();

    let mut out = String::new();
    let mut push = |s: &str| {
        if !out.is_empty() {
            out.push_str("\n\n");
        }
        out.push_str(s);
    };
    // 当前位置的标题栈 (级别, 块下标)；栈里的下标递增，下标不超过 `emitted` 的标题都已经输出过
    let mut stack: Vec<(u8, usize)> = Vec::new();
    let mut emitted: Option<usize> = None;
    for (i, c) in chunks.iter().enumerate() {
        if let Some(level) = levels[i] {
            while stack.last().is_some_and(|&(l, _)| l >= level) {
                stack.pop();
            }
            stack.push((level, i));
            continue;
        }
        if !keep.contains(&i) {
            continue;
        }
        if opts.headings {
            for &(_, h) in &stack {
                if emitted.is_none_or(|e| h > e) {
                    push(&chunks[h].text);
                    emitted = Some(h);
                }
            }
        }
        push(&c.text);
    }
    if !out.is_empty() {
        out.push('\n');
    }
    FilteredMarkdown {
        markdown: out,
        kept: keep.len(),
        total: body.len(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn query_terms_are_stemmed_and_deduplicated() {
        let q = Query::new("How to install the installer? Installation!");
        assert_eq!(q.terms(), ["install"]);
        assert_eq!(Query::new("how to").terms(), ["how", "to"]);
        assert!(Query::new("  ,. ").is_empty());
        let many: String = (0..100).map(|i| format!("word{i} ")).collect();
        assert_eq!(Query::new(&many).len(), MAX_QUERY_TERMS);
    }

    #[test]
    fn hit_mask_matches_word_forms() {
        let q = Query::new("pricing policy refund");
        assert_eq!(q.hit_mask("See our prices and refunds"), 0b101);
        assert_eq!(q.hit_mask("Privacy policies"), 0b010);
        assert_eq!(q.hit_mask("nothing here"), 0);
        assert!((q.coverage("price policy refund") - 1.0).abs() < 1e-6);
        // 短词只认完全相同
        let car = Query::new("car");
        assert_eq!(car.hit_mask("a cart"), 0);
        assert_eq!(car.hit_mask("a car"), 1);
        assert_eq!(Query::default().coverage("anything"), 0.0);
    }

    #[test]
    fn hit_mask_handles_cjk() {
        let q = Query::new("退款政策");
        assert_eq!(q.terms(), ["退款", "款政", "政策"]);
        assert_eq!(q.coverage("我们的退款流程"), 1.0 / 3.0);
    }

    const DOC: &str = "# Guide\n\nIntro text about the product.\n\n## Install\n\nRun the installer on Linux.\n\n### Linux\n\nUse apt to get the package.\n\n## Pricing\n\nThe pro plan costs ten dollars a month.\n\nPayments are monthly.\n\n## Support\n\nEmail us any time.\n";

    #[test]
    fn filter_keeps_relevant_paragraphs_with_heading_context() {
        let f = filter_markdown_by_query(DOC, "pricing plan", &QueryFilterOptions::default());
        assert_eq!(
            f.markdown,
            "# Guide\n\n## Pricing\n\nThe pro plan costs ten dollars a month.\n\nPayments are monthly.\n"
        );
        assert_eq!((f.kept, f.total), (2, 6));
    }

    #[test]
    fn filter_emits_each_heading_once_and_restarts_after_siblings() {
        let opts = QueryFilterOptions {
            threshold: 0.1,
            ..Default::default()
        };
        let f = filter_markdown_by_query(DOC, "linux monthly", &opts);
        assert_eq!(
            f.markdown,
            "# Guide\n\n## Install\n\nRun the installer on Linux.\n\n### Linux\n\nUse apt to get the package.\n\n## Pricing\n\nPayments are monthly.\n"
        );
        let bare = filter_markdown_by_query(
            DOC,
            "monthly",
            &QueryFilterOptions {
                headings: false,
                threshold: 0.1,
                ..Default::default()
            },
        );
        assert_eq!(bare.markdown, "Payments are monthly.\n");
    }

    #[test]
    fn filter_top_k_and_edge_cases() {
        let opts = QueryFilterOptions {
            threshold: 0.0,
            top_k: Some(1),
            ..Default::default()
        };
        let f = filter_markdown_by_query(DOC, "apt package", &opts);
        assert_eq!(f.kept, 1);
        assert!(f.markdown.ends_with("Use apt to get the package.\n"));
        let none = filter_markdown_by_query(DOC, "kubernetes", &QueryFilterOptions::default());
        assert_eq!((none.markdown.as_str(), none.kept), ("", 0));
        let all = filter_markdown_by_query(DOC, "the", &QueryFilterOptions::default());
        assert_eq!(
            all.markdown, DOC,
            "query of only stopwords keeps everything"
        );
        assert_eq!(filter_markdown_by_query("", "x", &opts).markdown, "");
    }
}
