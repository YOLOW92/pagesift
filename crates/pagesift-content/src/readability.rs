//! 按 Mozilla Readability（Apache-2.0，https://github.com/mozilla/readability）的打分思路找正文容器，
//! 在 pagesift 的 DOM 和预先算好的文字统计上重新实现：一次遍历给段落打分并分给祖先，
//! 不改动 DOM，不重复计算文字长度。
//!
//! 打分：每个够长的段落（`p`、`td`、`pre`、没有块级子元素的 `div`、`section`、`h2`–`h6`）得
//! 1 + 逗号数 + 每 100 字 1 分（最多 3 分），父元素全得、祖父得一半、更上层得 1/(3×层数)；
//! 容器按标签和 class/id 词加减分，最后乘以 (1 − 链接密度)。分数最高的容器再按 Readability 的规则
//! 往上找共同祖先，并带上分数够高或者像正文段落的兄弟元素。

use std::collections::HashMap;

use pagesift_html::{Html, NodeId, Tag};

use crate::dom::NodeR;
use crate::extract::Skipper;

const MIN_PARAGRAPH_CHARS: usize = 25;
const MIN_CANDIDATE_CHARS: usize = 100;
const TOP_CANDIDATES: usize = 5;
const MIN_ALTERNATIVES: usize = 3;

/// 正文：分数最高的容器和要一起输出的兄弟元素，按文档顺序。找不到时返回 `None`。
pub(crate) fn content_nodes<'a>(dom: &'a Html<'a>, skipper: &Skipper) -> Option<Vec<NodeR<'a>>> {
    let body = dom.body();
    let mut scores: HashMap<NodeId, f64> = HashMap::new();
    let init = |n: &NodeR<'_>| -> f64 {
        let base = match n.tag() {
            Some(Tag::Div) => 5.0,
            Some(Tag::Pre | Tag::Td | Tag::Blockquote) => 3.0,
            Some(
                Tag::Address
                | Tag::Ol
                | Tag::Ul
                | Tag::Dl
                | Tag::Dd
                | Tag::Dt
                | Tag::Li
                | Tag::Form,
            ) => -3.0,
            Some(Tag::H1 | Tag::H2 | Tag::H3 | Tag::H4 | Tag::H5 | Tag::H6 | Tag::Th) => -5.0,
            _ => 0.0,
        };
        base + class_weight(skipper.features(n))
    };

    for node in body.descendants() {
        let Some(tag) = node.tag() else { continue };
        let scored = match tag {
            Tag::P
            | Tag::Td
            | Tag::Pre
            | Tag::Section
            | Tag::H2
            | Tag::H3
            | Tag::H4
            | Tag::H5
            | Tag::H6 => true,
            Tag::Div => !skipper.has_block_descendant(&node),
            _ => false,
        };
        if !scored || skipped(&node, skipper) {
            continue;
        }
        // Readability 先删掉"不太可能是正文"的块；这里不删，只是不让它们里面的段落计分
        if node
            .ancestors()
            .take(6)
            .any(|a| unlikely(skipper.features(&a), &a))
        {
            continue;
        }
        let chars = skipper.visible_chars(&node);
        if chars < MIN_PARAGRAPH_CHARS {
            continue;
        }
        let commas = visible_commas(&node, skipper);
        let score = 1.0 + (commas + 1) as f64 + (chars / 100).min(3) as f64;
        for (level, a) in node
            .ancestors()
            .filter(|a| a.is_element())
            .take(5)
            .enumerate()
        {
            if a.tag() == Some(Tag::Html) {
                break;
            }
            let divider = match level {
                0 => 1.0,
                1 => 2.0,
                l => l as f64 * 3.0,
            };
            *scores.entry(a.id()).or_insert_with(|| init(&a)) += score / divider;
        }
    }
    if scores.is_empty() {
        return None;
    }
    let final_score = |id: NodeId| {
        let n = dom.get(id);
        let (chars, link_chars, _) = skipper.stats(&n);
        let density = if chars == 0 {
            0.0
        } else {
            link_chars as f64 / chars as f64
        };
        scores[&id] * (1.0 - density)
    };
    // 候选得有点正文：Readability 按类名给容器加的 25 分，会让几乎没字的 `main_video`
    // 这类块压过真正的正文
    let mut top: Vec<(NodeId, f64)> = scores
        .keys()
        .filter(|&&id| skipper.content_chars(&dom.get(id)) >= MIN_CANDIDATE_CHARS)
        .map(|&id| (id, final_score(id)))
        .collect();
    top.sort_by(|a, b| b.1.total_cmp(&a.1));
    top.truncate(TOP_CANDIDATES);
    let (mut best, best_score) = *top.first()?;
    if dom.get(best).tag() == Some(Tag::Body) {
        return None;
    }

    // 好几个高分候选有共同祖先时，正文是那个祖先（文章被拆成几段放在兄弟容器里）
    let alternatives: Vec<Vec<NodeId>> = top[1..]
        .iter()
        .filter(|(_, s)| *s / best_score >= 0.75)
        .map(|(id, _)| dom.get(*id).ancestors().map(|a| a.id()).collect())
        .collect();
    if alternatives.len() >= MIN_ALTERNATIVES {
        for p in dom.get(best).ancestors() {
            if p.tag() == Some(Tag::Body) {
                break;
            }
            if alternatives.iter().filter(|a| a.contains(&p.id())).count() >= MIN_ALTERNATIVES {
                best = p.id();
                break;
            }
        }
    }
    // 父元素分数更高就往上走；低于三分之一就停
    let mut last = scores.get(&best).map_or(best_score, |_| final_score(best));
    let threshold = last / 3.0;
    let mut cur = dom.get(best);
    while let Some(p) = cur
        .parent()
        .filter(|p| p.tag() != Some(Tag::Body) && p.is_element())
    {
        if scores.contains_key(&p.id()) {
            let s = final_score(p.id());
            if s < threshold {
                break;
            }
            if s > last {
                best = p.id();
                break;
            }
            last = s;
        }
        cur = p;
    }
    // 只有一个子元素的父元素等于它本身
    let mut cur = dom.get(best);
    while let Some(p) = cur
        .parent()
        .filter(|p| p.tag() != Some(Tag::Body) && p.is_element())
    {
        if p.children().filter(|c| c.is_element()).count() != 1 {
            break;
        }
        cur = p;
    }
    let top_node = cur;
    let top_score = scores
        .get(&top_node.id())
        .map_or(best_score, |_| final_score(top_node.id()));

    // 兄弟元素：分数够高，或者是像正文的段落
    let Some(parent) = top_node.parent() else {
        return Some(vec![top_node]);
    };
    let sibling_threshold = (top_score * 0.2).max(10.0);
    let top_class = top_node
        .as_element()
        .and_then(|e| e.attr("class"))
        .unwrap_or("");
    let mut out = Vec::new();
    for s in parent.children().filter(|c| c.is_element()) {
        if s.id() == top_node.id() {
            out.push(s);
            continue;
        }
        if skipped(&s, skipper) {
            continue;
        }
        let bonus = match s.as_element().and_then(|e| e.attr("class")) {
            Some(c) if !top_class.is_empty() && c == top_class => top_score * 0.2,
            _ => 0.0,
        };
        let score = scores.get(&s.id()).map(|_| final_score(s.id()));
        let append = if score.is_some_and(|v| v + bonus >= sibling_threshold) {
            true
        } else if s.tag() == Some(Tag::P) {
            let (chars, link_chars, _) = skipper.stats(&s);
            let density = if chars == 0 {
                0.0
            } else {
                link_chars as f64 / chars as f64
            };
            (chars > 80 && density < 0.25)
                || (chars > 0 && chars <= 80 && link_chars == 0 && ends_sentence(&s))
        } else {
            false
        };
        if append {
            out.push(s);
        }
    }
    Some(out)
}

/// class 和 id 各自命中正面词加 25 分、负面词减 25 分。
fn class_weight(f: u32) -> f64 {
    use crate::features::*;
    let mut w = 0.0;
    for (pos, neg) in [(POS_CLASS, NEG_CLASS), (POS_ID, NEG_ID)] {
        if f & neg != 0 {
            w -= 25.0;
        }
        if f & pos != 0 {
            w += 25.0;
        }
    }
    w
}

fn unlikely(f: u32, n: &NodeR<'_>) -> bool {
    !matches!(
        n.tag(),
        Some(Tag::Body | Tag::Html | Tag::A | Tag::Article | Tag::Main)
    ) && f & crate::features::UNLIKELY != 0
        // 手风琴的标题按钮常叫 `accordion__header`：不是页眉
        && !crate::extract::is_disclosure_header(n)
}

fn skipped(n: &NodeR<'_>, skipper: &Skipper) -> bool {
    skipper.skip(n) || n.ancestors().any(|a| a.is_element() && skipper.skip(&a))
}

/// 可见文字里的逗号（脚本、样式、隐藏元素里的不算：播放器配置里全是逗号）。
fn visible_commas(n: &NodeR<'_>, skipper: &Skipper) -> usize {
    let mut total = 0;
    for c in n.children() {
        if let Some(t) = c.as_text() {
            total += t
                .chars()
                .filter(|c| matches!(c, ',' | '，' | '、' | '،' | '﹐'))
                .count();
        } else if c.is_element() && !skipper.skip(&c) {
            total += visible_commas(&c, skipper);
        }
    }
    total
}

fn ends_sentence(n: &NodeR<'_>) -> bool {
    let text: String = n.descendants().filter_map(|d| d.as_text()).collect();
    text.contains(". ") || text.trim_end().ends_with('.')
}
