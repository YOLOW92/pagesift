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
/// 同一页上指向同一个簇的链接超过这个数（新闻列表、展品列表），每条按 sqrt(上限/条数) 打折。
pub(crate) const LIST_FREE: usize = 8;

/// 查询和从它派生的打分规则。
pub(crate) struct Focus {
    pub query: Query,
    weights: TermWeights,
}

impl Focus {
    pub(crate) fn new(query: &str) -> Self {
        let query = Query::new(query);
        let weights = TermWeights::new(query.len());
        Self { query, weights }
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
        } else if self.english() && language_portal(url, anchor) {
            raw = raw.max(LANGUAGE_PORTAL_SCORE);
        } else if is_section(url, anchor) {
            raw = raw.max(SECTION_PRIOR);
        }
        raw * DEPTH_DECAY.powi(depth.min(64) as i32)
    }

    /// 查询词全是 ASCII（英文）。
    fn english(&self) -> bool {
        self.query.terms().iter().all(|t| t.is_ascii())
    }
}

/// 栏目入口：路径最多一段（`/docs/`、`/services/`，或子域名的根）、锚文本不超过 4 个词。
fn is_section(url: &Url, anchor: Option<&str>) -> bool {
    let segments = url.path().split('/').filter(|s| !s.is_empty()).count();
    segments <= 1
        && url.query().is_none()
        && anchor.is_some_and(|a| {
            let words = a.split_whitespace().count();
            (1..=4).contains(&words)
        })
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
    // 只认英文版的入口本身（`/en/`、`/top/en/`），不认它下面的每一页（那些按自己的内容打分）
    url.path()
        .split('/')
        .rfind(|seg| !seg.is_empty())
        .is_some_and(|seg| {
            matches!(
                seg.to_ascii_lowercase().as_str(),
                "en" | "english" | "en-us" | "en-gb"
            )
        })
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
