//! 纯 Rust 树状编辑拓扑距离与自适应加权相似度仲裁器。

use super::fingerprint::{ElementFingerprint, tokenize};
use pagesift_html::NodeRef;
use std::collections::BTreeSet;

/// 计算候选节点与目标指纹的多维加权相似度（0.0 ~ 1.0）。
pub fn score_similarity(target: &ElementFingerprint, candidate: NodeRef<'_>) -> f64 {
    let Some(el) = candidate.as_element() else {
        return 0.0;
    };

    let cand_text = candidate.text().trim().to_string();
    let cand_tag = el.name();

    // 1. 文本内容相似度 (权重 0.50)
    let text_score = if cand_text == target.text && !target.text.is_empty() {
        1.0
    } else if target.text.is_empty() {
        0.5
    } else {
        let cand_tokens = tokenize(&cand_text);
        dice_coefficient(&target.tokens, &cand_tokens)
    };

    // 2. 标签兼容度与叶子节点匹配 (权重 0.20)
    let is_cand_leaf = !candidate.children().any(|c| c.is_element());
    let tag_score =
        tag_compatibility(&target.tag, cand_tag) + if is_cand_leaf { 0.05 } else { 0.0 };

    // 3. 上下文词素相似度（兄弟与父节点内容）(权重 0.20)
    let mut cand_context = BTreeSet::new();
    if let Some(parent) = candidate.parent() {
        for sibling in parent.children() {
            if sibling != candidate {
                for t in tokenize(&sibling.text()) {
                    cand_context.insert(t);
                    if cand_context.len() >= 64 {
                        break;
                    }
                }
            }
        }
    }
    let context_score = if target.context_tokens.is_empty() {
        0.6
    } else {
        dice_coefficient(&target.context_tokens, &cand_context)
    };

    // 4. 语义类名重合度 (权重 0.10)
    let class_score = if target.semantic_classes.is_empty() {
        0.5
    } else {
        let mut cand_classes = BTreeSet::new();
        if let Some(cls_str) = el.attr("class") {
            for word in cls_str.split_whitespace() {
                cand_classes.insert(word.to_lowercase());
            }
        }
        dice_coefficient(&target.semantic_classes, &cand_classes)
    };

    // 5. 祖先路径对齐度 (权重 0.05)
    let cand_ancestors: Vec<String> = candidate
        .ancestors()
        .filter_map(|a| a.as_element().map(|e| e.name().to_string()))
        .collect();
    let ancestor_score = ancestor_path_lcs(&target.ancestor_tags, &cand_ancestors);

    // 加权综合评分
    let total_score = text_score * 0.50
        + tag_score * 0.15
        + context_score * 0.20
        + class_score * 0.10
        + ancestor_score * 0.05;

    total_score.clamp(0.0, 1.0)
}

/// 语义相近标签等价打分
fn tag_compatibility(t1: &str, t2: &str) -> f64 {
    if t1.eq_ignore_ascii_case(t2) {
        return 1.0;
    }
    // 文本行内容器等价
    let is_text_elem = |t: &str| {
        matches!(
            t,
            "span"
                | "p"
                | "div"
                | "b"
                | "strong"
                | "em"
                | "i"
                | "h1"
                | "h2"
                | "h3"
                | "h4"
                | "h5"
                | "h6"
        )
    };
    if is_text_elem(t1) && is_text_elem(t2) {
        return 0.75;
    }
    // 交互操作元素等价
    let is_interactive = |t: &str| matches!(t, "button" | "a" | "input");
    if is_interactive(t1) && is_interactive(t2) {
        return 0.80;
    }
    // 块级容器等价
    let is_container = |t: &str| {
        matches!(
            t,
            "div" | "article" | "section" | "main" | "header" | "footer" | "aside"
        )
    };
    if is_container(t1) && is_container(t2) {
        return 0.80;
    }
    0.20
}

/// Dice 系数计算
fn dice_coefficient(a: &BTreeSet<String>, b: &BTreeSet<String>) -> f64 {
    if a.is_empty() && b.is_empty() {
        return 1.0;
    }
    if a.is_empty() || b.is_empty() {
        return 0.0;
    }
    let intersection = a.intersection(b).count();
    (2.0 * intersection as f64) / (a.len() + b.len()) as f64
}

/// 祖先拓扑路径最长公共子序列（LCS）比例
fn ancestor_path_lcs(a: &[String], b: &[String]) -> f64 {
    if a.is_empty() || b.is_empty() {
        return 0.5;
    }
    let mut matches = 0;
    let mut j = 0;
    for tag_a in a {
        while j < b.len() {
            if tag_a.eq_ignore_ascii_case(&b[j]) {
                matches += 1;
                j += 1;
                break;
            }
            j += 1;
        }
    }
    (matches as f64) / (a.len().max(b.len()) as f64)
}
