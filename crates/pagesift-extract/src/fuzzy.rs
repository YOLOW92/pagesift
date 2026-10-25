//! Optional appearance matching with explicit confidence and abstention. This
//! never changes `Anchor`'s stable-identity contract and cannot prove identity.
use std::collections::BTreeSet;

use pagesift_html::{Html, NodeRef, Selector};
use serde::{Deserialize, Serialize};

const MAX_NODES: usize = 20_000;
const MAX_TOKENS: usize = 128;

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct FuzzyAnchor {
    version: u32,
    tag: String,
    text: BTreeSet<String>,
    attributes: BTreeSet<String>,
    context: BTreeSet<String>,
}

#[derive(Clone, Copy, Debug)]
pub struct FuzzyOptions {
    pub minimum_score: f64,
    pub minimum_margin: f64,
}

impl Default for FuzzyOptions {
    fn default() -> Self {
        Self {
            minimum_score: 0.72,
            minimum_margin: 0.12,
        }
    }
}

#[derive(Debug)]
pub enum FuzzyMatch<'a> {
    Candidate {
        node: NodeRef<'a>,
        score: f64,
        runner_up: f64,
    },
    LowConfidence {
        best_score: f64,
    },
    Ambiguous {
        best_score: f64,
        runner_up: f64,
    },
    BudgetExceeded,
}

impl FuzzyAnchor {
    pub fn capture(doc: &Html<'_>, selector: &str) -> Result<Self, String> {
        let sel = Selector::parse(selector).map_err(|e| e.to_string())?;
        let mut nodes = doc.select(&sel);
        let node = nodes.next().ok_or("selector must match one element")?;
        if nodes.next().is_some() || !node.is_element() {
            return Err("selector must match one element".into());
        }
        let (tag, text, attributes, context) = features(node);
        if text.is_empty() && attributes.is_empty() {
            return Err("anchor needs text or descriptive attributes".into());
        }
        Ok(Self {
            version: 1,
            tag,
            text,
            attributes,
            context,
        })
    }

    /// Scores the whole bounded document; an old CSS match receives no special
    /// privilege. A tied/weak/over-budget result never returns a guessed node.
    pub fn locate<'a>(
        &self,
        doc: &'a Html<'a>,
        opts: FuzzyOptions,
    ) -> Result<FuzzyMatch<'a>, String> {
        if self.version != 1
            || self.tag.len() > 128
            || [&self.text, &self.attributes, &self.context]
                .iter()
                .any(|s| s.len() > MAX_TOKENS || s.iter().any(|t| t.len() > 256))
            || (self.text.is_empty() && self.attributes.is_empty())
        {
            return Err("invalid fuzzy anchor snapshot".into());
        }
        if !opts.minimum_score.is_finite()
            || !opts.minimum_margin.is_finite()
            || !(0.0..=1.0).contains(&opts.minimum_score)
            || !(0.0..=1.0).contains(&opts.minimum_margin)
        {
            return Err("fuzzy thresholds must be finite and within 0..=1".into());
        }
        let (mut best, mut runner_up) = (None, 0.0_f64);
        for (index, node) in doc.root().descendants().enumerate() {
            if index >= MAX_NODES {
                return Ok(FuzzyMatch::BudgetExceeded);
            }
            let Some(el) = node.as_element() else {
                continue;
            };
            let changed_tag = el.name() != self.tag;
            // 标签改变只在"同类角色"之间允许：
            //   - 容器之间（article/section/div/span/p 互换）：版面重构常见；
            //   - 行内与行内之间、块级与块级之间。
            // 跨角色（如 span→a、div→a）仍一律拒绝，避免只靠包含相同文字就误配到链接或包装器。
            if changed_tag && !same_tag_role(el.name(), &self.tag) {
                continue;
            }
            let (_, text, attributes, context) = features(node);
            // Corroborate container tag changes instead of matching arbitrary
            // wrappers or links just because they contain the same text.
            if changed_tag
                && (self.attributes.is_empty()
                    || dice(&self.attributes, &attributes) < 0.75
                    || dice(&self.text, &text) < 0.85)
            {
                continue;
            }
            let mut weight = 0.0;
            let mut score = 0.0;
            for (before, after, w) in [
                (&self.text, &text, 0.60),
                (&self.attributes, &attributes, 0.25),
                (&self.context, &context, 0.15),
            ] {
                if !before.is_empty() {
                    weight += w;
                    score += w * dice(before, after);
                }
            }
            let score = score / weight - if changed_tag { 0.04 } else { 0.0 };
            if best.is_none_or(|(_, s)| score > s) {
                runner_up = best.map_or(runner_up, |(_, s)| s);
                best = Some((node, score));
            } else {
                runner_up = runner_up.max(score);
            }
        }
        let Some((node, score)) = best else {
            return Ok(FuzzyMatch::LowConfidence { best_score: 0.0 });
        };
        if score < opts.minimum_score {
            Ok(FuzzyMatch::LowConfidence { best_score: score })
        } else if score - runner_up < opts.minimum_margin || score == runner_up {
            Ok(FuzzyMatch::Ambiguous {
                best_score: score,
                runner_up,
            })
        } else {
            Ok(FuzzyMatch::Candidate {
                node,
                score,
                runner_up,
            })
        }
    }
}

fn container_tag(tag: &str) -> bool {
    matches!(tag, "article" | "section" | "div")
}

/// 标签的角色分组。重构时标签常被换成同角色的另一个标签（`span`→`p`、`div`→`article`），
/// 但很少从行内换成链接、或从块级换成行内。
fn tag_role(tag: &str) -> u8 {
    match tag {
        // 容器：版面重构的主要对象
        "article" | "section" | "div" | "main" | "aside" | "header" | "footer" | "nav" => 1,
        // 文本块：段落与行内文本
        "p" | "span" | "blockquote" | "figcaption" | "label" | "strong" | "em" | "b" | "i" => 2,
        // 列表
        "ul" | "ol" | "li" | "dl" | "dt" | "dd" => 3,
        // 表格
        "table" | "thead" | "tbody" | "tr" | "td" | "th" => 4,
        // 标题
        "h1" | "h2" | "h3" | "h4" | "h5" | "h6" => 5,
        // 其它元素自成一类
        _ => 100,
    }
}

/// 两个标签是否属于同一角色（或都是容器），允许互相替代。
///
/// `span`（行内文本）与 `p`（文本块）算同一角色：它们承载的都是普通文本，
/// 真实站点的重构经常在两者之间切换。而 `a`（链接）、`img`、`button` 这类
/// 有明确语义的元素不参与替换，防止把正文误配到链接上。
fn same_tag_role(a: &str, b: &str) -> bool {
    if a == b {
        return true;
    }
    let (ra, rb) = (tag_role(a), tag_role(b));
    // 容器之间始终允许
    if container_tag(a) && container_tag(b) {
        return true;
    }
    // 容器与文本块之间允许（div→p 这类）
    if (ra == 1 && rb == 2) || (ra == 2 && rb == 1) {
        return true;
    }
    // 同角色内允许
    ra == rb && ra != 100
}
fn dice(a: &BTreeSet<String>, b: &BTreeSet<String>) -> f64 {
    let common = a.intersection(b).count();
    2.0 * common as f64 / (a.len() + b.len()).max(1) as f64
}

fn words(text: &str) -> BTreeSet<String> {
    text.split(|c: char| !c.is_alphanumeric())
        .filter(|s| !s.is_empty())
        .take(MAX_TOKENS)
        .map(|s| s.chars().take(64).flat_map(char::to_lowercase).collect())
        .collect()
}

fn attrs(node: NodeRef<'_>) -> BTreeSet<String> {
    let Some(el) = node.as_element() else {
        return BTreeSet::new();
    };
    // Deliberately omit values, hrefs, tokens and arbitrary data-* attributes.
    [
        "class",
        "role",
        "aria-label",
        "name",
        "type",
        "itemprop",
        "title",
    ]
    .into_iter()
    .flat_map(|a| {
        words(el.attr(a).unwrap_or(""))
            .into_iter()
            .map(move |v| format!("{a}:{v}"))
    })
    .take(MAX_TOKENS)
    .collect()
}

fn features(node: NodeRef<'_>) -> (String, BTreeSet<String>, BTreeSet<String>, BTreeSet<String>) {
    let mut text = String::new();
    for n in node.descendants().take(256) {
        if let Some(t) = n.as_text() {
            for c in t
                .chars()
                .take(2048_usize.saturating_sub(text.chars().count()))
            {
                text.push(c);
            }
            text.push(' ');
            if text.len() >= 2048 {
                break;
            }
        }
    }
    let context = node
        .ancestors()
        .take(2)
        .flat_map(attrs)
        .take(MAX_TOKENS)
        .collect();
    (
        node.as_element().unwrap().name().into(),
        words(&text),
        attrs(node),
        context,
    )
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn corroborated_container_tag_changes_and_cross_tag_duplicates() {
        let old = Html::parse("<article class='product'>Copper kettle</article>");
        let a = FuzzyAnchor::capture(&old, "article").unwrap();
        let new = Html::parse("<section class='product'>Copper kettle</section>");
        assert!(matches!(
            a.locate(&new, FuzzyOptions::default()).unwrap(),
            FuzzyMatch::Candidate { .. }
        ));
        let dup = Html::parse(
            "<article class='product'>Copper kettle</article><section class='product'>Copper kettle</section>",
        );
        assert!(matches!(
            a.locate(&dup, FuzzyOptions::default()).unwrap(),
            FuzzyMatch::Ambiguous { .. }
        ));
        let link = Html::parse("<a class='product'>Copper kettle</a>");
        assert!(matches!(
            a.locate(&link, FuzzyOptions::default()).unwrap(),
            FuzzyMatch::LowConfidence { .. }
        ));
    }
    #[test]
    fn relocation_does_not_trust_the_old_selector() {
        let old = Html::parse(
            "<main class='catalog'><article class='card chosen'>Copper kettle price 42</article></main>",
        );
        let a = FuzzyAnchor::capture(&old, ".chosen").unwrap();
        let a: FuzzyAnchor = serde_json::from_str(&serde_json::to_string(&a).unwrap()).unwrap();
        let new = Html::parse(
            "<main class='catalog'><article class='chosen'>Plastic bowl price 9</article><section><article class='card'>Copper kettle price 43</article></section></main>",
        );
        assert!(
            matches!(a.locate(&new, FuzzyOptions::default()).unwrap(), FuzzyMatch::Candidate { node, .. } if node.text().contains("Copper"))
        );
    }
    #[test]
    fn weak_and_tied_candidates_abstain() {
        let old = Html::parse("<article class='card'>Copper kettle</article>");
        let a = FuzzyAnchor::capture(&old, "article").unwrap();
        let unrelated = Html::parse("<article class='card'>Plastic bowl</article>");
        assert!(matches!(
            a.locate(&unrelated, FuzzyOptions::default()).unwrap(),
            FuzzyMatch::LowConfidence { .. }
        ));
        let dup = Html::parse(
            "<article class='card'>Copper kettle</article><article class='card'>Copper kettle</article>",
        );
        assert!(matches!(
            a.locate(&dup, FuzzyOptions::default()).unwrap(),
            FuzzyMatch::Ambiguous { .. }
        ));
    }
    #[test]
    fn validates_snapshots_thresholds_and_scan_budget() {
        let doc = Html::parse("<p>Hello</p>");
        let a = FuzzyAnchor::capture(&doc, "p").unwrap();
        assert!(
            a.locate(
                &doc,
                FuzzyOptions {
                    minimum_score: f64::NAN,
                    ..Default::default()
                }
            )
            .is_err()
        );
        let large = "<p>Hello</p>".repeat(11_000);
        let large = Html::parse(&large);
        assert!(matches!(
            a.locate(&large, FuzzyOptions::default()).unwrap(),
            FuzzyMatch::BudgetExceeded
        ));
        assert!(FuzzyAnchor::capture(&doc, "div").is_err());
    }
}
