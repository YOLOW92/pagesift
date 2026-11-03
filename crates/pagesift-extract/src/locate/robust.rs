//! 给元素生成稳健的新选择器：优先稳定属性、避开哈希化的类名、最后才用 `:nth-of-type`。
//!
//! 候选按“改版时多半不变”的程度排序：
//! 1. 稳定的 `id`；
//! 2. 专门给测试和自动化留的属性（`data-testid` 等，前端约定不随样式改）；
//! 3. 语义标注（`itemprop`、表单 `name`、无障碍名称 `aria-label`、`role`）；
//! 4. 其他值不像哈希的 `data-*` 属性；
//! 5. 标签加语义类名；
//! 6. 从最近的、本身有稳健选择器的祖先往下拼，必要时逐级加 `:nth-of-type`。
//!
//! 每个候选都在 `scope` 内验证“恰好命中这一个元素”才采用。

use pagesift_html::{NodeRef, Selector};

use super::fingerprint::{is_hash_like, semantic_classes};

/// 约定用于测试和自动化的属性，按常见程度排列。
const TEST_HOOKS: &[&str] = &[
    "data-testid",
    "data-test-id",
    "data-test",
    "data-qa",
    "data-cy",
];
/// 语义标注属性。
const SEMANTIC: &[&str] = &["itemprop", "name", "aria-label", "role", "title", "for"];

/// `scope` 内恰好命中 `node` 的稳健选择器（相对 `scope`）；`scope` 是文档根时就是绝对选择器。
pub fn robust_selector(node: NodeRef<'_>, scope: NodeRef<'_>) -> Option<String> {
    if let Some(s) = own_selector(node, scope) {
        return Some(s);
    }
    // 从近到远找有自己稳健选择器的祖先，再往下拼
    let mut chain: Vec<NodeRef<'_>> = vec![node];
    for anc in node.ancestors() {
        if anc == scope || !anc.is_element() {
            break;
        }
        if let Some(base) = own_selector(anc, scope) {
            return descend(&base, &chain, scope, node);
        }
        chain.push(anc);
    }
    descend("", &chain, scope, node)
}

/// 只看元素自身（不借祖先）的稳健选择器。
fn own_selector(node: NodeRef<'_>, scope: NodeRef<'_>) -> Option<String> {
    let el = node.as_element()?;
    let tag = el.name();
    let mut candidates = Vec::new();
    if let Some(id) = el.attr("id").filter(|v| stable_value(v)) {
        candidates.push(format!("{tag}[id=\"{}\"]", quote(id)));
    }
    for attr in TEST_HOOKS.iter().chain(SEMANTIC) {
        if let Some(v) = el.attr(attr).filter(|v| stable_value(v)) {
            candidates.push(format!("{tag}[{attr}=\"{}\"]", quote(v)));
        }
    }
    for (k, v) in el.attrs() {
        if k.starts_with("data-") && !TEST_HOOKS.contains(&k) && stable_value(v) && is_ident(k) {
            candidates.push(format!("{tag}[{k}=\"{}\"]", quote(v)));
        }
    }
    let classes: Vec<String> = el
        .attr("class")
        .map(|c| semantic_classes(c).filter(|c| is_ident(c)).collect())
        .unwrap_or_default();
    if !classes.is_empty() {
        candidates.push(format!("{tag}.{}", classes.join(".")));
    }
    candidates.into_iter().find(|c| unique(c, scope, node))
}

/// 从 `base`（可为空：从 scope 开始）往下到 `node`：先试后代组合，再逐级用子组合器加 `:nth-of-type`。
fn descend(
    base: &str,
    chain: &[NodeRef<'_>],
    scope: NodeRef<'_>,
    node: NodeRef<'_>,
) -> Option<String> {
    let el = node.as_element()?;
    // 文档根没有 `:scope` 可言，从 `html` 开始写；元素作用域用 `:scope` 开头写相对选择器
    let prefix = match (base.is_empty(), scope.is_document()) {
        (false, _) => base.to_string(),
        (true, true) => String::new(),
        (true, false) => ":scope".to_string(),
    };
    let simple = format!("{prefix} {}", el.name()).trim_start().to_string();
    if unique(&simple, scope, node) {
        return Some(simple);
    }
    let mut sel = prefix;
    for n in chain.iter().rev() {
        let e = n.as_element()?;
        let step = format!("{}:nth-of-type({})", e.name(), nth_of_type(*n));
        sel = if sel.is_empty() {
            step
        } else {
            format!("{sel} > {step}")
        };
    }
    unique(&sel, scope, node).then_some(sel)
}

fn nth_of_type(node: NodeRef<'_>) -> usize {
    let name = node.as_element().map(|e| e.name());
    let mut k = 1;
    let mut cur = node.prev_sibling();
    while let Some(s) = cur {
        if s.as_element().map(|e| e.name()) == name {
            k += 1;
        }
        cur = s.prev_sibling();
    }
    k
}

fn unique(sel: &str, scope: NodeRef<'_>, node: NodeRef<'_>) -> bool {
    let Ok(s) = Selector::parse(sel) else {
        return false;
    };
    let mut hits = scope.select(&s);
    hits.next() == Some(node) && hits.next().is_none()
}

fn stable_value(v: &str) -> bool {
    let v = v.trim();
    !v.is_empty() && !is_hash_like(v) && !v.contains(['\n', '\r'])
}

fn is_ident(s: &str) -> bool {
    let mut chars = s.chars();
    chars
        .next()
        .is_some_and(|c| c.is_ascii_alphabetic() || c == '_' || c == '-')
        && chars.all(|c| c.is_ascii_alphanumeric() || c == '_' || c == '-')
}

fn quote(v: &str) -> String {
    v.trim().replace('\\', "\\\\").replace('"', "\\\"")
}

#[cfg(test)]
mod tests {
    use super::*;
    use pagesift_html::Html;

    fn pick<'a>(doc: &'a Html<'a>, sel: &str) -> NodeRef<'a> {
        doc.select(&Selector::parse(sel).unwrap()).next().unwrap()
    }

    #[test]
    fn prefers_stable_attributes_then_classes_then_nth() {
        let doc = Html::parse(
            "<main><div class='css-1x8dfa'><span data-testid='price'>9</span></div>\
             <ul><li class='item'>a</li><li class='item'>b</li></ul><p>x</p><p>y</p></main>",
        );
        let root = doc.root();
        assert_eq!(
            robust_selector(pick(&doc, "span"), root).unwrap(),
            "span[data-testid=\"price\"]"
        );
        let second = pick(&doc, "li:nth-child(2)");
        let s = robust_selector(second, root).unwrap();
        assert!(s.ends_with("li:nth-of-type(2)"), "{s}");
        let p = robust_selector(pick(&doc, "p:nth-of-type(2)"), root).unwrap();
        let hit = doc.select(&Selector::parse(&p).unwrap()).next().unwrap();
        assert_eq!(hit.text(), "y");
    }
}
