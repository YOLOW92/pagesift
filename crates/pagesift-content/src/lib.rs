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

/// 只取正文的结果少于这么多字符、并且不到整页可见文字的这个比例时，放宽规则重试。
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
    let render = |skipper: &extract::Skipper| {
        markdown::Renderer {
            base: base.as_ref(),
            skipper,
        }
        .render(&[body])
    };
    let mut markdown = render(&skipper);
    // 规则删过头的保险：结果很短、又远少于整页文字时，关掉软跳过规则重来一次
    if opts.only_main_content {
        let chars = dom::count_non_whitespace(&markdown);
        let total = skipper.total_chars();
        if chars < FALLBACK_MAX_CHARS && (chars as f64) < total as f64 * FALLBACK_MAX_SHARE {
            skipper.relax();
            markdown = render(&skipper);
        }
    }
    let barrier = opts
        .detect_barrier
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
pub fn explain_skips(html: &str) -> (bool, Vec<SkippedBlock>) {
    let dom = Html::parse(html);
    let mut skipper = extract::Skipper::new(&dom, true);
    let body = dom.get(body_or_root(&dom));
    let render = |skipper: &extract::Skipper| {
        markdown::Renderer {
            base: None,
            skipper,
        }
        .render(&[body])
    };
    let markdown = render(&skipper);
    let chars = dom::count_non_whitespace(&markdown);
    if chars < FALLBACK_MAX_CHARS
        && (chars as f64) < skipper.total_chars() as f64 * FALLBACK_MAX_SHARE
    {
        skipper.relax();
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
