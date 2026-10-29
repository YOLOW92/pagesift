//! Firecrawl 的 `includeTags` / `excludeTags`：正文提取之前按 CSS 选择器筛选页面。
//!
//! - `excludeTags`：命中的元素连同子树删掉。
//! - `includeTags`：body 里只留命中的元素（按文档顺序，嵌套命中只算最外层），
//!   `<head>` 原样保留，标题、描述等元数据照常提取。
//!
//! 每一项可以是标签名（`article`）、类（`.post`）、id（`#main`）或任意 CSS 选择器。

use pagesift_html::{Html, NodeRef, Selector, Tag};

/// 解析好的标签筛选规则。
#[derive(Debug, Clone)]
pub struct TagFilter {
    include: Vec<Selector>,
    exclude: Vec<Selector>,
}

impl TagFilter {
    /// 两个列表都为空（或只有空白项）时返回 `Ok(None)`；有选择器解析不了时返回说明哪一项出错的消息。
    pub fn new(include: &[String], exclude: &[String]) -> Result<Option<Self>, String> {
        let include = parse_all(include, "includeTags")?;
        let exclude = parse_all(exclude, "excludeTags")?;
        if include.is_empty() && exclude.is_empty() {
            return Ok(None);
        }
        Ok(Some(Self { include, exclude }))
    }

    /// 返回筛选后的 HTML。
    pub fn apply(&self, html: &str) -> String {
        let dom = Html::parse(html);
        let excluded = |n: NodeRef<'_>| n.is_element() && self.exclude.iter().any(|s| s.matches(n));
        if self.include.is_empty() {
            return dom.root().html_without(&excluded);
        }
        let head = dom
            .root()
            .descendants()
            .find(|n| n.tag() == Some(Tag::Head));
        let mut out = String::with_capacity(html.len() / 2);
        out.push_str("<html><head>");
        if let Some(head) = head {
            for c in head.children() {
                out.push_str(&c.html_without(&excluded));
            }
        }
        out.push_str("</head><body>");
        let included = |n: NodeRef<'_>| self.include.iter().any(|s| s.matches(n));
        for n in dom.body().descendants() {
            if !n.is_element() || !included(n) || excluded(n) {
                continue;
            }
            // 祖先已经整棵写出（或被删掉）时不再重复
            if n.ancestors()
                .any(|a| a.is_element() && (included(a) || excluded(a)))
            {
                continue;
            }
            out.push_str(&n.html_without(&excluded));
        }
        out.push_str("</body></html>");
        out
    }
}

fn parse_all(items: &[String], field: &str) -> Result<Vec<Selector>, String> {
    items
        .iter()
        .map(|s| s.trim())
        .filter(|s| !s.is_empty())
        .map(|s| Selector::parse(s).map_err(|e| format!("{field}: invalid selector {s:?}: {e}")))
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn filter(include: &[&str], exclude: &[&str]) -> TagFilter {
        let v = |xs: &[&str]| xs.iter().map(|s| s.to_string()).collect::<Vec<_>>();
        TagFilter::new(&v(include), &v(exclude)).unwrap().unwrap()
    }

    const PAGE: &str = "<html><head><title>T</title></head><body>\
        <nav>menu</nav><article class=\"post\"><h1>A</h1><p>x<span class=\"ad\">ad</span></p></article>\
        <div id=\"side\"><article><p>nested</p></article></div><footer>f</footer></body></html>";

    #[test]
    fn empty_lists_mean_no_filter() {
        assert!(TagFilter::new(&[], &[" ".into()]).unwrap().is_none());
    }

    #[test]
    fn invalid_selector_names_the_field() {
        let err = TagFilter::new(&["p[".into()], &[]).unwrap_err();
        assert!(
            err.starts_with("includeTags: invalid selector \"p[\""),
            "{err}"
        );
    }

    #[test]
    fn exclude_removes_subtrees() {
        let out = filter(&[], &["nav", ".ad", "#side"]).apply(PAGE);
        assert!(out.contains("<title>T</title>"), "{out}");
        assert!(out.contains("<h1>A</h1><p>x</p>"), "{out}");
        assert!(
            !out.contains("menu") && !out.contains("ad<") && !out.contains("nested"),
            "{out}"
        );
        assert!(out.contains("<footer>f</footer>"), "{out}");
    }

    #[test]
    fn include_keeps_head_and_outermost_matches_in_order() {
        let out = filter(&["article", "footer"], &[".ad"]).apply(PAGE);
        assert_eq!(
            out,
            "<html><head><title>T</title></head><body>\
             <article class=\"post\"><h1>A</h1><p>x</p></article>\
             <article><p>nested</p></article><footer>f</footer></body></html>"
        );
        // 嵌套命中只写一次
        let out = filter(&["#side", "article"], &[]).apply(PAGE);
        assert_eq!(out.matches("nested").count(), 1, "{out}");
    }
}
