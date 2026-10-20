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

use scraper::Html;
use url::Url;

pub use barrier::{Barrier, BarrierKind};
pub use fingerprint::{Fingerprints, sha256_hex};
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
    let dom = Html::parse_document(html);
    let base = effective_base(&dom, opts.base_url.as_ref());
    let mut skipper = extract::Skipper::new(&dom, opts.only_main_content);
    // 只取正文靠跳过规则去掉页面框架（导航、页眉页脚、侧边栏、链接密集块），其余整页保留。
    // 评测集上这比 Readability 式的"挑一个正文节点"更好：首页、产品页、落地页没有单一的文章节点，
    // 文章页的"文章信号"（og:type、单个 <article>）也不可靠。
    let body = dom.tree.get(body_or_root(&dom)).expect("body node exists");
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
        let chars = markdown.chars().filter(|c| !c.is_whitespace()).count();
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
