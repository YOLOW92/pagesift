//! 文章模式：页面自己声明是一篇文章时，找出文章主体，只输出它，并在里面用更严格的跳过规则
//! （评论、标签、署名、面包屑、链接列表都去掉）。
//!
//! 默认的跳过规则是"去掉认得出的页面框架，其余保留"，适合首页、产品页这类没有单一正文块的页面；
//! 对新闻和博客文章，它会留下文章周围认不出的部分（评论区、相关文章、分享栏）。文章模式补上这一块。

use pagesift_html::{Element, Html, Tag};

use crate::dom::NodeR;
use crate::extract::Skipper;

/// 文章主体至少要有这么多非链接文字，太短的多半是摘要卡片，不是正文。
const MIN_ROOT_CHARS: usize = 400;
/// 往下收窄时，子元素装着父元素这个比例以上的非链接文字，就换成子元素。
const DESCEND_SHARE: f64 = 0.85;

// 正文容器和"不是正文"的 class/id 规律参考了 trafilatura（Apache-2.0，https://github.com/adbar/trafilatura）
// 的 xpaths.py，按元素一次匹配成特征位（见 crate::features），这里只读位。

/// 文章模式里要去掉的非正文块：只看常见的容器标签，正文段落里的 `<a>`、`<em>` 不判断。
pub(crate) fn is_furniture(el: Element<'_>, features: u32) -> bool {
    if !matches!(
        el.tag(),
        Tag::Div
            | Tag::Section
            | Tag::Span
            | Tag::P
            | Tag::Ul
            | Tag::Ol
            | Tag::Li
            | Tag::Aside
            | Tag::Figure
    ) && el.tag() != Tag::Figcaption
    {
        return false;
    }
    if el.tag() == Tag::Figcaption {
        return true;
    }
    features & crate::features::DISCARD != 0
}

/// 按当前跳过规则数子树里会输出的、不在链接里的文字（不拼 Markdown，只遍历计数）。
/// 和渲染器的口径一致：白名单模式下，行内文字只在它所属的块级容器被允许时计入
/// （标题、列表项、单元格总是允许）。
pub(crate) fn prose(node: &NodeR<'_>, skipper: &Skipper, wl: crate::markdown::Whitelist) -> usize {
    use crate::markdown::Whitelist;
    let allowed = |n: &NodeR<'_>| match wl {
        Whitelist::Off => true,
        Whitelist::On { div } => match n.tag() {
            Some(
                Tag::P
                | Tag::Blockquote
                | Tag::Li
                | Tag::Dd
                | Tag::Dt
                | Tag::Td
                | Tag::Th
                | Tag::Pre
                | Tag::Code
                | Tag::H1
                | Tag::H2
                | Tag::H3
                | Tag::H4
                | Tag::H5
                | Tag::H6,
            ) => true,
            Some(Tag::Div) => div || crate::dom::is_heading(n),
            _ => false,
        },
    };
    fn walk(
        n: &NodeR<'_>,
        skipper: &Skipper,
        in_link: bool,
        keep: bool,
        allowed: &dyn Fn(&NodeR<'_>) -> bool,
    ) -> usize {
        let mut total = 0;
        for c in n.children() {
            if let Some(t) = c.as_text() {
                if keep && !in_link {
                    total += crate::dom::count_non_whitespace(t);
                }
            } else if c.is_element() && !skipper.skip(&c) {
                // 块级元素开始自己的段落；行内元素沿用所在段落的去留
                let block = c.tag().is_some_and(crate::markdown::is_block_tag)
                    || skipper.has_block_descendant(&c);
                let k = if block { allowed(&c) } else { keep };
                total += walk(&c, skipper, in_link || c.tag() == Some(Tag::A), k, allowed);
            }
        }
        total
    }
    if node.is_element() && skipper.skip(node) {
        return 0;
    }
    walk(node, skipper, false, allowed(node), &allowed)
}

/// 按当前跳过规则数子树里的全部可见文字（含链接文字），不算评论区：
/// 评论很长的文章，分母不能被评论撑大。
pub(crate) fn text_without_comments(node: &NodeR<'_>, skipper: &Skipper) -> usize {
    let mut total = 0;
    for c in node.children() {
        if let Some(t) = c.as_text() {
            total += crate::dom::count_non_whitespace(t);
        } else if c.is_element() && !skipper.skip(&c) && !skipper.is_comments(&c) {
            total += text_without_comments(&c, skipper);
        }
    }
    total
}

/// 有没有成段的文字：某个没被跳过的 `<p>` 里至少有 80 个非链接字符。
pub(crate) fn has_paragraph(node: &NodeR<'_>, skipper: &Skipper) -> bool {
    node.descendants()
        .any(|d| d.tag() == Some(Tag::P) && skipper.content_chars(&d) >= 80 && !skipper.skip(&d))
}

/// 文章模式里整个去掉的标签（trafilatura 的 MANUALLY_CLEANED 里 pagesift 默认还保留的那些）：
/// 题图和图注、表单和按钮、时间戳。
pub(crate) fn is_cleaned_tag(tag: Tag) -> bool {
    matches!(
        tag,
        Tag::Figure
            | Tag::Form
            | Tag::Button
            | Tag::Label
            | Tag::Legend
            | Tag::Fieldset
            | Tag::Time
            | Tag::Select
            | Tag::Textarea
            | Tag::Input
            | Tag::Dialog
            | Tag::Menu
    )
}

/// trafilatura 的 link_density_test：链接多的块是框架（推荐、标签、"阅读更多"）。
/// 只看 `div`、列表、`p`；带图片的块不删（相册、卡片）。
pub(crate) fn link_dense(node: &NodeR<'_>, skipper: &Skipper) -> bool {
    let Some(tag) = node.tag() else { return false };
    if !matches!(tag, Tag::Div | Tag::Ul | Tag::Ol | Tag::P | Tag::Dl) {
        return false;
    }
    // 链接统计在构造 Skipper 时已经一次算好，这里不再遍历子树
    let (chars, linklen, links) = skipper.stats(node);
    let (text_links, short, images) = skipper.link_stats(node);
    if links == 0 || images > 0 {
        return false;
    }
    let last = {
        let mut sib = node.next_sibling();
        loop {
            match sib {
                None => break true,
                Some(s) if s.is_element() => break false,
                Some(s) => sib = s.next_sibling(),
            }
        }
    };
    if links == 1 && linklen > 100 && linklen as f64 > chars as f64 * 0.9 {
        return true;
    }
    let limit = match (tag, last) {
        (Tag::P, true) => 60,
        (Tag::P, false) => 30,
        (_, true) => 300,
        _ => 100,
    };
    if chars < limit {
        if text_links == 0 {
            return true;
        }
        return linklen as f64 > chars as f64 * 0.8
            || (text_links > 1 && short as f64 / text_links as f64 > 0.8);
    }
    // 大块的"最新消息"链接墙；平均链接文字 100 字以上的是目录、卡片，不算
    links > 4 && linklen as f64 > chars as f64 * 0.9 && linklen < 100 * text_links
}

/// 文章主体：要渲染的节点（按文档顺序），和主体外面的标题（离得太远、没有并进来时）。
pub(crate) struct Root<'a> {
    pub nodes: Vec<NodeR<'a>>,
    pub headline: Option<String>,
}

/// 容器规律找到的主体（[`find_root`]），带上标题区。
pub(crate) fn container_root<'a>(dom: &'a Html<'a>, skipper: &Skipper) -> Option<Root<'a>> {
    let (root, teaser) = find_root(dom, skipper)?;
    // 标题所在的 <article> 只是导语时，正文在它外面：标题从导语那里拿
    let (root, headline) = match teaser {
        Some(t) => (root, headline_text(&t)),
        None => with_headline(dom, root, skipper),
    };
    Some(Root {
        nodes: vec![root],
        headline,
    })
}

/// 导语里的标题文字（`<h1>`，没有时 `<h2>`）。
fn headline_text(teaser: &NodeR<'_>) -> Option<String> {
    let h = teaser
        .descendants()
        .find(|n| n.tag() == Some(Tag::H1))
        .or_else(|| teaser.descendants().find(|n| n.tag() == Some(Tag::H2)))?;
    let title = h.text().split_whitespace().collect::<Vec<_>>().join(" ");
    (!title.is_empty()).then_some(title)
}

/// Readability 打分找到的主体：正文外面的 `<h1>` 离得近时扩到同时包含标题和正文的最小容器
/// （导语、署名、日期一般在这里），离得远时把标题单独放在前面。
pub(crate) fn scored_root<'a>(dom: &'a Html<'a>, skipper: &Skipper) -> Option<Root<'a>> {
    let mut nodes = crate::readability::content_nodes(dom, skipper)?;
    // 打分也可能落在两栏的公共容器上：和容器规律一样按标题收窄
    if let [only] = nodes.as_mut_slice() {
        *only = narrow_to_headline(dom, *only, skipper);
    }
    let first = *nodes.first()?;
    if nodes
        .iter()
        .any(|n| n.descendants().any(|d| d.tag() == Some(Tag::H1)))
    {
        return Some(Root {
            nodes,
            headline: None,
        });
    }
    match with_headline(dom, first, skipper) {
        // 扩到标题和正文的公共容器时，这个容器也可能连着侧栏（打分选中的本来就是侧栏的卡片列表）
        (region, None) if region.id() != first.id() => Some(Root {
            nodes: vec![narrow_to_headline(dom, region, skipper)],
            headline: None,
        }),
        (_, headline) => Some(Root { nodes, headline }),
    }
}

/// trafilatura 的规则：正文范围里 `<p>` 的文字够多时只输出白名单元素；不够时把 `div` 也算上
/// （页面不用 `<p>` 排版）。
pub(crate) fn whitelist(roots: &[NodeR<'_>], skipper: &Skipper) -> crate::markdown::Whitelist {
    use crate::markdown::Whitelist;
    let p_chars: usize = roots
        .iter()
        .flat_map(|r| r.descendants())
        .filter(|n| n.tag() == Some(Tag::P) && !skipper.skip(n))
        .map(|n| skipper.visible_chars(&n))
        .sum();
    Whitelist::On {
        div: p_chars < WHITELIST_MIN_P_CHARS,
    }
}

/// trafilatura 的 min_extracted_size（250）× 3。
const WHITELIST_MIN_P_CHARS: usize = 750;

fn container(e: Element<'_>) -> bool {
    matches!(e.tag(), Tag::Article | Tag::Div | Tag::Main | Tag::Section)
}

/// 标题和主体的公共祖先比主体多出的非链接文字不超过主体的这个比例（再加上 [`HEADER_EXTRA_CHARS`]）时，
/// 把范围扩到公共祖先：标题、导语、日期一般就在主体上方的同一个容器里。
const HEADER_EXTRA_SHARE: f64 = 1.0;
const HEADER_EXTRA_CHARS: usize = 2000;

/// 页面没有可见 `<h1>` 时，用文档标题补一个。标题过长（导航堆叠、站点拼接）时不补。
fn document_title(dom: &Html<'_>, skipper: &Skipper) -> Option<String> {
    let has_visible_h1 = dom.root().descendants().any(|n| {
        n.tag() == Some(Tag::H1) && !skipper.skip(&n) && !n.ancestors().any(|a| skipper.skip(&a))
    });
    if has_visible_h1 {
        return None;
    }
    let title = dom
        .root()
        .descendants()
        .find(|n| n.tag() == Some(Tag::Title))?
        .text();
    let title = title.split_whitespace().collect::<Vec<_>>().join(" ");
    (!title.is_empty() && title.chars().count() <= 200).then_some(title)
}

/// 文章主体加上它的标题：主体外面的 `<h1>` 离得近时扩到两者的公共祖先，离得远时返回标题文字，
/// 由调用方放在最前面。
fn with_headline<'a>(
    dom: &'a Html<'a>,
    root: NodeR<'a>,
    skipper: &Skipper,
) -> (NodeR<'a>, Option<String>) {
    let inside = root.descendants().any(|d| d.tag() == Some(Tag::H1));
    if inside {
        return (root, None);
    }
    // A headline in another article is not this article's title. In particular,
    // recommendation grids may use H1 even when the main story uses H2.
    let scope = std::iter::once(root)
        .chain(root.ancestors())
        .find(|n| n.tag() == Some(Tag::Article));
    let headline_scope = scope.unwrap_or_else(|| dom.root());
    // 不在 <article> 里时先找和文档标题一致的那个：页面唯一的 <h1> 常常是栏目名（"Asiya"、
    // "News"），文章标题是 <h2>；拿栏目名当标题会把范围扩到栏目和正文的公共容器，连侧栏一起
    let agreeing = scope
        .is_none()
        .then(|| page_headline(dom, dom.root(), skipper))
        .flatten()
        .filter(|h| !h.ancestors().any(|a| a.tag() == Some(Tag::Article)));
    let Some(h1) = agreeing
        .or_else(|| {
            headline_scope.descendants().find(|n| {
                let title = n.tag() == Some(Tag::H1)
                    || (scope.is_some()
                        && n.tag() == Some(Tag::H2)
                        && (n.parent().is_some_and(|p| p.tag() == Some(Tag::Header))
                            || n.as_element()
                                .is_some_and(|e| e.attr("itemprop") == Some("headline"))));
                title
                    && !skipper.skip(n)
                    && !n.ancestors().any(|a| skipper.skip(&a))
                    && (scope.is_some() || !n.ancestors().any(|a| a.tag() == Some(Tag::Article)))
            })
        })
        .or_else(|| {
            scope?;
            let title = dom
                .root()
                .descendants()
                .find(|n| n.tag() == Some(Tag::Title))?
                .text();
            let signature = TitleSignature::new(&title);
            dom.root().descendants().find(|n| {
                n.tag() == Some(Tag::H1)
                    && !skipper.skip(n)
                    && !n
                        .ancestors()
                        .any(|a| skipper.skip(&a) || a.tag() == Some(Tag::Article))
                    && title_agrees_with(&signature, &n.text())
            })
        })
    else {
        // 页面上唯一可见的标题可能在另一个被裁掉的预览块里（食谱站的 quickview 就是）。
        // 标题是正文的一部分：没有可见 <h1> 时退回文档标题，别让主体孤零零地没有标题。
        return (root, document_title(dom, skipper));
    };
    if scope.is_some_and(|owner| !h1.ancestors().any(|a| a == owner)) {
        // Keep a document-level title without expanding the content into its
        // surrounding layout and recommendations.
        return (
            root,
            Some(h1.text().split_whitespace().collect::<Vec<_>>().join(" ")),
        );
    }
    let root_ancestors: Vec<_> = root.ancestors().map(|a| a.id()).collect();
    let common = h1.ancestors().find(|a| root_ancestors.contains(&a.id()));
    let base = skipper.content_chars(&root);
    if let Some(c) = common
        && skipper.content_chars(&c) as f64
            <= base as f64 * (1.0 + HEADER_EXTRA_SHARE) + HEADER_EXTRA_CHARS as f64
    {
        return (c, None);
    }
    let title = h1.text().split_whitespace().collect::<Vec<_>>().join(" ");
    (root, (!title.is_empty()).then_some(title))
}

/// Lowercase/alphanumeric form of a page title. The same title is compared
/// against every candidate heading on the page, so building the normalized
/// form and word list once avoids a lowercase allocation plus a word-set
/// allocation for each heading.
pub(crate) struct TitleSignature {
    normalized: String,
    words: Vec<String>,
}

impl TitleSignature {
    pub(crate) fn new(title: &str) -> Self {
        let lower = title.to_lowercase();
        let normalized = lower
            .chars()
            .filter(|c| c.is_alphanumeric())
            .collect::<String>();
        let words = lower
            .split(|c: char| !c.is_alphanumeric())
            .filter(|word| word.chars().count() > 2)
            .map(str::to_owned)
            .collect();
        Self { normalized, words }
    }
}

pub(crate) fn title_agrees_with(signature: &TitleSignature, heading: &str) -> bool {
    if heading.is_ascii() {
        return title_agrees_ascii(signature, heading);
    }
    title_agrees_allocating(signature, heading)
}

/// ASCII fast path: normalize the heading into a stack buffer instead of
/// allocating a lowercase `String` plus a second normalized `String` for every
/// candidate heading on the page.
fn title_agrees_ascii(signature: &TitleSignature, heading: &str) -> bool {
    let mut needle = [0u8; 128];
    let mut len = 0usize;
    for byte in heading.bytes() {
        let byte = byte.to_ascii_lowercase();
        if !byte.is_ascii_alphanumeric() {
            continue;
        }
        if len == needle.len() {
            return title_agrees_allocating(signature, heading);
        }
        needle[len] = byte;
        len += 1;
    }
    if len >= 8 {
        let needle = std::str::from_utf8(&needle[..len]).expect("ASCII buffer");
        if signature.normalized.contains(needle) {
            return true;
        }
    }
    // Deduplicate heading words the way the old HashSet did, without hashing
    // or per-word allocation: headings have only a handful of words.
    let mut heading_words = [""; 16];
    let mut count = 0usize;
    for word in heading.split(|c: char| !c.is_ascii_alphanumeric()) {
        if word.len() <= 2 {
            continue;
        }
        if heading_words[..count]
            .iter()
            .any(|existing| existing.eq_ignore_ascii_case(word))
        {
            continue;
        }
        if count == heading_words.len() {
            break;
        }
        heading_words[count] = word;
        count += 1;
    }
    if count < 2 {
        return false;
    }
    let matches = heading_words[..count]
        .iter()
        .filter(|word| {
            signature
                .words
                .iter()
                .any(|title_word| title_word.eq_ignore_ascii_case(word))
        })
        .count();
    matches * 5 >= count * 4
}

fn title_agrees_allocating(signature: &TitleSignature, heading: &str) -> bool {
    let heading = heading.to_lowercase();
    let normalized = heading
        .chars()
        .filter(|c| c.is_alphanumeric())
        .collect::<String>();
    let normalized_len = if normalized.is_ascii() {
        normalized.len()
    } else {
        normalized.chars().count()
    };
    if normalized_len >= 8 && signature.normalized.contains(&normalized) {
        return true;
    }
    // Deduplicate heading words the way the old HashSet did, but without
    // hashing or per-word allocation: headings have only a handful of words.
    let mut heading_words: Vec<&str> = Vec::new();
    for word in heading.split(|c: char| !c.is_alphanumeric()) {
        if word.chars().count() > 2 && !heading_words.contains(&word) {
            heading_words.push(word);
        }
    }
    if heading_words.len() < 2 {
        return false;
    }
    let matches = heading_words
        .iter()
        .filter(|word| signature.words.iter().any(|title_word| title_word == *word))
        .count();
    matches * 5 >= heading_words.len() * 4
}

pub(crate) fn title_agrees(title: &str, heading: &str) -> bool {
    title_agrees_with(&TitleSignature::new(title), heading)
}

/// Bind a document headline to its owning article, rather than choosing the
/// largest article-shaped recommendation aggregate. Ambiguous owners abstain.
pub(crate) fn headline_owner<'a>(dom: &'a Html<'a>, skipper: &Skipper) -> Option<NodeR<'a>> {
    let title = dom
        .root()
        .descendants()
        .find(|n| n.tag() == Some(Tag::Title))?
        .text()
        .to_lowercase();
    let signature = TitleSignature::new(&title);
    let mut owner = None;
    for h in dom
        .root()
        .descendants()
        .filter(|n| matches!(n.tag(), Some(Tag::H1 | Tag::H2)))
    {
        if skipper.skip(&h) || h.ancestors().any(|a| skipper.skip(&a)) {
            continue;
        }
        if !title_agrees_with(&signature, &h.text()) {
            continue;
        }
        let Some(article) = h.ancestors().find(|a| a.tag() == Some(Tag::Article)) else {
            continue;
        };
        if skipper.content_chars(&article) < MIN_ROOT_CHARS || !has_paragraph(&article, skipper) {
            continue;
        }
        if owner.is_some_and(|old| old != article) {
            return None;
        }
        owner = Some(article);
    }
    owner
}

/// 找文章主体。信号从强到弱：`itemprop="articleBody"`、文字最多的 `<article>`、常见的正文类名；
/// 都没有时从 `<main>`（或 body）往下，收窄到装着大部分文字的最小容器。
/// 标题所在的 `<article>` 只是导语时（见 [`teaser_body`]），返回正文块和那个导语。
fn find_root<'a>(dom: &'a Html<'a>, skipper: &Skipper) -> Option<(NodeR<'a>, Option<NodeR<'a>>)> {
    if let Some(owner) = headline_owner(dom, skipper) {
        return Some(match teaser_body(dom, owner, skipper) {
            Some(body) => (body, Some(owner)),
            None => (owner, None),
        });
    }
    // 一次遍历收集每种信号下文字最多的候选：itemprop、第 0 层、<article>、第 1–2 层、<main>
    const ITEMPROP: usize = 0;
    const ARTICLE: usize = 2;
    let mut best: [Option<(usize, NodeR<'a>)>; 5] = [None; 5];
    let mut consider = |slot: usize, n: NodeR<'a>, chars: usize| {
        if best[slot].is_none_or(|(c, _)| chars > c) {
            best[slot] = Some((chars, n));
        }
    };
    let mut main = None;
    for n in dom.root().descendants() {
        let Some(e) = n.as_element() else { continue };
        if main.is_none() && (e.tag() == Tag::Main || e.attr("role") == Some("main")) {
            main = Some(n);
        }
        let itemprop = e.attr("itemprop").is_some_and(|v| {
            v.split_whitespace()
                .any(|w| w.eq_ignore_ascii_case("articleBody"))
        });
        let is_article = e.tag() == Tag::Article;
        let f = if container(e) {
            skipper.features(&n)
        } else {
            0
        };
        let tiers =
            f & (crate::features::TIER[0] | crate::features::TIER[1] | crate::features::TIER[2]);
        if !itemprop && !is_article && tiers == 0 {
            continue;
        }
        if skipper.skip(&n) {
            continue;
        }
        let chars = skipper.content_chars(&n);
        if itemprop {
            consider(ITEMPROP, n, chars);
        }
        if is_article {
            consider(ARTICLE, n, chars);
        }
        for (slot, bit) in [
            (1, crate::features::TIER[0]),
            (3, crate::features::TIER[1]),
            (4, crate::features::TIER[2]),
        ] {
            if tiers & bit != 0 {
                consider(slot, n, chars);
            }
        }
    }
    // 按信号强弱的顺序取第一个够长的。`itemprop="articleBody"` 是页面自己标的正文，原样用；
    // 其余的（类名、<article>、往下收窄）可能连侧栏一起框进来，按标题再收一次
    for slot in [ITEMPROP, 1, ARTICLE, 3, 4] {
        if let Some((chars, n)) = best[slot]
            && chars >= MIN_ROOT_CHARS
        {
            return Some(match slot {
                ITEMPROP => (n, None),
                ARTICLE => (narrow_to_headline(dom, descend(n, skipper), skipper), None),
                _ => (narrow_to_headline(dom, n, skipper), None),
            });
        }
    }
    let root = descend(main.unwrap_or_else(|| dom.body()), skipper);
    (skipper.content_chars(&root) >= MIN_ROOT_CHARS)
        .then(|| (narrow_to_headline(dom, root, skipper), None))
}

/// 按标题收窄时，标题所在的容器至少要装着主体这个比例的正文。
const NARROW_SHARE: f64 = 0.8;
/// 卡片摘要最多这么多字：别的文章的一两句导语。
const CARD_SUMMARY_MAX_CHARS: usize = 400;
/// 往上找卡片标题链接时最多看几层。
const CARD_DEPTH: usize = 3;

/// 主体比正文宽时按页面标题收窄：从和 `<title>` 一致的那个 `<h1>`/`<h2>` 往上，找到装着主体
/// [`NARROW_SHARE`] 以上正文的最小容器。主体是由类名或"装着大部分文字"框出来的，碰上两栏版式
/// （正文一栏、新闻列表一栏）常常停在两栏的公共容器上；标题所在的那一栏才是文章。
/// 正文按非链接文字算，但不算卡片摘要（[`card_summaries`]）：侧栏新闻卡片的摘要是非链接文字，
/// 却是别的文章。
fn narrow_to_headline<'a>(dom: &'a Html<'a>, root: NodeR<'a>, skipper: &Skipper) -> NodeR<'a> {
    let Some(headline) = page_headline(dom, root, skipper) else {
        return root;
    };
    let cards = card_summaries(root, skipper);
    let prose = |n: &NodeR<'_>| {
        let cards: usize = cards
            .iter()
            .filter(|(p, _)| p == n || p.ancestors().any(|a| a == *n))
            .map(|(_, c)| c)
            .sum();
        skipper.content_chars(n).saturating_sub(cards)
    };
    let total = prose(&root);
    for a in headline.ancestors() {
        if a == root {
            break;
        }
        let chars = prose(&a);
        if chars >= MIN_ROOT_CHARS && chars as f64 >= total as f64 * NARROW_SHARE {
            return a;
        }
    }
    root
}

/// 当作页面标题的 `<h1>`/`<h2>` 至少要有文档标题这个比例的长度（字母、数字、汉字）：
/// `<title>` 常把小节名一起列出来（"Uses, Side Effects, Interactions"），小节标题
/// "Side Effects" 也"和标题一致"，但它不是文章标题。
const HEADLINE_MIN_TITLE_SHARE: f64 = 0.4;

/// `root` 里和文档标题一致、长度够的第一个可见 `<h1>` 或 `<h2>`。
fn page_headline<'a>(dom: &'a Html<'a>, root: NodeR<'a>, skipper: &Skipper) -> Option<NodeR<'a>> {
    let title = dom
        .root()
        .descendants()
        .find(|n| n.tag() == Some(Tag::Title))?
        .text()
        .to_lowercase();
    let len = |s: &str| s.chars().filter(|c| c.is_alphanumeric()).count();
    let min_len = (len(&title) as f64 * HEADLINE_MIN_TITLE_SHARE).ceil() as usize;
    let signature = TitleSignature::new(&title);
    root.descendants().find(|n| {
        matches!(n.tag(), Some(Tag::H1 | Tag::H2))
            && !skipper.skip(n)
            && {
                let text = n.text();
                len(&text) >= min_len && title_agrees_with(&signature, &text)
            }
            && !n
                .ancestors()
                .take_while(|a| *a != root)
                .any(|a| skipper.skip(&a))
    })
}

/// 子树里新闻卡片摘要（[`card_summaries`]）的非链接文字数。
pub(crate) fn card_chars(root: NodeR<'_>, skipper: &Skipper) -> usize {
    card_summaries(root, skipper).iter().map(|(_, c)| c).sum()
}

/// 新闻卡片的摘要：以省略号结尾的短段落，旁边（往上 [`CARD_DEPTH`] 层以内）有指向别处的链接
/// （卡片的标题或图片）。CMS 自动截出来的摘要都是这样，正文段落很少以省略号结尾。
fn card_summaries<'a>(root: NodeR<'a>, skipper: &Skipper) -> Vec<(NodeR<'a>, usize)> {
    root.descendants()
        .filter(|n| n.tag() == Some(Tag::P))
        .filter_map(|p| {
            let chars = skipper.content_chars(&p);
            if chars == 0 || chars > CARD_SUMMARY_MAX_CHARS {
                return None;
            }
            let text = p.text();
            let text = text.trim_end();
            if !(text.ends_with("...") || text.ends_with('…')) {
                return None;
            }
            let linked = p.ancestors().take(CARD_DEPTH).any(|a| {
                a.descendants().any(|d| {
                    d.tag() == Some(Tag::A)
                        && !d.ancestors().any(|x| x == p)
                        && d.as_element().is_some_and(|e| e.attr("href").is_some())
                })
            });
            linked.then_some((p, chars))
        })
        .collect()
}

/// 导语最多这么多非链接文字：一两段摘要。再长的 `<article>` 不当导语看。
const TEASER_MAX_CHARS: usize = 3 * MIN_ROOT_CHARS;
/// 拿正文段落的前这么多个字（字母、数字、汉字，小写）去导语里找。
const TEASER_KEY_CHARS: usize = 60;

/// 标题所在的 `<article>` 只是导语：它的文字是从页面别处一段正文里摘的（那段的开头在导语里
/// 原样出现），而那段所在的块比导语长一倍以上。新闻稿、通讯社稿件的模板常把标题和摘要单独
/// 包成一个 `<article>`，正文放在它外面；只取这个 `<article>` 就只有一段以 "..." 结尾的摘要。
/// 返回装着那段正文、又不包含导语的最小的够长容器。
fn teaser_body<'a>(dom: &'a Html<'a>, owner: NodeR<'a>, skipper: &Skipper) -> Option<NodeR<'a>> {
    let own = skipper.content_chars(&owner);
    if own > TEASER_MAX_CHARS {
        return None;
    }
    let key = |n: &NodeR<'_>| -> String {
        n.text()
            .chars()
            .filter(|c| c.is_alphanumeric())
            .flat_map(char::to_lowercase)
            .collect()
    };
    let teaser = key(&owner);
    for p in dom.root().descendants() {
        if p.tag() != Some(Tag::P) || p.ancestors().any(|a| a == owner) {
            continue;
        }
        let text = key(&p);
        let Some((cut, _)) = text.char_indices().nth(TEASER_KEY_CHARS) else {
            continue;
        };
        if !teaser.contains(&text[..cut])
            || skipper.skip(&p)
            || p.ancestors().any(|a| skipper.skip(&a))
        {
            continue;
        }
        for a in p.ancestors() {
            if owner.ancestors().any(|o| o == a) {
                break;
            }
            let chars = skipper.content_chars(&a);
            if chars >= 2 * own && chars >= MIN_ROOT_CHARS {
                return Some(a);
            }
        }
    }
    None
}

/// 往下收窄：只要某个子元素装着这一层大部分的非链接文字，就进到子元素里。
fn descend<'a>(mut node: NodeR<'a>, skipper: &Skipper) -> NodeR<'a> {
    loop {
        let total = skipper.content_chars(&node);
        let next = node
            .children()
            .filter(|c| c.is_element() && !skipper.skip(c))
            .max_by_key(|c| skipper.content_chars(c));
        match next {
            Some(c)
                if skipper.content_chars(&c) as f64 >= total as f64 * DESCEND_SHARE
                    && skipper.content_chars(&c) >= MIN_ROOT_CHARS =>
            {
                node = c
            }
            _ => return node,
        }
    }
}

/// 页面是否**明显不是**一篇文章：链接墙、列表/卡片主导、几乎没有散文段落。
///
/// 只做"排除"，不做"确认"。原因是实测（WebMainBench 800 页 + WCXB 2,006 页）：
/// - 强行给所有页面收窄：WebMainBench 0.6406、WCXB 0.6672
/// - 要求结构"像文章"才收窄：WebMainBench 0.6013（掉 0.039）、WCXB 0.6744（只涨 0.007）
///
/// 也就是说"确认像文章"这个判据误杀太多真文章页，得不偿失。
/// 这里改成只排除结构上明确是列表/卡片/链接墙的页面，其余一律照常收窄。
pub(crate) fn clearly_not_article(dom: &Html<'_>, skipper: &Skipper) -> bool {
    let body = dom.body();
    let (total, link_chars, _) = skipper.stats(&body);
    if total < 400 {
        // 文字太少，谈不上是文章；交给后续兜底
        return false;
    }
    // 链接文字占七成以上：导航页、目录页、链接墙
    if link_chars * 100 >= total * 70 {
        return true;
    }
    let mut prose_chars = 0usize;
    let mut structured = 0usize;
    for n in body.descendants() {
        if skipper.skip(&n) {
            continue;
        }
        match n.tag() {
            Some(Tag::P) => prose_chars += skipper.content_chars(&n),
            Some(Tag::Li | Tag::Td | Tag::Th) => structured += skipper.content_chars(&n),
            _ => {}
        }
    }
    // 列表项与表格单元格的文字是散文的 4 倍以上：商品列表、聚合页、数据表页
    structured >= prose_chars.max(1) * 4
}

#[cfg(test)]
mod title_tests {
    use super::{TitleSignature, title_agrees, title_agrees_allocating, title_agrees_ascii};

    #[test]
    fn title_agrees_matches_normalized_containment() {
        assert!(title_agrees(
            "Global Market Tech Breakthrough News Item #1",
            "Global Market Tech Breakthrough News Item #1"
        ));
    }

    #[test]
    fn title_agrees_counts_unique_heading_words() {
        // Duplicate heading words must collapse exactly like the old HashSet.
        assert!(title_agrees("alpha beta gamma", "alpha alpha alpha gamma"));
        assert!(!title_agrees("alpha beta gamma", "alpha alpha alpha delta"));
    }

    #[test]
    fn title_agrees_rejects_unrelated_heading() {
        assert!(!title_agrees(
            "Global Market News",
            "Completely Different Topic"
        ));
    }

    #[test]
    fn ascii_fast_path_matches_allocating_path() {
        let titles = [
            "Global Market Tech Breakthrough News Item #1",
            "alpha beta gamma",
            "A",
            "Short",
            "Café résumé naïve",
        ];
        let headings = [
            "Global Market Tech Breakthrough News Item #1",
            "GLOBAL market tech",
            "alpha alpha alpha gamma",
            "alpha beta delta",
            "a",
            "ab cd",
            "Café résumé",
            "naïve café",
            "",
        ];
        for title in titles {
            let signature = TitleSignature::new(title);
            for heading in headings {
                if !heading.is_ascii() {
                    continue;
                }
                assert_eq!(
                    title_agrees_ascii(&signature, heading),
                    title_agrees_allocating(&signature, heading),
                    "title={title:?} heading={heading:?}"
                );
            }
        }
    }

    #[test]
    fn non_ascii_heading_still_uses_the_allocating_path() {
        assert!(title_agrees("Café résumé naïve", "café résumé naïve"));
        assert!(!title_agrees("Café résumé naïve", "完全不同的标题"));
    }
}
