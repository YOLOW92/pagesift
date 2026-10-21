//! 查询相关性：爬虫给链接打分、自适应爬取算覆盖度用的查询词匹配，
//! 以及按查询过滤正文（BM25 选段落，保留它们所在的标题层级）。

use std::collections::HashSet;

use crate::bm25::{Bm25Options, bm25_scores, for_each_token, tokenize};
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

/// 打分单位的大小（token）：小节内部再切到这么大。评测里比按段落选（段落可能长达上万字）召回高一倍。
const UNIT_TOKENS: usize = 100;

/// [`filter_markdown_by_query`] 的参数。
#[derive(Debug, Clone, PartialEq)]
pub struct QueryFilterOptions {
    /// 绝对阈值：BM25 分数不低于它的单位才保留（Crawl4AI 的 `bm25_threshold`）。
    pub threshold: Option<f32>,
    /// 相对阈值：分数不低于最高分的这个比例才保留。默认 0.5。
    pub relative: Option<f32>,
    /// 最多保留分数最高的几个单位。
    pub top_k: Option<usize>,
    /// 保留内容的 token 上限（按分数从高到低取，不含补上的标题）。
    pub max_tokens: Option<usize>,
    /// 在保留的内容前补上它所在的各级标题。
    pub headings: bool,
}

/// 默认的相对阈值：在 Chroma 分块评测集上，保留集召回 0.87、平均只留 700 词
/// （Crawl4AI 默认的绝对阈值 1.0 召回 0.69、留 1.3 万词）。
pub const DEFAULT_RELATIVE: f32 = 0.5;

impl Default for QueryFilterOptions {
    fn default() -> Self {
        Self {
            threshold: None,
            relative: Some(DEFAULT_RELATIVE),
            top_k: None,
            max_tokens: None,
            headings: true,
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct FilteredMarkdown {
    pub markdown: String,
    /// 保留的单位数。
    pub kept: usize,
    /// 单位总数。
    pub total: usize,
    /// 保留的内容在输入里的字节区间（相邻单位已合并，不含补上的标题），可用来标出处。
    pub ranges: Vec<std::ops::Range<usize>>,
}

struct Unit {
    range: std::ops::Range<usize>,
    path: Vec<String>,
    tokens: usize,
}

/// 打分单位：先按标题切成小节，小节内部按大小递归切到 [`UNIT_TOKENS`] 以内（不跨小节）。
fn units(markdown: &str) -> Vec<Unit> {
    let mut out = Vec::new();
    for sec in chunk(markdown, &ChunkStrategy::Headings { max_level: 6 }).unwrap_or_default() {
        let text = &markdown[sec.start..sec.end];
        for c in chunk(text, &ChunkStrategy::Recursive { size: UNIT_TOKENS }).unwrap_or_default() {
            out.push(Unit {
                range: sec.start + c.start..sec.start + c.end,
                path: sec.heading_path.clone(),
                tokens: c.tokens,
            });
        }
    }
    out
}

/// 只保留和查询相关的内容（自研）。文档先切成小节内约 100 token 的单位，每个单位带着标题路径用 BM25 打分，
/// 默认保留分数不低于最高分一半的单位；也可以加绝对阈值、`top_k`、token 上限。
/// 输出按原文顺序，相邻的保留单位合并成原文的一整段，每段前补上还没输出过的各级标题。
/// 查询为空（或只有停用词）时原样返回。
pub fn filter_markdown_by_query(
    markdown: &str,
    query: &str,
    opts: &QueryFilterOptions,
) -> FilteredMarkdown {
    let units = units(markdown);
    // 和 BM25 打分用同样的分词：查询只剩停用词时没法打分，原样返回
    if tokenize(query, true).is_empty() {
        return FilteredMarkdown {
            markdown: markdown.to_owned(),
            kept: units.len(),
            total: units.len(),
            ranges: std::iter::once(0..markdown.len()).collect(),
        };
    }
    let docs: Vec<String> = units
        .iter()
        .map(|u| {
            let text = &markdown[u.range.clone()];
            if u.path.is_empty() {
                text.to_owned()
            } else {
                format!("{}\n{}", u.path.join(" "), text)
            }
        })
        .collect();
    let bm25 = Bm25Options {
        top_k: None,
        threshold: None,
        block_weights: false,
        ..Default::default()
    };
    let scores = bm25_scores(&docs, query, &bm25);
    let top = scores.iter().copied().fold(0.0f32, f32::max);
    let mut order: Vec<usize> = (0..units.len())
        .filter(|&i| {
            let s = scores[i];
            s > 0.0
                && opts.threshold.is_none_or(|t| s >= t)
                && opts.relative.is_none_or(|r| s >= r * top)
        })
        .collect();
    order.sort_by(|&a, &b| scores[b].total_cmp(&scores[a]).then(a.cmp(&b)));
    // 内容相同的单位只留一个
    let mut seen = HashSet::new();
    order.retain(|&i| seen.insert(markdown[units[i].range.clone()].trim()));
    if let Some(k) = opts.top_k {
        order.truncate(k);
    }
    if let Some(budget) = opts.max_tokens {
        let mut used = 0;
        order.retain(|&i| {
            if used + units[i].tokens > budget && used > 0 {
                return false;
            }
            used += units[i].tokens;
            true
        });
    }
    let mut keep = order.clone();
    keep.sort_unstable();

    // 标题：(起点, 级别, 原文这一行)
    let headings: Vec<(usize, u8, &str)> = chunk(markdown, &ChunkStrategy::Paragraphs)
        .unwrap_or_default()
        .into_iter()
        .filter(|c| !c.text.contains('\n'))
        .filter_map(|c| {
            atx_heading(&c.text).map(|(level, _)| (c.start, level, &markdown[c.start..c.end]))
        })
        .collect();
    let mut out = String::new();
    let mut push = |s: &str| {
        if !out.is_empty() {
            out.push_str("\n\n");
        }
        out.push_str(s.trim_end());
    };
    // 相邻的保留单位合并成一段原文
    let mut spans: Vec<std::ops::Range<usize>> = Vec::new();
    for &i in &keep {
        let r = units[i].range.clone();
        match spans.last_mut() {
            Some(last)
                if i > 0
                    && keep.binary_search(&(i - 1)).is_ok()
                    && units[i - 1].range.end <= r.start =>
            {
                last.end = r.end
            }
            _ => spans.push(r),
        }
    }
    let mut stack: Vec<(u8, usize)> = Vec::new();
    let mut next_heading = 0;
    let mut emitted: Option<usize> = None;
    for span in &spans {
        // 起点就在这一段开头的标题也算进标题栈（它会弹出已经结束的兄弟小节），但它在正文里，不单独输出
        while next_heading < headings.len() && headings[next_heading].0 <= span.start {
            let level = headings[next_heading].1;
            while stack.last().is_some_and(|&(l, _)| l >= level) {
                stack.pop();
            }
            stack.push((level, next_heading));
            next_heading += 1;
        }
        if opts.headings {
            for &(_, h) in &stack {
                if emitted.is_none_or(|e| h > e) && headings[h].0 < span.start {
                    push(headings[h].2);
                    emitted = Some(h);
                }
            }
        }
        push(&markdown[span.clone()]);
        if let Some(&(_, h)) = stack.last()
            && headings[h].0 == span.start
        {
            emitted = Some(emitted.map_or(h, |e| e.max(h)));
        }
        // 段落里自带的标题也算输出过
        while next_heading < headings.len() && headings[next_heading].0 < span.end {
            let level = headings[next_heading].1;
            while stack.last().is_some_and(|&(l, _)| l >= level) {
                stack.pop();
            }
            stack.push((level, next_heading));
            emitted = Some(next_heading);
            next_heading += 1;
        }
    }
    if !out.is_empty() {
        out.push('\n');
    }
    FilteredMarkdown {
        markdown: out,
        kept: keep.len(),
        total: units.len(),
        ranges: spans,
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
        assert_eq!((f.kept, f.total), (1, 5));
    }

    #[test]
    fn filter_emits_each_heading_once_and_restarts_after_siblings() {
        let opts = QueryFilterOptions {
            threshold: Some(0.1),
            relative: None,
            ..Default::default()
        };
        let f = filter_markdown_by_query(DOC, "linux monthly", &opts);
        assert_eq!(
            f.markdown,
            "# Guide\n\n## Install\n\nRun the installer on Linux.\n\n### Linux\n\nUse apt to get the package.\n\n## Pricing\n\nThe pro plan costs ten dollars a month.\n\nPayments are monthly.\n"
        );
        let bare = filter_markdown_by_query(
            DOC,
            "monthly",
            &QueryFilterOptions {
                headings: false,
                ..Default::default()
            },
        );
        assert_eq!(
            bare.markdown,
            "## Pricing\n\nThe pro plan costs ten dollars a month.\n\nPayments are monthly.\n"
        );
    }

    #[test]
    fn long_paragraphs_are_split_into_units_and_budget_applies() {
        let filler =
            "lorem ipsum dolor sit amet consectetur adipiscing elit sed do eiusmod tempor "
                .repeat(30);
        let doc = format!("{filler}The refund policy allows returns within thirty days. {filler}");
        let f = filter_markdown_by_query(&doc, "refund policy", &QueryFilterOptions::default());
        assert!(
            f.markdown.contains("The refund policy allows returns"),
            "{}",
            f.markdown
        );
        assert!(
            f.markdown.split_whitespace().count() <= 2 * UNIT_TOKENS,
            "only the unit around the answer"
        );
        assert!(f.total > 3);
        let tight = filter_markdown_by_query(
            &doc,
            "lorem refund",
            &QueryFilterOptions {
                relative: None,
                max_tokens: Some(150),
                ..Default::default()
            },
        );
        assert!(
            tight.markdown.split_whitespace().count() <= 150,
            "{}",
            tight.markdown.split_whitespace().count()
        );
    }

    #[test]
    fn filter_top_k_and_edge_cases() {
        let opts = QueryFilterOptions {
            relative: None,
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
