//! pagesift 的内容处理核心：把 HTML 变成干净的 Markdown 和元数据（由 [`yolowhtml`] 驱动）。
//!
//! ```
//! let html = "<html><body><nav>Home</nav><article><h1>Hi</h1><p>Hello, world.</p></article></body></html>";
//! let doc = pagesift_content::process(html, &pagesift_content::Options::default());
//! assert_eq!(doc.markdown, "# Hi\n\nHello, world.\n");
//! ```

#![forbid(unsafe_code)]

pub use yolowhtml::*;

/// Reusable single-DOM tag filter (`includeTags` / `excludeTags`) that operates directly on an
/// already-parsed [`yolowhtml_dom::Html`] tree instead of re-parsing raw HTML strings.
#[derive(Debug, Clone)]
pub struct DomTagFilter {
    include: Vec<yolowhtml_dom::Selector>,
    exclude: Vec<yolowhtml_dom::Selector>,
}

impl DomTagFilter {
    /// Parse `includeTags` and `excludeTags` selectors. Returns `Ok(None)` when both lists are
    /// empty (or whitespace-only), matching [`TagFilter::new`] error messages verbatim.
    pub fn new(include: &[String], exclude: &[String]) -> Result<Option<Self>, String> {
        let include = parse_selectors(include, "includeTags")?;
        let exclude = parse_selectors(exclude, "excludeTags")?;
        if include.is_empty() && exclude.is_empty() {
            return Ok(None);
        }
        Ok(Some(Self { include, exclude }))
    }

    /// Render the filtered HTML fragment from an already-parsed DOM tree without building a second
    /// DOM for `html`.
    pub fn apply_dom(&self, dom: &yolowhtml_dom::Html<'_>) -> String {
        use yolowhtml_dom::{NodeRef, Tag};
        let excluded = |n: NodeRef<'_>| n.is_element() && self.exclude.iter().any(|s| s.matches(n));
        if self.include.is_empty() {
            return dom.root().html_without(&excluded);
        }
        let head = dom
            .root()
            .descendants()
            .find(|n| n.tag() == Some(Tag::Head));
        // [策略] 包装骨架 html/head/body 与常见 head 元数据的初始预分配容量
        let mut out = String::with_capacity(256);
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

    /// Parse `html` with `scratch`, apply the filter on that DOM, and run `inspect` on the same
    /// original DOM before recycling its arena allocations.
    pub fn apply_with_scratch_then<R>(
        &self,
        html: &str,
        scratch: &mut yolowhtml_dom::ParseScratch,
        inspect: impl FnOnce(&yolowhtml_dom::Html<'_>) -> R,
    ) -> (String, R) {
        with_parsed_dom(html, scratch, |dom| (self.apply_dom(dom), inspect(dom)))
    }
}

/// Parse `html` using reusable worker `scratch` buffers, run `f` on the resulting DOM, and
/// recycle node/attribute/entity buffers back into `scratch`.
#[inline]
pub fn with_parsed_dom<R>(
    html: &str,
    scratch: &mut yolowhtml_dom::ParseScratch,
    f: impl FnOnce(&yolowhtml_dom::Html<'_>) -> R,
) -> R {
    let dom = yolowhtml_dom::Html::parse_with_scratch(html, scratch);
    let out = f(&dom);
    scratch.recycle(dom);
    out
}

fn parse_selectors(items: &[String], field: &str) -> Result<Vec<yolowhtml_dom::Selector>, String> {
    items
        .iter()
        .map(|s| s.trim())
        .filter(|s| !s.is_empty())
        .map(|s| {
            yolowhtml_dom::Selector::parse(s)
                .map_err(|e| format!("{field}: invalid selector {s:?}: {e}"))
        })
        .collect()
}
