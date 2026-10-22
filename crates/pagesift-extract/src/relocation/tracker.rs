//! 自适应 DOM 元素重定位跟踪器：基于拓扑指纹在变异 DOM 中自愈找回。

use pagesift_html::{Html, NodeRef};
use super::fingerprint::ElementFingerprint;
use super::similarity::score_similarity;

#[derive(Debug, Clone, Copy)]
pub struct RelocationOptions {
    pub min_score: f64,
    pub min_margin: f64,
}

impl Default for RelocationOptions {
    fn default() -> Self {
        Self {
            min_score: 0.50,
            min_margin: 0.05,
        }
    }
}

#[derive(Debug)]
pub enum RelocateOutcome<'a> {
    Found {
        node: NodeRef<'a>,
        score: f64,
        margin: f64,
    },
    Ambiguous {
        best_score: f64,
        runner_up: f64,
    },
    NotFound,
}

/// 在变异的新 DOM 文档中自愈找回目标元素。
pub fn relocate<'a>(
    fingerprint: &ElementFingerprint,
    doc: &'a Html<'a>,
    opts: RelocationOptions,
) -> RelocateOutcome<'a> {
    let mut best: Option<(NodeRef<'a>, f64)> = None;
    let mut runner_up = 0.0_f64;

    for node in doc.root().descendants().filter(|n| n.is_element()) {
        let score = score_similarity(fingerprint, node);

        if best.is_none() || score > best.unwrap().1 {
            if let Some((prev_node, prev_score)) = best {
                if !is_family(prev_node, node) {
                    runner_up = runner_up.max(prev_score);
                }
            }
            best = Some((node, score));
        } else if let Some((best_node, _)) = best {
            if !is_family(best_node, node) {
                runner_up = runner_up.max(score);
            }
        }
    }

    let Some((best_node, best_score)) = best else {
        return RelocateOutcome::NotFound;
    };

    if best_score < opts.min_score {
        return RelocateOutcome::NotFound;
    }

    let margin = best_score - runner_up;
    if margin < opts.min_margin && runner_up > 0.0 {
        RelocateOutcome::Ambiguous {
            best_score,
            runner_up,
        }
    } else {
        RelocateOutcome::Found {
            node: best_node,
            score: best_score,
            margin,
        }
    }
}

/// 判定两个节点是否为直接祖先-后代包装关系（外层包装不是独立竞争候选者）
fn is_family(a: NodeRef<'_>, b: NodeRef<'_>) -> bool {
    a == b || a.ancestors().any(|anc| anc == b) || b.ancestors().any(|anc| anc == a)
}
