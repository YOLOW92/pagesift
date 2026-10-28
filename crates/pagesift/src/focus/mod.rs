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
pub(crate) mod furniture;
mod queue;
mod weights;

pub(crate) use adaptive::{Adaptive, MIN_RELEVANCE};
pub(crate) use context::link_contexts;
use context::prose_words;
pub(crate) use queue::{BestFirst, Offer, cluster_key};

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
/// 别的主机的根（院系、图书馆、系统的子站首页）的保底分：是另一个站，排在本站的栏目之后。
const SUBSITE_PRIOR: f32 = SECTION_PRIOR * 0.5;
/// 英文查询碰到别的语言版本的入口（`/de/`、`/ja/`）：和家具页一样打折。
const FOREIGN_LANGUAGE_FACTOR: f32 = furniture::FURNITURE_FACTOR;
/// 同一页上指向同一个簇的链接超过这个数（新闻列表、展品列表），每条按 sqrt(上限/条数) 打折。
pub(crate) const LIST_FREE: usize = 8;

/// 查询和从它派生的打分规则。
pub(crate) struct Focus {
    pub query: Query,
    weights: TermWeights,
    /// 种子页的主机（去掉 www.）：英文版入口的保底分只给本站的 `/en/`，别的主机的 `/en/` 是子站。
    host: Option<String>,
    /// 已经抓到本站的英文版首页：之后别的 `/xx/en/` 不再拿英文入口的保底分（那是子栏目的英文版）。
    english_home: bool,
}

impl Focus {
    pub(crate) fn new(query: &str) -> Self {
        let query = Query::new(query);
        let weights = TermWeights::new(query.len());
        Self {
            query,
            weights,
            host: None,
            english_home: false,
        }
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

    /// 记下一页正文命中的词，调整词权重。抓到的每一页都要记（包括无用页：正是它们说明哪些词常见）。
    pub(crate) fn observe_page(&mut self, markdown: &str) {
        self.weights.observe(self.query.hit_mask(markdown));
    }

    fn coverage(&self, text: &str) -> f32 {
        self.weights.coverage(self.query.hit_mask(text))
    }

    /// 页面和查询的相关度，0 到 1：标题和正文各占一半；枢纽页（链接标签以外的正文比链接少）和
    /// 家具页打折。`links` 是页面上的链接数。
    pub(crate) fn page_relevance(
        &self,
        url: &Url,
        title: Option<&str>,
        markdown: &str,
        links: usize,
    ) -> f32 {
        let mut r = 0.5 * self.coverage(title.unwrap_or_default()) + 0.5 * self.coverage(markdown);
        if furniture::is_hub(links, prose_words(markdown)) {
            r *= furniture::HUB_FACTOR;
        }
        if furniture::is_furniture_url(url, &self.wanted()) {
            r *= furniture::FURNITURE_FACTOR;
        }
        r
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
        let anchor_mask = self.query.hit_mask(anchor.unwrap_or_default());
        let mut raw = W_ANCHOR * self.weights.coverage(anchor_mask)
            + W_LOCATOR * self.coverage(&locator(url))
            + W_CONTEXT * self.coverage(context.unwrap_or_default())
            + W_PARENT * parent;
        if anchor_mask.count_ones() >= 2 {
            raw += PHRASE_BONUS;
        }
        let wanted = self.wanted();
        let furniture = furniture::is_furniture_url(url, &wanted)
            || anchor.is_some_and(|a| furniture::is_furniture_anchor(a, &wanted));
        if furniture::is_dated_url(url) {
            // 按日期归档的文章是叶子：再切题也通不到别处
            raw *= furniture::DATED_FACTOR;
        } else if furniture {
            raw *= furniture::FURNITURE_FACTOR;
        } else if self.english() && foreign_language_segment(url) {
            raw *= FOREIGN_LANGUAGE_FACTOR;
        } else if self.english() && !self.english_home && self.english_entry(url, anchor) {
            raw = raw.max(LANGUAGE_PORTAL_SCORE);
        } else {
            // 本站的一级栏目拿栏目的保底分；别的主机上的（子站的根或它的一级栏目）拿子站的
            match section(url, anchor) {
                Some(Section::SameHost) if self.same_host(url) => raw = raw.max(SECTION_PRIOR),
                Some(_) => raw = raw.max(SUBSITE_PRIOR),
                None => {}
            }
        }
        raw * DEPTH_DECAY.powi(depth.min(64) as i32)
    }

    /// 这一页是不是本站英文版的首页（`/en/`、`/en.html`、`/top/en/`），而且是第一个：
    /// 它是第二个首页，链接不多衰减一层（它下面的 `/en/studies/` 按一级栏目算，见 [`section`]）。
    pub(crate) fn is_language_portal(&self, url: &Url) -> bool {
        self.english() && !self.english_home && self.english_entry(url, None)
    }

    /// 种子页用 hreflang 声明的英文版的分数（英文查询、还没找到英文首页时）：和英文入口一样，
    /// 深度不加（同一页的另一种语言）；别的语言、别的页面声明的都是 None（每一页都声明自己的
    /// 英文版，跟着走就成了按语言重抓一遍）。
    pub(crate) fn alternate_score(&self, lang: &str, depth: usize) -> Option<f32> {
        let english = lang == "en" || lang.starts_with("en-");
        (self.english() && english && depth == 0 && !self.english_home)
            .then_some(LANGUAGE_PORTAL_SCORE)
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
/// 开头的语言代码段不算（`/en/studies/` 是英文版的一级栏目）。
fn section(url: &Url, anchor: Option<&str>) -> Option<Section> {
    let mut segs: Vec<&str> = url.path().split('/').filter(|s| !s.is_empty()).collect();
    if segs.first().is_some_and(|s| is_language_code(s)) {
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
        assert!(f.page_relevance(&home, None, list, 8) < article);
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
            f.observe_page(if i == 0 {
                "students visa"
            } else {
                "students and more students"
            });
        }
        let students = f.link_score(&u("https://e.edu/x"), Some("students"), None, 0.0, 1);
        let visa = f.link_score(&u("https://e.edu/y"), Some("visa"), None, 0.0, 1);
        assert!(visa > students, "{visa} {students}");
    }
}
