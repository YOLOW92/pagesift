//! 带查询的整站爬取：相关性优先的调度队列（best-first）、链接打分和自适应停止。
//!
//! 链接分数 = 锚文本、URL（主机名和路径）、链接所在那一行各自覆盖了多少查询词（加权），
//! 再加上所在页面本身的相关度，最后按深度衰减。查询词的权重按已抓页面的文档频率调整
//! （[`weights`]）：每页都有的词不再算数，少见的词才是证据。家具页（新闻、关于、法律页脚、
//! 带日期的路径）和枢纽页降权（[`furniture`]）；队列出队时按簇分散（[`queue`]），
//! 一个子树吃不完预算；自适应停止只拿相关页面当证据（[`adaptive`]）。
//! 分数相同的链接按发现顺序出队，所以完全不相关的站点上，顺序退化成广度优先。

mod adaptive;
mod context;
mod facet;
pub(crate) mod furniture;
mod harvest;
mod queue;
mod weights;

pub(crate) use adaptive::{Adaptive, MIN_RELEVANCE};
pub(crate) use context::link_contexts;
use context::prose_words;
pub(crate) use harvest::{Harvest, subtree};
pub(crate) use queue::{BestFirst, Offer, cluster_key};

use facet::{FacetStats, Facets};
use pagesift_extract::Query;
use pagesift_net::Url;
use weights::TermWeights;

const W_ANCHOR: f32 = 2.0;
const W_LOCATOR: f32 = 1.5;
const W_CONTEXT: f32 = 1.0;
const W_PARENT: f32 = 0.5;
/// 锚文本命中两个以上查询词（"international students"、"tuition fees"）的加分。
const PHRASE_BONUS: f32 = 0.5;
/// 每深一层，分数乘以它。0.8 时首页一级的门户页压过两级下的内容页，0.9 留给锚文本做主。
const DEPTH_DECAY: f32 = 0.9;
/// 英文查询在非英文站点上："English" 入口本身不含查询词，但英文内容都在它后面；给它一个保底分。
const LANGUAGE_PORTAL_SCORE: f32 = 0.6;
/// 栏目入口（`/docs/`、`/services/`、`/admissions/` 这类一层路径、短锚文本的链接）的保底分：
/// 锚文本本身常常不含查询词，内容却都在它下面。定义成"一条锚文本命中半个查询的家具链接"的分数：
/// 栏目先于新闻、博客探，但不先于锚文本真命中查询词的普通链接（那是 W_ANCHOR × 覆盖度 ≥ 0.4）。
const SECTION_PRIOR: f32 = furniture::FURNITURE_FACTOR * (W_ANCHOR * 0.5 + PHRASE_BONUS);
/// 链接的分数不高于栏目保底分：它本身没有查询词的线索，抓它是"盲探"（自适应停止用，
/// 见 [`Adaptive::observe`]）。种子页（分数无穷大）不算。
pub(crate) fn is_blind(score: f32) -> bool {
    score <= SECTION_PRIOR
}

/// 别的主机的根（院系、图书馆、系统的子站首页）的保底分：是另一个站，排在本站的栏目之后。
const SUBSITE_PRIOR: f32 = SECTION_PRIOR * 0.5;
/// 英文查询碰到别的语言版本的入口（`/de/`、`/ja/`）：和家具页一样打折。
const FOREIGN_LANGUAGE_FACTOR: f32 = furniture::FURNITURE_FACTOR;
/// 同一页上指向同一个簇的链接超过这个数（新闻列表、展品列表），每条按 sqrt(上限/条数) 打折。
pub(crate) const LIST_FREE: usize = 8;

/// 词权重变化超过这么多（最大的那个词）才把排队的链接重新打分：权重随抓到的页面渐渐稳定，
/// 后期几乎不用重算；队列长度不超过 limit，一次重算很便宜。
const RESCORE_DELTA: f32 = 0.05;

/// 页面相关度的组成：各查询方面的证据（[`facet`]），枢纽页和家具页的折扣。
/// `fixed` 是直接给定的相关度（没有页面可看的链接，测试）。
#[derive(Clone, Copy, Debug, Default)]
pub(crate) struct PageSignal {
    title: u64,
    body: u64,
    facets: Facets,
    factor: f32,
    fixed: f32,
    /// 页面声明的语言不是英文（`lang` 不以 `en` 开头）。
    foreign: bool,
}

impl PageSignal {
    pub(crate) fn fixed(relevance: f32) -> Self {
        Self {
            fixed: relevance,
            ..Self::default()
        }
    }
}

/// URL 的形状对分数的作用：打折（家具、日期、别的语言），保底（栏目、子站、英文入口），
/// 或者直接给定（种子页、hreflang 声明的英文版）。
#[derive(Clone, Copy, Debug)]
enum Shape {
    Plain,
    Factor(f32),
    Floor(f32),
    Fixed(f32),
}

/// 链接分数的组成（见 [`Focus::link_signal`]）。
#[derive(Clone, Copy, Debug)]
pub(crate) struct LinkSignal {
    anchor: u64,
    locator: u64,
    context: u64,
    parent: PageSignal,
    shape: Shape,
    depth: u8,
    /// 调用方另乘的折扣（模板降级、同一页上的长列表）。
    factor: f32,
}

impl LinkSignal {
    /// 直接给定分数的链接：重新打分时不变。
    pub(crate) fn fixed(score: f32) -> Self {
        Self {
            anchor: 0,
            locator: 0,
            context: 0,
            parent: PageSignal::default(),
            shape: Shape::Fixed(score),
            depth: 0,
            factor: 1.0,
        }
    }

    /// 另乘一个折扣。
    pub(crate) fn scale(&mut self, factor: f32) {
        self.factor *= factor;
    }
}

/// 查询和从它派生的打分规则。
pub(crate) struct Focus {
    pub query: Query,
    weights: TermWeights,
    /// 页面相关度用的本站统计量。
    facets: FacetStats,
    /// 种子页的主机（去掉 www.）：英文版入口的保底分只给本站的 `/en/`，别的主机的 `/en/` 是子站。
    host: Option<String>,
    /// 已经抓到本站的英文版首页：之后别的 `/xx/en/` 不再拿英文入口的保底分（那是子栏目的英文版）。
    english_home: bool,
    /// 排队链接上一次打分时的词权重（见 [`Self::weights_moved`]）。
    scored_with: Vec<f32>,
    /// 种子页是英文的（见 [`Self::english_target`]）。
    english_seed: bool,
}

impl Focus {
    pub(crate) fn new(query: &str) -> Self {
        let query = Query::new(query);
        let weights = TermWeights::new(query.len());
        let facets = FacetStats::new(query.len());
        Self {
            scored_with: weights
                .current()
                .iter()
                .copied()
                .chain(facets.shares())
                .collect(),
            query,
            weights,
            facets,
            host: None,
            english_home: false,
            english_seed: false,
        }
    }

    /// 记下种子页声明的语言。
    pub(crate) fn note_seed_language(&mut self, lang: Option<&str>) {
        self.english_seed = lang.is_some_and(is_english);
    }

    /// 英文查询、而且已经知道站点的英文内容在哪（种子页就是英文的，或者找到了英文首页）：
    /// 这时别的语言的页面上的栏目链接不再拿保底分。挪威语主站、德语主站的导航不是线索，
    /// 跟着它们走就把预算花在查询读不懂的页面上了。
    /// 还在找本站的英文版：英文查询，种子页不是英文的，也还没找到英文首页。种子页本身就是英文时
    /// 不找：`english.washington.edu`（英语系）以前被当成英文版入口拿保底分，英文站点上的
    /// 计算机方向一路先抓英语系。
    fn seeking_english(&self) -> bool {
        self.english() && !self.english_home && !self.english_seed
    }

    fn english_target(&self) -> bool {
        self.english() && (self.english_home || self.english_seed)
    }

    /// 记下抓到了本站的英文版首页（见 [`Self::is_language_portal`]）。
    pub(crate) fn found_english_home(&mut self) {
        self.english_home = true;
    }

    /// 记下种子页的主机（见 [`Self::same_host`]）。
    pub(crate) fn with_host(mut self, host: &str) -> Self {
        self.host = Some(host.trim_start_matches("www.").to_ascii_lowercase());
        self
    }

    /// URL 在种子页的主机上（`www.` 不算区别）；没记主机时都算。
    fn same_host(&self, url: &Url) -> bool {
        match (&self.host, url.host_str()) {
            (Some(h), Some(u)) => u.trim_start_matches("www.").eq_ignore_ascii_case(h),
            _ => true,
        }
    }

    /// 这个 URL 是本站英文版的入口：本站主机上的 `/en/`、锚文本是 "English" 的链接，
    /// 或者同一站点的英文子域名（`en.`、`eng.`、`english.`）的首页。别的子域名（院系子站）的
    /// `/en/` 不算，那是子站自己的英文版。
    fn english_entry(&self, url: &Url, anchor: Option<&str>) -> bool {
        if self.same_host(url) {
            return language_portal(url, anchor);
        }
        let host = url.host_str().unwrap_or_default().to_ascii_lowercase();
        let same_site = self
            .host
            .as_deref()
            .is_none_or(|h| pagesift_content::site_key(h) == pagesift_content::site_key(&host));
        let english_label = matches!(
            host.split('.').next().unwrap_or_default(),
            "en" | "eng" | "english"
        );
        let root = url.path().split('/').filter(|s| !s.is_empty()).count() == 0;
        same_site && english_label && root
    }

    /// 记下一页（正文命中的词、各方面的证据），调整词权重和本站统计量。抓到的每一页都要记
    /// （包括无用页：正是它们说明哪些词常见）。
    pub(crate) fn observe_page(&mut self, page: &PageSignal) {
        self.weights.observe(page.body);
        self.facets.observe(&page.facets);
    }

    /// 页面和查询的相关度，0 到 1：标题和正文各占一半；枢纽页（链接标签以外的正文比链接少）和
    /// 家具页打折。`links` 是页面上的链接数。
    #[cfg(test)]
    pub(crate) fn page_relevance(
        &self,
        url: &Url,
        title: Option<&str>,
        markdown: &str,
        links: usize,
    ) -> f32 {
        self.relevance(&self.page_signal(url, title, markdown, links, None))
    }

    /// 相关度的组成（见 [`Self::page_relevance`]）：词权重变了以后按它重算。
    pub(crate) fn page_signal(
        &self,
        url: &Url,
        title: Option<&str>,
        markdown: &str,
        links: usize,
        lang: Option<&str>,
    ) -> PageSignal {
        let mut factor = 1.0;
        if furniture::is_hub(links, prose_words(markdown)) {
            factor *= furniture::HUB_FACTOR;
        }
        if furniture::is_furniture_url(url, &self.wanted()) {
            factor *= furniture::FURNITURE_FACTOR;
        }
        let title = title.unwrap_or_default();
        PageSignal {
            title: self.query.hit_mask(title),
            body: self.query.hit_mask(markdown),
            facets: Facets::of(&self.query, title, &locator(url), markdown),
            factor,
            fixed: 0.0,
            foreign: lang.is_some_and(|l| !is_english(l)),
        }
    }

    pub(crate) fn relevance(&self, p: &PageSignal) -> f32 {
        // 只有给定值的信号（链接的父页面未知、测试）没有页面可看
        if p.title == 0 && p.body == 0 && p.facets.is_empty() {
            return p.fixed;
        }
        p.fixed + p.factor * self.facets.relevance(&p.facets)
    }

    /// 词权重和上次重新打分时比变得够多了（变化最大的词差 [`RESCORE_DELTA`] 以上）：
    /// 排队链接的分数要按新权重重算。返回 true 时记下现在的权重。
    pub(crate) fn weights_moved(&mut self) -> bool {
        let now: Vec<f32> = self
            .weights
            .current()
            .iter()
            .copied()
            .chain(self.facets.shares())
            .collect();
        let moved = now
            .iter()
            .zip(&self.scored_with)
            .any(|(a, b)| (a - b).abs() > RESCORE_DELTA);
        if moved {
            self.scored_with = now;
        }
        moved
    }

    /// 一个词是不是查询要找的：查 "api"、"events" 时，`/api/`、`/events/` 不是家具。
    fn wanted(&self) -> impl Fn(&str) -> bool {
        let query = &self.query;
        move |w: &str| query.hit_mask(w) != 0
    }

    /// 一个链接的分数。`parent` 是所在页面的相关度，`depth` 是链接目标的深度。
    pub(crate) fn link_score(
        &self,
        url: &Url,
        anchor: Option<&str>,
        context: Option<&str>,
        parent: f32,
        depth: usize,
    ) -> f32 {
        self.score(&self.link_signal(url, anchor, context, PageSignal::fixed(parent), depth))
    }

    /// 链接分数的组成（见 [`Self::score`]）：命中了哪些词、所在页面、URL 的形状和深度。
    /// 存下它，词权重变了以后不用再看一遍页面就能重算分数。
    pub(crate) fn link_signal(
        &self,
        url: &Url,
        anchor: Option<&str>,
        context: Option<&str>,
        parent: PageSignal,
        depth: usize,
    ) -> LinkSignal {
        let wanted = self.wanted();
        let furniture = furniture::is_furniture_url(url, &wanted)
            || anchor.is_some_and(|a| furniture::is_furniture_anchor(a, &wanted));
        let shape = if furniture::is_dated_url(url) {
            // 按日期归档的文章是叶子：再切题也通不到别处
            Shape::Factor(furniture::DATED_FACTOR)
        } else if furniture {
            Shape::Factor(furniture::FURNITURE_FACTOR)
        } else if self.english() && foreign_language_segment(url) {
            Shape::Factor(FOREIGN_LANGUAGE_FACTOR)
        } else if self.seeking_english() && self.english_entry(url, anchor) {
            Shape::Floor(LANGUAGE_PORTAL_SCORE)
        } else {
            // 本站的一级栏目拿栏目的保底分；别的主机上的（子站的根或它的一级栏目）拿子站的
            match section(url, anchor) {
                Some(Section::SameHost) if self.same_host(url) => Shape::Floor(SECTION_PRIOR),
                Some(_) => Shape::Floor(SUBSITE_PRIOR),
                None => Shape::Plain,
            }
        };
        LinkSignal {
            anchor: self.query.hit_mask(anchor.unwrap_or_default()),
            locator: self.query.hit_mask(&locator(url)),
            context: self.query.hit_mask(context.unwrap_or_default()),
            parent,
            shape,
            depth: depth.min(64) as u8,
            factor: 1.0,
        }
    }

    /// 链接的分数：锚文本、URL、所在行按当前词权重的覆盖率，加上所在页面的相关度，
    /// 按 URL 的形状打折或给保底分，再按深度衰减。
    pub(crate) fn score(&self, s: &LinkSignal) -> f32 {
        let raw = W_ANCHOR * self.weights.coverage(s.anchor)
            + W_LOCATOR * self.weights.coverage(s.locator)
            + W_CONTEXT * self.weights.coverage(s.context)
            + W_PARENT * self.relevance(&s.parent)
            + if s.anchor.count_ones() >= 2 {
                PHRASE_BONUS
            } else {
                0.0
            };
        let raw = match s.shape {
            Shape::Plain => raw,
            Shape::Factor(f) => raw * f,
            Shape::Floor(_) if s.parent.foreign && self.english_target() => raw,
            Shape::Floor(f) => raw.max(f),
            Shape::Fixed(v) => return v * s.factor,
        };
        raw * DEPTH_DECAY.powi(i32::from(s.depth)) * s.factor
    }

    /// 这一页是不是本站英文版的首页（`/en/`、`/en.html`、`/top/en/`），而且是第一个：
    /// 它是第二个首页，链接不多衰减一层（它下面的 `/en/studies/` 按一级栏目算，见 [`section`]）。
    pub(crate) fn is_language_portal(&self, url: &Url) -> bool {
        self.seeking_english() && self.english_entry(url, None)
    }

    /// 种子页用 hreflang 声明的英文版的分数（英文查询、还没找到英文首页时）：和英文入口一样，
    /// 深度不加（同一页的另一种语言）；别的语言、别的页面声明的都是 None（每一页都声明自己的
    /// 英文版，跟着走就成了按语言重抓一遍）。
    pub(crate) fn alternate_score(&self, lang: &str, depth: usize) -> Option<f32> {
        let english = lang == "en" || lang.starts_with("en-");
        (self.seeking_english() && english && depth == 0).then_some(LANGUAGE_PORTAL_SCORE)
    }

    /// 查询词全是 ASCII（英文）。
    fn english(&self) -> bool {
        self.query.terms().iter().all(|t| t.is_ascii())
    }
}

#[derive(PartialEq, Eq, Debug)]
enum Section {
    /// 本站的一级栏目（`/docs/`、`/services/`）。
    SameHost,
    /// 别的主机的根（子站首页）。
    Subsite,
}

/// 栏目入口：路径一段的是栏目，路径为空的是子站的根；锚文本都要短（1 到 4 个词）。
/// 开头的语言段不算（`/en/studies/`、`/english/research/` 是英文版的一级栏目）。
fn section(url: &Url, anchor: Option<&str>) -> Option<Section> {
    let mut segs: Vec<&str> = url.path().split('/').filter(|s| !s.is_empty()).collect();
    if segs
        .first()
        .is_some_and(|s| is_language_code(s) || is_language_name(s))
    {
        segs.remove(0);
    }
    let segments = segs.len();
    let short = anchor.is_some_and(|a| (1..=4).contains(&a.split_whitespace().count()));
    if !short || url.query().is_some() {
        return None;
    }
    match segments {
        0 => Some(Section::Subsite),
        1 => Some(Section::SameHost),
        _ => None,
    }
}

/// 路径里有别的语言的代码段（`/de/`、`/ja/`、`/zh-cn/`）：英文查询不该进去。
fn foreign_language_segment(url: &Url) -> bool {
    const CODES: &[&str] = &[
        "de", "fr", "es", "it", "nl", "pt", "ru", "ja", "jp", "zh", "zh-cn", "zh-tw", "zh-hans",
        "zh-hant", "ko", "kr", "ar", "sv", "fi", "da", "no", "nb", "pl", "tr", "cs", "el", "he",
        "hi", "th", "vi", "id", "hu", "ro", "uk", "ca", "eu", "gl", "cy", "ga", "mi",
    ];
    url.path()
        .split('/')
        .filter(|s| !s.is_empty())
        .any(|seg| CODES.contains(&language_code(seg).as_str()))
}

/// 通往英文版的入口：锚文本是 "English"/"EN" 之类，或者路径里有 `/en/`、`/english/` 一段。
fn language_portal(url: &Url, anchor: Option<&str>) -> bool {
    let anchor = anchor.unwrap_or_default().trim().to_ascii_lowercase();
    if matches!(
        anchor.as_str(),
        "english"
            | "en"
            | "eng"
            | "in english"
            | "english site"
            | "english version"
            | "english page"
            | "english top"
            | "english home"
    ) {
        return true;
    }
    // 只认英文版的入口本身（`/en/`、`/top/en/`、`/en.html`），不认它下面的每一页（那些按自己的内容打分）
    url.path()
        .split('/')
        .rfind(|seg| !seg.is_empty())
        .is_some_and(|seg| {
            matches!(
                language_code(seg).as_str(),
                "en" | "english" | "en-us" | "en-gb"
            )
        })
}

/// 路径段是英文版的名字（`english`、`eng`）：和语言代码一样是语言段。
fn is_language_name(seg: &str) -> bool {
    matches!(language_code(seg).as_str(), "english" | "eng")
}

/// 语言标签是英文（`en`、`en-US`）。
fn is_english(lang: &str) -> bool {
    let primary = lang.split(['-', '_']).next().unwrap_or_default();
    primary.eq_ignore_ascii_case("en")
}

/// 路径段是语言代码：两个字母，或者 `xx-yy`（可以带扩展名）。
fn is_language_code(seg: &str) -> bool {
    let code = language_code(seg);
    let two = |p: &str| p.len() == 2 && p.bytes().all(|b| b.is_ascii_alphabetic());
    two(&code)
        || code
            .split_once('-')
            .is_some_and(|(a, b)| two(a) && (2..=4).contains(&b.len()))
}

/// 路径段当语言代码看：去掉扩展名，小写（`en.html` → `en`）。
fn language_code(seg: &str) -> String {
    seg.split_once('.')
        .map_or(seg, |(s, _)| s)
        .to_ascii_lowercase()
}

/// URL 里可以当文字看的部分：主机名的标签（`iss.example.edu` 的 iss）、路径段和查询串。
/// 子域名常常就是栏目名（international、admissions、iss），路径同理。
fn locator(url: &Url) -> String {
    let mut s = String::with_capacity(url.as_str().len());
    if let Some(host) = url.host_str() {
        for label in host.split('.') {
            if label != "www" {
                s.push_str(label);
                s.push(' ');
            }
        }
    }
    s.push_str(url.path());
    if let Some(q) = url.query() {
        s.push(' ');
        s.push_str(q);
    }
    s
}

#[cfg(test)]
mod tests {
    use super::*;

    fn u(s: &str) -> Url {
        Url::parse(s).unwrap()
    }

    #[test]
    fn relevant_links_score_higher_and_depth_decays() {
        let f = Focus::new("refund policy");
        let good = f.link_score(
            &u("https://e.com/help/refund-policy"),
            Some("Refund policy"),
            None,
            0.0,
            1,
        );
        let ctx = f.link_score(
            &u("https://e.com/help/x"),
            Some("here"),
            Some("Read our refund policy here"),
            0.0,
            1,
        );
        let bad = f.link_score(&u("https://e.com/team/x"), Some("Our team"), None, 0.0, 1);
        assert!(good > ctx && ctx > bad, "{good} {ctx} {bad}");
        assert_eq!(bad, 0.0);
        // 栏目入口（一层路径、短锚文本）有保底分：先探栏目，再碰带日期的新闻里带一个查询词的
        let section = f.link_score(&u("https://e.com/docs/"), Some("Docs"), None, 0.0, 1);
        let dated = f.link_score(
            &u("https://e.com/blog/2026/04/refund-story/"),
            Some("A refund story"),
            None,
            0.0,
            1,
        );
        assert!(section > dated && dated > 0.0, "{section} {dated}");
        assert!(section < ctx, "{section} {ctx}");
        // 子站的根排在本站栏目之后；别的语言的入口和家具一样
        let subsite = f.link_score(&u("https://inf.e.com/"), Some("Informatics"), None, 0.0, 1);
        let german = f.link_score(&u("https://e.com/de/"), Some("Deutsch"), None, 0.0, 1);
        assert!(
            subsite < section && subsite > german,
            "{section} {subsite} {german}"
        );
        assert!(f.alternate_score("en-gb", 0).is_some());
        assert!(f.alternate_score("de", 0).is_none());
        assert!(
            f.alternate_score("en", 1).is_none(),
            "only the seed's alternate"
        );
        // 英文版下面的一级栏目按栏目算
        let en_section = f.link_score(
            &u("https://e.com/en/studies/"),
            Some("Studies"),
            None,
            0.0,
            1,
        );
        assert!(
            (en_section - section).abs() < 1e-6,
            "{en_section} {section}"
        );
        let english_section = f.link_score(
            &u("https://e.com/english/research/"),
            Some("Research"),
            None,
            0.0,
            1,
        );
        assert!(
            (english_section - section).abs() < 1e-6,
            "{english_section} {section}"
        );
        assert!(f.is_language_portal(&u("https://e.com/en/")));
        assert!(f.is_language_portal(&u("https://e.com/en.html")));
        assert!(!f.is_language_portal(&u("https://e.com/en/docs/")));
        // 记了种子主机以后：别的主机的 /en/ 是子站的根，不是本站的英文入口
        let g = Focus::new("api authentication").with_host("www.e.com");
        let own = g.link_score(&u("https://e.com/en/"), Some("English"), None, 0.0, 1);
        let dept = g.link_score(&u("https://cs.e.com/en/"), Some("English"), None, 0.0, 1);
        assert!(own > dept && dept < section, "{own} {dept} {section}");
        assert!(!g.is_language_portal(&u("https://cs.e.com/en/")));
        // 同一站点的英文子域名首页是英文入口
        assert!(g.is_language_portal(&u("https://en.e.com/")));
        assert!(!g.is_language_portal(&u("https://en.other.com/")));
        assert!(!g.is_language_portal(&u("https://en.e.com/news/")));
        // 抓到英文首页以后，别的英文入口不再拿保底分
        let mut g = g;
        g.found_english_home();
        let later = g.link_score(
            &u("https://e.com/staff/en.html"),
            Some("English"),
            None,
            0.0,
            1,
        );
        assert!(later < own, "{later} {own}");
        assert!(!g.is_language_portal(&u("https://e.com/en/")));
        // 查询要找的词不算家具：查 api 时 /docs/api/ 是正经链接
        let api = Focus::new("api authentication");
        let reference = api.link_score(
            &u("https://e.com/docs/api/"),
            Some("API reference"),
            None,
            0.0,
            2,
        );
        let blog = api.link_score(
            &u("https://e.com/blog/api-news/"),
            Some("API news"),
            None,
            0.0,
            2,
        );
        assert!(reference > 2.0 * blog, "{reference} {blog}");
        let deep = f.link_score(
            &u("https://e.com/help/refund-policy"),
            Some("Refund policy"),
            None,
            0.0,
            3,
        );
        assert!(deep < good);
        let parent = f.link_score(&u("https://e.com/team/x"), Some("Our team"), None, 1.0, 1);
        assert!(parent > bad);
        let home = u("https://e.com/");
        assert!(
            f.page_relevance(&home, Some("Refund policy"), "refund", 3)
                > f.page_relevance(&home, None, "refund", 3)
        );
    }

    #[test]
    fn furniture_and_hubs_are_demoted() {
        let f = Focus::new("international students admission");
        let content = f.link_score(
            &u("https://e.edu/admissions/international/"),
            Some("International students"),
            None,
            0.2,
            2,
        );
        let news = f.link_score(
            &u("https://e.edu/news/2026/05/story-1/"),
            Some("International students celebrate the festival"),
            None,
            0.2,
            1,
        );
        assert!(content > news, "{content} {news}");
        let about = f.link_score(&u("https://e.edu/about/"), Some("About"), None, 0.9, 1);
        let study = f.link_score(&u("https://e.edu/study/"), Some("Study"), None, 0.9, 1);
        assert!(about < study);
        let home = u("https://e.edu/");
        let text = "international students admission";
        let article = f.page_relevance(&home, None, text, 3);
        let hub = f.page_relevance(&home, None, text, 40);
        // 正文全是带查询词的链接标签（文章列表）：也是枢纽页
        let list = "- [International students admission](https://e.edu/a)\n- [International students admission](https://e.edu/b)\n- [International students admission](https://e.edu/c)\n- [International students admission](https://e.edu/d)\n- [International students admission](https://e.edu/e)\n- [International students admission](https://e.edu/f)\n- [International students admission](https://e.edu/g)\n- [International students admission](https://e.edu/h)\n";
        assert!(f.page_relevance(&home, None, list, 8) < f.page_relevance(&home, None, list, 0));
        let dated = f.page_relevance(&u("https://e.edu/news/2026/"), None, text, 3);
        assert!(hub < article && dated < article, "{article} {hub} {dated}");
    }

    #[test]
    fn host_labels_count_and_common_terms_fade() {
        let mut f = Focus::new("international students visa");
        let intl = f.link_score(&u("https://international.e.edu/"), None, None, 0.0, 1);
        let plain = f.link_score(&u("https://www.e.edu/"), None, None, 0.0, 1);
        assert!(intl > plain && plain == 0.0, "{intl} {plain}");
        // 每一页都有 students，visa 只在一页出现：后者的权重更高
        for i in 0..6 {
            let text = if i == 0 {
                "students visa"
            } else {
                "students and more students"
            };
            let page = f.page_signal(&u("https://e.edu/p"), None, text, 0, None);
            f.observe_page(&page);
        }
        let students = f.link_score(&u("https://e.edu/x"), Some("students"), None, 0.0, 1);
        let visa = f.link_score(&u("https://e.edu/y"), Some("visa"), None, 0.0, 1);
        assert!(visa > students, "{visa} {students}");
    }
}
