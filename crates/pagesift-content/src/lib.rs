//! pagesift 的内容处理核心：把 HTML 变成干净的 Markdown 和元数据。
//!
//! ```
//! let html = "<html><body><nav>Home</nav><article><h1>Hi</h1><p>Hello, world.</p></article></body></html>";
//! let doc = pagesift_content::process(html, &pagesift_content::Options::default());
//! assert_eq!(doc.markdown, "# Hi\n\nHello, world.\n");
//! ```

#![forbid(unsafe_code)]

mod article;
mod barrier;
mod classes;
mod clean_html;
mod coverage;
mod dates;
mod dom;
mod editorial;
mod extract;
mod features;
mod fingerprint;
mod jsonld;
mod landmarks;
mod links;
mod markdown;
mod math;
mod metadata;
mod readability;
mod script_links;
mod text;

use extract::Relax;
use pagesift_html::Html;
use url::Url;

pub use barrier::{Barrier, BarrierKind};
pub use coverage::Coverage;
#[doc(hidden)]
pub use dates::{time_published, trace_published};
pub use fingerprint::{Fingerprints, hash_hex};
pub use links::{Image, Link, same_site};
pub use metadata::Metadata;
pub use script_links::{ScriptScan, scan_script};
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
    /// 正文完整度：结果可能只有正文一部分时给出警告（[`Coverage::warnings`]）。
    pub coverage: Coverage,
}

/// Per-worker reusable allocations for the content pipeline. Keep one of
/// these on the owning NUMA worker/reactor and never move it between workers.
#[derive(Default)]
pub struct ProcessWorkspace {
    html: pagesift_html::ParseScratch,
    extract: extract::Scratch,
}

/// 只取正文的结果文字少于这么多字符、并且不到整页可见文字的这个比例时，放宽规则重试。
/// 阈值由 `pagesift bench --auto` 扫描确定。
const FALLBACK_MAX_CHARS: usize = 1000;
const FALLBACK_MAX_SHARE: f64 = 0.2;

/// 只提取页面元数据（标题、作者、发布/修改日期、语言等），不做正文抽取和 Markdown 渲染。
pub fn extract_metadata(html: &str, opts: &Options) -> Metadata {
    let dom = Html::parse(html);
    let base = effective_base(&dom, opts.base_url.as_ref());
    metadata::extract(&dom, html, base.as_ref())
}

pub fn process(html: &str, opts: &Options) -> Document {
    let dom = Html::parse(html);
    process_dom(html, opts, &dom, None)
}

/// Process a page while reusing DOM backing allocations owned by the calling
/// worker. Output strings remain independently owned by the returned document.
pub fn process_with_workspace(
    html: &str,
    opts: &Options,
    workspace: &mut ProcessWorkspace,
) -> Document {
    let dom = Html::parse_with_scratch(html, &mut workspace.html);
    let document = process_dom(html, opts, &dom, Some(&mut workspace.extract));
    workspace.html.recycle(dom);
    document
}

/// 是否对所有页面都强制尝试正文定位（对照用，默认关闭）。
///
/// 默认行为是**按结构判断**：只有声明为文章、或结构像文章（见
/// [`article::looks_like_article`]）的页面才收窄，其余保持整页输出。
/// 设 `PAGESIFT_FORCE_ARTICLE=1` 可强制全页收窄，用于复现旧实验。
///
/// 只读一次：本函数在 `process_dom` 里逐页调用，每次都走 `std::env::var_os`
/// 会带一把全局锁并扫描 environ，在高吞吐下是可测的开销。
fn try_article_everywhere() -> bool {
    static CACHED: std::sync::OnceLock<bool> = std::sync::OnceLock::new();
    *CACHED.get_or_init(|| std::env::var_os("PAGESIFT_FORCE_ARTICLE").is_some())
}

fn process_dom(
    html: &str,
    opts: &Options,
    dom: &Html<'_>,
    scratch: Option<&mut extract::Scratch>,
) -> Document {
    let base = effective_base(dom, opts.base_url.as_ref());
    let mut local_scratch = extract::Scratch::default();
    let scratch = match scratch {
        Some(scratch) => scratch,
        None => &mut local_scratch,
    };
    let mut skipper = extract::Skipper::new_with_scratch(dom, opts.only_main_content, scratch);
    skipper.set_has_math(math::may_have_math(html));
    // 只取正文靠跳过规则去掉页面框架（导航、页眉页脚、侧边栏、链接密集块），其余整页保留。
    // 评测集上这比 Readability 式的"挑一个正文节点"更好：首页、产品页、落地页没有单一的文章节点，
    // 文章页的"文章信号"（og:type、单个 <article>）也不可靠。
    let body = dom.get(body_or_root(dom));
    let mut metadata = metadata::extract(dom, html, base.as_ref());
    // 文章页先试文章模式，成功就不用再按默认规则渲染一遍
    // 网站首页是列表页，不是一篇文章（很多站点在首页也写文章类的 JSON-LD）
    let home = base.as_ref().is_some_and(|u| {
        let p = u.path().trim_end_matches('/');
        p.is_empty() || p.starts_with("/index.") && !p[1..].contains('/')
    });
    // 正文收窄只在两种情况下启用：
    //   1) 页面自己声明是文章（JSON-LD / og:type）；
    //   2) 结构上不是明确的列表/卡片/链接墙（见 article::clearly_not_article）。
    // 只做排除不做确认：实测"确认像文章"会误杀大量真文章页（WebMainBench −0.039），
    // 而只排除明显的非文章页几乎不损失收益。
    let mut root_in_comments = false;
    let article_shaped = !metadata.is_article && !article::clearly_not_article(dom, &skipper);
    let article = (opts.only_main_content
        && (metadata.is_article || article_shaped || try_article_everywhere())
        && !home)
        .then(|| {
            let comments = &mut root_in_comments;
            render_article(dom, &mut skipper, base.as_ref(), article_shaped, comments)
        })
        .flatten();

    // `unmarked_root` 是给没有文章标记的旧版式用的。它按"标题匹配 + 附近有成段文字"挑一个区域，
    // 碰上"导语在独立的小 `<article>` 里、正文在兄弟节点"的页面（食谱站常见）会只挑到导语。
    // 挑出来的区域连整页的 20% 都不到时，宁可回退到整页默认规则，也不要把正文丢掉。
    let inferred = (opts.only_main_content && !metadata.is_article && !home)
        .then(|| editorial::unmarked_root(dom, &skipper))
        .flatten()
        // <template> 里的内容浏览器不显示；整页可见文字为 0 时下面的占比检查会直接放行，先排除
        .filter(|root| !dom::in_template(root))
        .filter(|root| {
            let page_chars = article::text_without_comments(&body, &skipper);
            page_chars == 0
                || article::text_without_comments(root, &skipper) as f64
                    >= page_chars as f64 * UNMARKED_MIN_SHARE
        });
    let output_body = inferred.unwrap_or(body);
    let mut markdown = match article {
        Some(md) => md,
        None if inferred.is_some() => {
            markdown::Renderer::new(base.as_ref(), &skipper).render(&[output_body])
        }
        None => render_main(&mut skipper, body, base.as_ref()),
    };
    // 还是几乎为空、页面又有 <noscript>：有的单页应用只在 <noscript> 里给不跑脚本的抓取器留一份内容。
    // 按 HTML 规范 <noscript> 里是纯文本，拆掉这层标签重新解析整页再抽一次，明显更长才用
    if text_chars(&markdown) < HIDDEN_FALLBACK_CHARS
        && let Some(unwrapped) = unwrap_noscript(html)
    {
        let alt = process(&unwrapped, opts).markdown;
        if text_chars(&alt) >= HIDDEN_FALLBACK_CHARS.max(3 * text_chars(&markdown)) {
            markdown = alt;
        }
    }

    // 视频页（YouTube 等）正文在播放器 JSON 里，HTML 路径看不到。正文为空时用页面声明的
    // VideoObject 补标题和简介，比交白卷有用，也比伪造正文诚实。
    if text_chars(&markdown) < VIDEO_FALLBACK_MAX_CHARS
        && let Some(video) = jsonld::video_markdown(
            &metadata.json_ld,
            metadata.title.as_deref(),
            metadata.description.as_deref(),
        )
    {
        markdown = video;
        metadata.video_fallback = true;
    }

    // 只取正文时可见文字数已经算好（和拦截页识别的口径一样）：超过上限的页面直接跳过识别
    let surely_content =
        opts.only_main_content && skipper.total_chars() > barrier::visible_limit(opts.status);
    let barrier = (opts.detect_barrier && !surely_content)
        .then(|| barrier::detect(dom, html, metadata.title.as_deref(), opts.status))
        .flatten();
    let plain_text =
        (opts.plain_text || opts.fingerprints).then(|| text::render(output_body, &skipper));
    let fingerprints = opts
        .fingerprints
        .then(|| fingerprint::compute(html, &markdown, plain_text.as_deref().unwrap_or_default()));
    let coverage = coverage::measure(&body, &skipper, &markdown, root_in_comments);
    let document = Document {
        coverage,
        html: opts
            .clean_html
            .then(|| clean_html::render(output_body, &skipper, base.as_ref())),
        links: opts.links.then(|| links::links(dom, base.as_ref())),
        images: opts.images.then(|| links::images(dom, base.as_ref())),
        plain_text: plain_text.filter(|_| opts.plain_text),
        markdown,
        metadata,
        barrier,
        fingerprints,
    };
    skipper.recycle(scratch);
    document
}

/// 渲染 Markdown。只取正文时带上规则删过头的保险：结果的文字很短、又远少于整页文字时，
/// 先只保留链接块规则重来一次，还不够再关掉全部软跳过规则。只数文字不数链接地址：
/// 图片和链接多的页面，地址会把很短的正文撑过阈值。
fn render_main(
    skipper: &mut extract::Skipper,
    body: pagesift_html::NodeRef<'_>,
    base: Option<&Url>,
) -> String {
    let render =
        |skipper: &extract::Skipper| markdown::Renderer::new(base, skipper).render(&[body]);
    let mut markdown = render(skipper);
    if skipper.only_main() {
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
    }
    // 还是几乎为空：内容可能整页藏在 display:none 里等脚本显示，或者只在 <noscript> 里。
    // 连隐藏内容一起渲染，明显更长才用（普通的短页面不受影响）
    if text_chars(&markdown) < HIDDEN_FALLBACK_CHARS {
        let before = skipper.relaxed();
        skipper.relax(Relax::Hidden);
        let shown = render(skipper);
        if text_chars(&shown) >= HIDDEN_FALLBACK_CHARS.max(3 * text_chars(&markdown)) {
            return shown;
        }
        skipper.relax(before);
    }
    markdown
}

/// 去掉 `<noscript>` 和 `</noscript>` 这两层标签、保留里面的内容。页面没有 `<noscript>` 时返回 `None`。
fn unwrap_noscript(html: &str) -> Option<String> {
    let lower = html.to_ascii_lowercase();
    let mut at = lower.find("<noscript")?;
    let mut out = String::with_capacity(html.len());
    let mut from = 0;
    loop {
        out.push_str(&html[from..at]);
        // 开标签到 '>' 为止；没有就原样保留剩余部分
        let Some(gt) = lower[at..].find('>') else {
            from = at;
            break;
        };
        from = at + gt + 1;
        let close = lower[from..].find("</noscript").map(|i| from + i);
        match close {
            Some(c) => {
                out.push_str(&html[from..c]);
                from = lower[c..].find('>').map_or(html.len(), |g| c + g + 1);
            }
            None => break,
        }
        match lower[from..].find("<noscript") {
            Some(i) => at = from + i,
            None => break,
        }
    }
    out.push_str(&html[from..]);
    // 标签不完整、拆不干净时不重抽，免得反复递归
    (!out.to_ascii_lowercase().contains("<noscript")).then_some(out)
}

/// 正常规则（含放宽）的结果少于这么多字时，试着把隐藏内容也算上，或者拆开 `<noscript>` 重抽。
const HIDDEN_FALLBACK_CHARS: usize = 200;

/// 文章模式的结果至少要有参照结果（整页按严格规则渲染）这个比例的段落文字，否则认为找错了主体。
const ARTICLE_MIN_SHARE: f64 = 0.3;
/// 结构判断路径（页面没声明是文章、但结构像文章）用的更严阈值。
///
/// 实测依据（WebMainBench 800 页，两模式输出不同的 368 页）：
/// 收窄结果占参考 0.5–2 倍时，收窄明显更好（0.67–0.70 vs 0.56–0.57）；
/// 只占参考不到 0.5 倍时，收窄是裁过头（0.24 vs 0.50）。
/// 所以结构路径要求收窄结果不低于整页的 50%，否则回退整页。
const ARTICLE_MIN_SHARE_SHAPED: f64 = 0.5;
/// 文章模式的结果至少要有默认规则下全部文字（含链接，不算评论区）的这个比例。
const ARTICLE_MIN_DEFAULT_SHARE: f64 = 0.1;

/// 严格规则在选中的文章主体里至少保留这么多（按主体的默认规则文字算），否则按默认规则渲染主体。
const ARTICLE_MIN_STRICT_SHARE: f64 = 0.5;

/// 正文几乎为空（播放器外壳）才用 VideoObject 兜底。已经有一段短正文时保留它：
/// 侧栏嵌的视频也会声明 VideoObject，拿它整段替换会把真正的短文换掉。
const VIDEO_FALLBACK_MAX_CHARS: usize = 30;

/// `editorial::unmarked_root` 挑出的区域至少要占整页（去掉评论区）这么多文字，否则视为误挑。
const UNMARKED_MIN_SHARE: f64 = 0.2;

/// 文章模式：页面声明是文章时，只渲染文章主体，里面用严格规则（trafilatura 的非正文标记、
/// 标签白名单、链接密度）。主体有两个候选，照 trafilatura 的 compare_extraction 仲裁：
/// 默认用容器规律找到的；它为空、Readability 打分找到的长一倍以上、或者它没有成段文字时，
/// 改用 Readability 的。找不到主体、或者结果比参照少太多时返回 `None`，用默认结果。
fn render_article(
    dom: &Html<'_>,
    skipper: &mut extract::Skipper,
    base: Option<&Url>,
    shaped_only: bool,
    in_comments: &mut bool,
) -> Option<String> {
    if skipper.is_relaxed() {
        // 默认规则已经放宽过：页面上可用的文字本来就少，不再收窄
        return None;
    }
    // 两个候选和参照都只数段落文字（按严格规则遍历计数，不拼 Markdown），最后只渲染选中的那个
    let outside_template = |r: &article::Root<'_>| !r.nodes.iter().any(dom::in_template);
    let container = article::container_root(dom, skipper).filter(outside_template);
    let scored = article::scored_root(dom, skipper).filter(outside_template);
    let measure = |skipper: &mut extract::Skipper, root: &Option<article::Root<'_>>| {
        root.as_ref().map(|r| {
            let chars = r.nodes.iter().map(|n| skipper.content_chars(n)).sum();
            skipper.set_strict(true, chars);
            let wl = article::whitelist(&r.nodes, skipper);
            let headline = r.headline.as_deref().map_or(0, dom::count_non_whitespace);
            let prose = headline
                + r.nodes
                    .iter()
                    .map(|n| article::prose(n, skipper, wl))
                    .sum::<usize>();
            let para = r.nodes.iter().any(|n| article::has_paragraph(n, skipper));
            skipper.set_strict(false, 0);
            (prose, para)
        })
    };
    let bound_to_headline = article::headline_owner(dom, skipper).is_some_and(|owner| {
        container.as_ref().is_some_and(|r| {
            r.nodes
                .iter()
                .all(|n| *n == owner || n.ancestors().any(|a| a == owner))
        })
    });
    let a = measure(skipper, &container);
    let r = measure(skipper, &scored);
    let (root, got) = match (a, r) {
        (Some((la, pa)), Some((lr, _))) => {
            if la == 0 || !bound_to_headline && (lr > 2 * la || !pa && lr > la) {
                (scored?, lr)
            } else {
                (container?, la)
            }
        }
        (Some((la, _)), None) => (container?, la),
        (None, Some((lr, _))) => (scored?, lr),
        (None, None) => return None,
    };
    // 参照是"整页去掉评论、推荐这类块"：评论区很长的短文章，默认结果被评论撑大，
    // 拿它比会把正确的短正文当成删多了
    let body = dom.body();
    skipper.set_strict(true, skipper.content_chars(&body));
    let full = article::prose(&body, skipper, markdown::Whitelist::Off);
    skipper.set_strict(false, 0);
    let min_share = if shaped_only {
        ARTICLE_MIN_SHARE_SHAPED
    } else {
        ARTICLE_MIN_SHARE
    };
    if got == 0 || !bound_to_headline && (got as f64) < full as f64 * min_share {
        return None;
    }
    // 兜底：连默认规则的全部文字（含链接文字，不算评论区）的 10% 都不到，多半是把页面类型
    // 认错了：写了文章 JSON-LD 的产品页、列表页、图集，内容大多是链接卡片，文章模式会删光
    let page_text = article::text_without_comments(&body, skipper);
    let chars = root.nodes.iter().map(|n| skipper.content_chars(n)).sum();
    skipper.set_strict(true, chars);
    let article_text: usize = root
        .nodes
        .iter()
        .map(|n| article::text_without_comments(n, skipper))
        .sum();
    skipper.set_strict(false, 0);
    if !bound_to_headline && (article_text as f64) < page_text as f64 * ARTICLE_MIN_DEFAULT_SHARE {
        return None;
    }
    *in_comments = coverage::in_comments(&root.nodes, skipper, body.id());
    // 区域选对了、但严格规则（标签白名单、链接密度）在区域里删掉了一半以上的文字：多半是
    // 正文本身就是列表、表格或成串的链接（维基的列表页、文档的目录式正文），这时按默认规则
    // 渲染这个区域，只收窄范围、不再细筛（同 trafilatura：严格结果太短时退回较宽的抽取）
    if (got as f64) < article_text as f64 * ARTICLE_MIN_STRICT_SHARE {
        return render_relaxed(skipper, base, root);
    }
    render_strict(skipper, base, Some(root))
}

/// 文章主体按默认规则渲染（不加严格规则）；主体外面的标题放在最前面。
fn render_relaxed(
    skipper: &mut extract::Skipper,
    base: Option<&Url>,
    root: article::Root<'_>,
) -> Option<String> {
    let md = markdown::Renderer::new(base, skipper).render(&root.nodes);
    Some(match root.headline {
        Some(h) => format!("# {h}\n\n{md}"),
        None => md,
    })
}

/// 按严格规则渲染文章主体；主体外面的标题放在最前面。
fn render_strict(
    skipper: &mut extract::Skipper,
    base: Option<&Url>,
    root: Option<article::Root<'_>>,
) -> Option<String> {
    let root = root?;
    let chars = root.nodes.iter().map(|n| skipper.content_chars(n)).sum();
    skipper.set_strict(true, chars);
    let whitelist = article::whitelist(&root.nodes, skipper);
    let md = markdown::Renderer::new(base, skipper)
        .whitelist(whitelist)
        .render(&root.nodes);
    skipper.set_strict(false, 0);
    Some(match root.headline {
        Some(h) => format!("# {h}\n\n{md}"),
        None => md,
    })
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
    let mut href = None;
    for n in dom.root().descendants() {
        let Some(el) = n.as_element() else { continue };
        match el.name() {
            "base" => {
                href = el.attr("href");
                break;
            }
            "body" => break,
            _ => {}
        }
    }
    match (href, given) {
        (Some(h), Some(g)) => g.join(h).ok().or_else(|| Some(g.clone())),
        (Some(h), None) => Url::parse(h).ok(),
        (None, g) => g.cloned(),
    }
}
