//! pagesift 的内容处理核心：把 HTML 变成干净的 Markdown 和元数据。
//!
//! ```
//! let html = "<html><body><nav>Home</nav><article><h1>Hi</h1><p>Hello, world.</p></article></body></html>";
//! let doc = pagesift_content::process(html, &pagesift_content::Options::default());
//! assert_eq!(doc.markdown, "# Hi\n\nHello, world.\n");
//! ```

#![forbid(unsafe_code)]

mod dom;
mod extract;
mod markdown;
mod metadata;

use scraper::Html;
use url::Url;

pub use metadata::Metadata;

#[derive(Debug, Clone)]
pub struct Options {
    /// 页面的最终 URL，用来把相对链接和图片地址转成绝对地址。
    pub base_url: Option<Url>,
    /// 只保留正文（对应 Firecrawl 的 `onlyMainContent`，默认开启）。
    pub only_main_content: bool,
}

impl Default for Options {
    fn default() -> Self {
        Self {
            base_url: None,
            only_main_content: true,
        }
    }
}

#[derive(Debug, Clone)]
pub struct Document {
    pub markdown: String,
    pub metadata: Metadata,
}

pub fn process(html: &str, opts: &Options) -> Document {
    let dom = Html::parse_document(html);
    let base = effective_base(&dom, opts.base_url.as_ref());
    let skipper = extract::Skipper::new(&dom, opts.only_main_content);
    let root_ids = if opts.only_main_content {
        extract::main_roots(&dom, &skipper)
    } else {
        vec![body_or_root(&dom)]
    };
    let roots: Vec<_> = root_ids
        .iter()
        .map(|id| dom.tree.get(*id).expect("root node exists"))
        .collect();
    let renderer = markdown::Renderer {
        base: base.as_ref(),
        skipper: &skipper,
    };
    let metadata = metadata::extract(&dom, base.as_ref());
    let mut markdown = renderer.render(&roots);
    if opts.only_main_content
        && let Some(title) = outside_title(&dom, &roots, &metadata, &renderer)
    {
        markdown = if markdown.is_empty() {
            format!("# {title}\n")
        } else {
            format!("# {title}\n\n{markdown}")
        };
    }
    Document { markdown, metadata }
}

/// 很多站点把文章标题放在正文容器外面（和正文是兄弟节点）。
/// 正文里没有和 `<title>` 对得上的 h1/h2、而正文前面有时，把它补到开头。
fn outside_title(
    dom: &Html,
    roots: &[ego_tree::NodeRef<'_, scraper::Node>],
    metadata: &Metadata,
    renderer: &markdown::Renderer<'_>,
) -> Option<String> {
    let is_heading = |n: &ego_tree::NodeRef<'_, scraper::Node>| {
        n.value()
            .as_element()
            .is_some_and(|e| matches!(e.name(), "h1" | "h2"))
    };
    let page_title = metadata.title.as_deref()?.to_lowercase();
    let matches_title = |n: ego_tree::NodeRef<'_, scraper::Node>| {
        let text = renderer.heading_text(n);
        (text.chars().count() >= 4 && page_title.contains(&text.to_lowercase())).then_some(text)
    };
    if roots
        .iter()
        .flat_map(|r| r.descendants())
        .any(|n| is_heading(&n) && matches_title(n).is_some())
    {
        return None;
    }
    let first = roots.first()?.id();
    for node in dom.tree.root().descendants() {
        if node.id() == first {
            break;
        }
        if !is_heading(&node) || renderer.skipped(&node) {
            continue;
        }
        if let Some(text) = matches_title(node) {
            return Some(text);
        }
    }
    None
}

fn body_or_root(dom: &Html) -> ego_tree::NodeId {
    dom.tree
        .root()
        .descendants()
        .find(|n| n.value().as_element().is_some_and(|e| e.name() == "body"))
        .unwrap_or_else(|| dom.tree.root())
        .id()
}

/// 页面里的 `<base href>` 优先于调用方给的 URL。
fn effective_base(dom: &Html, given: Option<&Url>) -> Option<Url> {
    let href = dom.tree.root().descendants().find_map(|n| {
        let el = n.value().as_element()?;
        (el.name() == "base").then(|| el.attr("href")).flatten()
    });
    match (href, given) {
        (Some(h), Some(g)) => g.join(h).ok().or_else(|| Some(g.clone())),
        (Some(h), None) => Url::parse(h).ok(),
        (None, g) => g.cloned(),
    }
}
