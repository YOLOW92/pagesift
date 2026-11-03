//! 元素的结构指纹：标签、稳定属性、语义类名、文字、祖先链、兄弟上下文、子元素形状。
//!
//! 每个特征给出一个 Dice 系数；它们怎样合成证据、何时接受，见 `posterior.rs`：每个特征的分量
//! 按“文档里有多少元素在这一项上同样像”换算成似然比，所以不需要人为的权重和阈值。

use std::collections::BTreeSet;

use pagesift_html::NodeRef;
use serde::{Deserialize, Serialize};

/// 一个特征最多保留的词数。是存储预算而不是判决阈值：超出的词按文档顺序截掉，
/// 对相似度只有“少看了一些证据”的影响。值取 2⁷，和 `FuzzyAnchor`（0.91）的上限一致。
const MAX_TOKENS: usize = 128;

/// 元素的结构指纹，可序列化后持久化（`pagesift-store` 的选择器命名空间）。
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Fingerprint {
    pub tag: String,
    /// 稳定属性：`名=值`（值像哈希的只记名字）和单独的属性名。
    pub attrs: BTreeSet<String>,
    /// 不像构建工具生成哈希的类名。
    pub classes: BTreeSet<String>,
    /// 元素自身文字的词。
    pub text: BTreeSet<String>,
    /// 祖先的标签、稳定 id 和语义类名（由近到远，不记层数：包一层不改变集合）。
    pub path: BTreeSet<String>,
    /// 父元素下其他子元素的文字词。
    pub context: BTreeSet<String>,
    /// 子元素的标签。
    pub shape: BTreeSet<String>,
}

impl Fingerprint {
    pub fn of(node: NodeRef<'_>) -> Option<Self> {
        let el = node.as_element()?;
        let mut attrs = BTreeSet::new();
        let mut classes = BTreeSet::new();
        for (k, v) in el.attrs() {
            match k {
                "class" => classes.extend(semantic_classes(v)),
                "style" => {}
                _ if is_hash_like(v) => {
                    attrs.insert(k.to_ascii_lowercase());
                }
                _ => {
                    attrs.insert(k.to_ascii_lowercase());
                    attrs.insert(format!("{}={}", k.to_ascii_lowercase(), v.trim()));
                }
            }
        }
        let mut path = BTreeSet::new();
        for a in node.ancestors().filter_map(|a| a.as_element()) {
            path.insert(a.name().to_string());
            if let Some(id) = a.attr("id").filter(|v| !is_hash_like(v)) {
                path.insert(format!("#{id}"));
            }
            if let Some(c) = a.attr("class") {
                path.extend(semantic_classes(c).map(|c| format!(".{c}")));
            }
            if path.len() >= MAX_TOKENS {
                break;
            }
        }
        let mut context = BTreeSet::new();
        if let Some(parent) = node.parent() {
            for s in parent.children().filter(|s| *s != node) {
                extend_words(&mut context, &s.text());
                if context.len() >= MAX_TOKENS {
                    break;
                }
            }
        }
        let shape = node
            .children()
            .filter_map(|c| c.as_element().map(|e| e.name().to_string()))
            .take(MAX_TOKENS)
            .collect();
        let mut text = BTreeSet::new();
        extend_words(&mut text, &node.text());
        Some(Self {
            tag: el.name().to_string(),
            attrs,
            classes,
            text,
            path,
            context,
            shape,
        })
    }

    /// 和另一个指纹逐个特征的相似度（Dice 系数，0 到 1）；两边都为空的特征是 `None`（没有证据）。
    /// 顺序：标签、稳定属性、语义类名、文字、祖先、兄弟文字、子元素标签。
    pub fn features(&self, other: &Fingerprint) -> [Option<f64>; FEATURES] {
        [
            Some(f64::from(u8::from(self.tag == other.tag))),
            dice(&self.attrs, &other.attrs),
            dice(&self.classes, &other.classes),
            dice(&self.text, &other.text),
            dice(&self.path, &other.path),
            dice(&self.context, &other.context),
            dice(&self.shape, &other.shape),
        ]
    }

    /// 各特征相似度的等权平均（只用于展示和排序参考；接受与否见 `posterior.rs`）。
    pub fn similarity(&self, other: &Fingerprint) -> f64 {
        let f = self.features(other);
        let known: Vec<f64> = f.iter().flatten().copied().collect();
        known.iter().sum::<f64>() / known.len().max(1) as f64
    }
}

/// 特征个数（[`Fingerprint::features`]）。
pub const FEATURES: usize = 7; // [推导] 上面列出的特征个数

fn extend_words(set: &mut BTreeSet<String>, text: &str) {
    for w in text
        .split(|c: char| !c.is_alphanumeric())
        .filter(|w| !w.is_empty())
    {
        if set.len() >= MAX_TOKENS {
            return;
        }
        set.insert(w.to_lowercase());
    }
}

/// 两个集合的 Dice 系数；两边都为空时返回 `None`（没有证据）。
fn dice(a: &BTreeSet<String>, b: &BTreeSet<String>) -> Option<f64> {
    if a.is_empty() && b.is_empty() {
        return None;
    }
    let both = a.intersection(b).count();
    Some(2.0 * both as f64 / (a.len() + b.len()) as f64)
}

/// 不像生成哈希的类名（小写）。
pub fn semantic_classes(attr: &str) -> impl Iterator<Item = String> + '_ {
    attr.split_whitespace()
        .filter(|c| !is_hash_like(c))
        .map(str::to_lowercase)
}

/// 值像构建工具生成的哈希（CSS Modules、styled-components、emotion 等）。
///
/// 按生成器的构造规则判断，不是统计阈值：把值按 `-`、`_` 切段，任一段里字母和数字交替至少两次
/// （`1x8dfa`、`a3b9`、`3fa2b1`）即为哈希。自然语言的名字和业务编号最多在一处由字母转到数字
/// （`col2`、`h1`、`A1`、`SKU2024`），交替两次以上只出现在随机编码里。
pub fn is_hash_like(value: &str) -> bool {
    value
        .split(['-', '_'])
        .filter(|s| !s.is_empty())
        .any(|seg| {
            let mut switches = 0;
            let mut prev: Option<bool> = None;
            for c in seg.chars() {
                if !c.is_ascii_alphanumeric() {
                    prev = None;
                    continue;
                }
                let is_digit = c.is_ascii_digit();
                if prev.is_some_and(|p| p != is_digit) {
                    switches += 1;
                }
                prev = Some(is_digit);
            }
            switches >= 2
        })
}

#[cfg(test)]
mod tests {
    use super::*;
    use pagesift_html::{Html, Selector};

    #[test]
    fn hash_like_values() {
        for h in ["css-1x8dfa", "sc-a3b9", "Button_root__3fa2b1", "x9k2"] {
            assert!(is_hash_like(h), "{h}");
        }
        for ok in [
            "price",
            "col2",
            "h1",
            "product-title",
            "v2",
            "item_name",
            "A1",
            "SKU2024",
        ] {
            assert!(!is_hash_like(ok), "{ok}");
        }
    }

    #[test]
    fn wrapper_and_hashed_classes_keep_similarity_high() {
        let a = Html::parse(
            "<div class='card'><h2 class='title css-1x8dfa'>Widget</h2><span>9</span></div>",
        );
        let b = Html::parse(
            "<div class='card'><div><h2 class='title css-9zq2kk'>Widget</h2></div><span>9</span></div>",
        );
        let sel = Selector::parse("h2").unwrap();
        let fa = Fingerprint::of(a.select(&sel).next().unwrap()).unwrap();
        let fb = Fingerprint::of(b.select(&sel).next().unwrap()).unwrap();
        assert!(fa.classes.contains("title") && !fa.classes.contains("css-1x8dfa"));
        let span = Fingerprint::of(b.select(&Selector::parse("span").unwrap()).next().unwrap());
        assert!(fa.similarity(&fb) > fa.similarity(&span.unwrap()));
    }
}
