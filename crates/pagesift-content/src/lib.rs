//! pagesift 的内容处理核心：把 HTML 变成干净的 Markdown 和元数据。
//!
//! ```
//! let html = "<html><body><nav>Home</nav><article><h1>Hi</h1><p>Hello, world.</p></article></body></html>";
//! let doc = pagesift_content::process(html, &pagesift_content::Options::default());
//! assert_eq!(doc.markdown, "# Hi\n\nHello, world.\n");
//! ```

#![forbid(unsafe_code)]

mod barrier;
mod clean_html;
mod datetime;
mod dom;
mod extract;
mod fingerprint;
mod jsonld;
mod links;
mod markdown;
mod metadata;
mod text;

use extract::Relax;
use pagesift_html::Html;
use url::Url;

pub use barrier::{Barrier, BarrierKind};
pub use fingerprint::{Fingerprints, hash_hex};
pub use links::{Image, Link, same_site};
pub use metadata::Metadata;
pub use text::{estimate_tokens, normalize_text};

#[derive(Debug, Clone)]
pub struct Options {
    /// 页面的最终 URL，用来把相对链接和图片地址转成绝对地址。
    pub base_url: Option<Url>,
    /// 只保留正文（对应 Firecrawl 的 `onlyMainContent`，默认开启）。
    pub only_main_content: bool,
    /// 响应的 HTTP 状态码（可选），拦截页识别会参考它。
    pub status: Option<u16>,
    /// 识别拦截页，结果放进 [`Document::barrier`]（默认开启）。
    pub detect_barrier: bool,
    /// 输出 [`Document::plain_text`]。
    pub plain_text: bool,
    /// 输出 [`Document::html`]。
    pub clean_html: bool,
    /// 输出 [`Document::links`]。
    pub links: bool,
    /// 输出 [`Document::images`]。
    pub images: bool,
    /// 输出 [`Document::fingerprints`]。
    pub fingerprints: bool,
}

impl Default for Options {
    fn default() -> Self {
        Self {
            base_url: None,
            only_main_content: true,
            status: None,
            detect_barrier: true,
            plain_text: false,
            clean_html: false,
            links: false,
            images: false,
            fingerprints: false,
        }
    }
}

/// 处理结果。`Option` 字段只在 [`Options`] 打开对应开关时计算。
#[derive(Debug, Clone)]
pub struct Document {
    pub markdown: String,
    pub metadata: Metadata,
    /// 识别出的拦截页。这里只给判断，怎么处理（比如不返回正文）由调用方决定。
    pub barrier: Option<Barrier>,
    /// 纯文本，和 Markdown 用同一套跳过规则。
    pub plain_text: Option<String>,
    /// 清洗后的 HTML 片段（`<body>` 的内容）。
    pub html: Option<String>,
    /// 全页链接。
    pub links: Option<Vec<Link>>,
    /// 全页图片。
    pub images: Option<Vec<Image>>,
    pub fingerprints: Option<Fingerprints>,
}

/// 只取正文的结果文字少于这么多字符、并且不到整页可见文字的这个比例时，放宽规则重试。
/// 阈值由 `pagesift bench --auto` 扫描确定。
const FALLBACK_MAX_CHARS: usize = 1000;
const FALLBACK_MAX_SHARE: f64 = 0.2;

pub fn process(html: &str, opts: &Options) -> Document {
    let dom = Html::parse(html);
    let base = effective_base(&dom, opts.base_url.as_ref());
    let mut skipper = extract::Skipper::new(&dom, opts.only_main_content);
    // 只取正文靠跳过规则去掉页面框架（导航、页眉页脚、侧边栏、链接密集块），其余整页保留。
    // 评测集上这比 Readability 式的"挑一个正文节点"更好：首页、产品页、落地页没有单一的文章节点，
    // 文章页的"文章信号"（og:type、单个 <article>）也不可靠。
    let body = dom.get(body_or_root(&dom));
    let metadata = metadata::extract(&dom, base.as_ref());
    let markdown = render_main(&mut skipper, body, base.as_ref());
    // 只取正文时可见文字数已经算好（和拦截页识别的口径一样）：超过上限的页面直接跳过识别
    let surely_content =
        opts.only_main_content && skipper.total_chars() > barrier::visible_limit(opts.status);
    let barrier = (opts.detect_barrier && !surely_content)
        .then(|| barrier::detect(&dom, html, metadata.title.as_deref(), opts.status))
        .flatten();
    let plain_text = (opts.plain_text || opts.fingerprints).then(|| text::render(body, &skipper));
    let fingerprints = opts
        .fingerprints
        .then(|| fingerprint::compute(html, &markdown, plain_text.as_deref().unwrap_or_default()));
    Document {
        html: opts
            .clean_html
            .then(|| clean_html::render(body, &skipper, base.as_ref())),
        links: opts.links.then(|| links::links(&dom, base.as_ref())),
        images: opts.images.then(|| links::images(&dom, base.as_ref())),
        plain_text: plain_text.filter(|_| opts.plain_text),
        markdown,
        metadata,
        barrier,
        fingerprints,
    }
}

/// 渲染 Markdown。只取正文时带上规则删过头的保险：结果的文字很短、又远少于整页文字时，
/// 先只保留链接块规则重来一次，还不够再关掉全部软跳过规则。只数文字不数链接地址：
/// 图片和链接多的页面，地址会把很短的正文撑过阈值。
fn render_main(
    skipper: &mut extract::Skipper,
    body: pagesift_html::NodeRef<'_>,
    base: Option<&Url>,
) -> String {
    let render = |skipper: &extract::Skipper| markdown::Renderer { base, skipper }.render(&[body]);
    let mut markdown = render(skipper);
    if !skipper.only_main() {
        return markdown;
    }
    let total = skipper.total_chars();
    let short = |md: &str| {
        let chars = text_chars(md);
        chars < FALLBACK_MAX_CHARS && (chars as f64) < total as f64 * FALLBACK_MAX_SHARE
    };
    for level in [Relax::LinkBlocks, Relax::All] {
        if !short(&markdown) {
            break;
        }
        skipper.relax(level);
        markdown = render(skipper);
    }
    markdown
}

/// Markdown 里可见文字的非空白字符数：链接和图片的地址 `](...)` 不算。
fn text_chars(md: &str) -> usize {
    let mut n = 0;
    let mut rest = md;
    // 按字符找 `]`（走 memchr），再看后面是不是 `(`
    while let Some(i) = rest.find(']') {
        n += dom::count_non_whitespace(&rest[..i]);
        rest = &rest[i + 1..];
        if rest.starts_with('(') {
            rest = rest.find(')').map_or("", |j| &rest[j + 1..]);
        }
    }
    n + dom::count_non_whitespace(rest)
}

fn body_or_root(dom: &Html<'_>) -> pagesift_html::NodeId {
    dom.root()
        .descendants()
        .find(|n| n.as_element().is_some_and(|e| e.name() == "body"))
        .unwrap_or_else(|| dom.root())
        .id()
}

/// 页面里的 `<base href>` 优先于调用方给的 URL。
fn effective_base(dom: &Html<'_>, given: Option<&Url>) -> Option<Url> {
    let href = dom.root().descendants().find_map(|n| {
        let el = n.as_element()?;
        (el.name() == "base").then(|| el.attr("href")).flatten()
    });
    match (href, given) {
        (Some(h), Some(g)) => g.join(h).ok().or_else(|| Some(g.clone())),
        (Some(h), None) => Url::parse(h).ok(),
        (None, g) => g.cloned(),
    }
}

/// 只取正文时被跳过的一个块（最外层的那个，子孙不再列出）。诊断用。
#[derive(Debug, Clone)]
pub struct SkippedBlock {
    /// 标签名，带上 id 和 class，例如 `div#main.sidebar.left`。
    pub element: String,
    /// 命中的规则。
    pub reason: &'static str,
    /// 块里可见文字的非空白字符数。
    pub chars: usize,
    /// 文字开头一段。
    pub snippet: String,
}

/// 解释只取正文时删掉了什么：和 [`process`] 一样的流程（包括结果太短时放宽规则重试），
/// 返回是否放宽了规则，以及每个被跳过的最外层块和原因。只用于调试提取规则，不在正常流程里调用。
#[doc(hidden)]
pub fn explain_skips(html: &str, allow_relax: bool) -> (bool, Vec<SkippedBlock>) {
    let dom = Html::parse(html);
    let mut skipper = extract::Skipper::new(&dom, true);
    let body = dom.get(body_or_root(&dom));
    if allow_relax {
        render_main(&mut skipper, body, None);
    }
    let mut out = Vec::new();
    let mut stack = vec![body];
    while let Some(node) = stack.pop() {
        if let Some(reason) = skipper.reason(&node) {
            if let Some(el) = node.as_element() {
                let mut name = el.name().to_string();
                if let Some(id) = el.attr("id") {
                    name.push('#');
                    name.push_str(id);
                }
                for c in el.attr("class").unwrap_or_default().split_whitespace() {
                    name.push('.');
                    name.push_str(c);
                }
                let text = dom::collapse_whitespace(&node.text());
                out.push(SkippedBlock {
                    element: name,
                    reason,
                    chars: skipper.visible_chars(&node),
                    snippet: text.trim().chars().take(100).collect(),
                });
            }
            continue;
        }
        let start = stack.len();
        stack.extend(node.children());
        stack[start..].reverse();
    }
    (skipper.is_relaxed(), out)
}

/// 评测短语在只取正文时落在哪里，每个短语一项：`None` 表示页面里找不到；
/// `Some((Some(reason), element))` 表示被这条规则删掉了（`element` 是命中规则的元素）；
/// `Some((None, chain))` 表示保留了，`chain` 是保留下来的那一处往上的祖先链，
/// 每层带 `[可见文字数/链接文字数/链接数]`。短语出现在多处时，优先报告保留下来的那一处。
/// 只用于调试提取规则，不在正常流程里调用。
#[doc(hidden)]
pub fn explain_phrases(
    html: &str,
    phrases: &[String],
) -> Vec<Option<(Option<&'static str>, String)>> {
    let dom = Html::parse(html);
    let mut skipper = extract::Skipper::new(&dom, true);
    let body = dom.get(body_or_root(&dom));
    render_main(&mut skipper, body, None);
    let describe = |n: &pagesift_html::NodeRef<'_>| {
        let Some(el) = n.as_element() else {
            return String::new();
        };
        let mut name = el.name().to_string();
        if let Some(id) = el.attr("id") {
            name.push('#');
            name.push_str(id);
        }
        for c in el
            .attr("class")
            .unwrap_or_default()
            .split_whitespace()
            .take(3)
        {
            name.push('.');
            name.push_str(c);
        }
        name
    };
    // 从外往里第一个被跳过的祖先（含自身）；都没被跳过时给出往上的祖先链
    let locate = |node: pagesift_html::NodeRef<'_>| {
        let mut chain: Vec<_> = std::iter::once(node)
            .chain(node.ancestors())
            .filter(|a| a.is_element())
            .collect();
        chain.reverse();
        if let Some((r, a)) = chain.iter().find_map(|a| skipper.reason(a).map(|r| (r, a))) {
            return (Some(r), describe(a));
        }
        let path: Vec<String> = chain
            .iter()
            .rev()
            .take(8)
            .map(|a| {
                let (c, lc, l) = skipper.stats(a);
                format!("{}[{c}/{lc}/{l}]", describe(a))
            })
            .collect();
        (None, path.join(" < "))
    };
    let lower = |s: &str| dom::collapse_whitespace(&s.to_lowercase());
    phrases
        .iter()
        .map(|p| {
            let key: String = lower(p).chars().take(24).collect();
            let mut found = None;
            for text in body
                .descendants()
                .filter(|n| n.as_text().is_some_and(|t| lower(t).contains(&key)))
            {
                let r = locate(text.parent().unwrap_or(text));
                if r.0.is_none() {
                    return Some(r);
                }
                found.get_or_insert(r);
            }
            if found.is_some() {
                return found;
            }
            // 跨多个文本节点的短语：找文字包含它的最小元素
            let el = body
                .descendants()
                .filter(|n| n.is_element() && lower(&n.text()).contains(&key))
                .last()?;
            Some(locate(el))
        })
        .collect()
}
