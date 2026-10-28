//! 子树的收成：一个子站或栏目抓了不少页、相关度却一直远低于这次爬取里的好页面，就是撞词的
//! 陷阱（期刊系统一期期目录、科研新闻、商店商品页），之后不再往里抓。
//!
//! 只拿已抓页面的相关度和本次爬取里的好页面比，不看站点、不看具体的词，所以对任何查询都成立。

use std::collections::{HashMap, HashSet};

use pagesift_net::Url;

use super::adaptive::EVIDENCE_RELEVANCE;

/// 一个子树至少抓了这么多页才判断它是不是枯竭。
const MIN_PAGES: u32 = 8;
/// 子树的平均相关度低于好页面平均相关度的这个比例，就算枯竭。在 69 所大学 × 5 个方向的结果上
/// 回放（benchmarks/history/0.80.11）：0.3 时少抓约十分之一的页面、有用页只丢几十页；
/// 0.4 以上开始切掉爱丁堡研究生申请那种相关度不高、但确实有用的大栏目。
const DRY_SHARE: f32 = 0.3;

/// 各子树的收成。
pub(crate) struct Harvest {
    /// 种子页的主机（去掉 www.）。
    seed_host: String,
    /// 子树 → (页数, 相关度之和)。
    trees: HashMap<String, (u32, f32)>,
    /// 好页面（相关度到证据线）的 (页数, 相关度之和)。
    good: (u32, f32),
    dry: HashSet<String>,
}

impl Harvest {
    pub(crate) fn new(seed_host: &str) -> Self {
        Self {
            seed_host: seed_host.trim_start_matches("www.").to_ascii_lowercase(),
            trees: HashMap::new(),
            good: (0, 0.0),
            dry: HashSet::new(),
        }
    }

    /// 页面所在的子树：别的主机（子站）整个算一棵；种子主机上按页面的上级目录分，跳过开头的
    /// 语言段，最多两段（`/research-highlight/某篇` → `research-highlight`，
    /// `/en/education/programmes/x` → `education/programmes`）。只按第一段分的话，整站都在
    /// `/en/` 下的站点就成了一棵树，判它枯竭等于停掉整个爬取。
    fn tree(&self, url: &Url) -> String {
        let host = url
            .host_str()
            .unwrap_or_default()
            .trim_start_matches("www.")
            .to_ascii_lowercase();
        if host != self.seed_host {
            return host;
        }
        let mut segs: Vec<&str> = url.path().split('/').filter(|s| !s.is_empty()).collect();
        segs.pop();
        if segs
            .first()
            .is_some_and(|s| super::is_language_code(s) || super::is_language_name(s))
        {
            segs.remove(0);
        }
        segs.truncate(2);
        format!("{host}/{}", segs.join("/"))
    }

    /// 记下抓到的一页和它的相关度。
    pub(crate) fn observe(&mut self, url: &Url, relevance: f32) {
        if relevance >= EVIDENCE_RELEVANCE {
            self.good.0 += 1;
            self.good.1 += relevance;
        }
        let tree = self.tree(url);
        let t = self.trees.entry(tree.clone()).or_default();
        t.0 += 1;
        t.1 += relevance;
        let (pages, sum) = *t;
        // 还没有好页面时拿证据线当参照
        let reference = if self.good.0 > 0 {
            self.good.1 / self.good.0 as f32
        } else {
            EVIDENCE_RELEVANCE
        };
        if pages >= MIN_PAGES && sum / (pages as f32) < DRY_SHARE * reference {
            self.dry.insert(tree);
        }
    }

    /// 这个 URL 所在的子树已经枯竭。
    pub(crate) fn is_dry(&self, url: &Url) -> bool {
        !self.dry.is_empty() && self.dry.contains(&self.tree(url))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn u(s: &str) -> Url {
        Url::parse(s).unwrap()
    }

    #[test]
    fn a_subtree_far_below_the_good_pages_runs_dry() {
        let mut h = Harvest::new("www.e.edu");
        h.observe(&u("https://www.e.edu/library/hours/"), 0.6);
        h.observe(&u("https://www.e.edu/library/borrow/"), 0.5);
        // 期刊系统：子站上一页页都只沾一点边
        for i in 0..7 {
            h.observe(
                &u(&format!("https://journals.e.edu/j{i}/issue/current")),
                0.07,
            );
        }
        assert!(
            !h.is_dry(&u("https://journals.e.edu/j9/")),
            "seven pages are not enough"
        );
        h.observe(&u("https://journals.e.edu/j7/issue/current"), 0.07);
        assert!(h.is_dry(&u("https://journals.e.edu/j9/")));
        // 别的子树不受影响；种子主机上按第一段路径分
        assert!(!h.is_dry(&u("https://www.e.edu/library/visit/")));
        for i in 0..8 {
            h.observe(
                &u(&format!("https://e.edu/research-highlight/story-{i}")),
                0.05,
            );
        }
        assert!(h.is_dry(&u("https://www.e.edu/research-highlight/story-99")));
        assert!(!h.is_dry(&u("https://www.e.edu/research/")));
        // 相关度一般、但和好页面差得不多的大栏目不算枯竭
        for i in 0..20 {
            h.observe(&u(&format!("https://e.edu/study/programme-{i}")), 0.2);
        }
        assert!(!h.is_dry(&u("https://e.edu/study/programme-99")));
    }
}
