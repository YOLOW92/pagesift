//! 页面相关度：页面是不是"讲"查询的某个方面，按本站的统计量加权。
//!
//! 以前的相关度是"覆盖了多少查询词"（标题、正文各一半），奖励凑词：大学站上 department、
//! faculty、research 到处都是，别的院系的页面凑齐这几个词就拿高分，真正定主题的
//! "computer science" 反而不起作用；一页只讲签证的页面只命中一个词，又吃亏。
//! 现在按"方面"打分，所有量都从本次爬取已抓到的页面里现估，没有调出来的常数：
//!
//! - 单位是查询词，加上查询里相邻的两个词：它们在本站正文里紧挨着出现的次数显著高于
//!   独立出现时的期望（二项近似，超出 3 个标准差），就当成一个词组单位（computer science、
//!   student housing、opening hours）；
//! - 每个单位的证据：出现在标题或网址里（页面"讲的就是它"）、出现在小标题里、正文词频
//!   （按本站平均页长归一化后饱和：tf / (tf + 1 + 页长 / 平均页长)），三者平均，0 到 1；
//! - 单位的权重是它在本站的 IDF：处处都有的词不算数；
//! - 相关度 = 各单位证据按权重的加权平均，0 到 1。
//!
//! 在 1,833 个按正文人工判过的页面上（61 所大学 × 5 个方向，按学校分出没参与设计的一半做验证），
//! 区分有用和无用的 AUC 从 0.79 升到 0.86，区分"正中查询"的从 0.86 升到 0.90。

/// 参与打分的查询词数上限；更长的查询只看前这么多个词。
pub(crate) const TERMS: usize = 16;
/// 单位数：查询词，加上相邻两词组成的候选词组。
const UNITS: usize = 2 * TERMS - 1;

/// 攒够这么多页之后才用本站的统计量；之前查询词同权、不认词组。
const MIN_PAGES: u32 = 3;
/// 词组判定：紧挨着出现的次数超出独立出现期望的标准差倍数。
const COLLOCATION_SIGMA: f64 = 3.0;

/// 一页上每个单位的证据（见模块说明），存在页面信号里：本站统计量变了以后不用再看页面就能重算。
#[derive(Clone, Copy, Debug)]
pub(crate) struct Facets {
    /// 出现在标题或网址里的单位（第 k 位对应单位 k）。
    about: u32,
    /// 出现在小标题里的单位。
    head: u32,
    /// 正文里每个单位出现的次数。
    tf: [u16; UNITS],
    /// 正文的词数。
    len: u32,
}

impl Default for Facets {
    fn default() -> Self {
        Self {
            about: 0,
            head: 0,
            tf: [0; UNITS],
            len: 0,
        }
    }
}

/// 一段文字里各单位的出现次数和词数。
fn count(query: &pagesift_extract::Query, text: &str, tf: &mut [u16; UNITS]) -> u32 {
    let n = query.len().min(TERMS);
    let mut prev: Option<usize> = None;
    let mut len = 0u32;
    query.for_each_term(text, |t| {
        len = len.saturating_add(1);
        let t = t.filter(|&i| i < n);
        if let Some(i) = t {
            tf[i] = tf[i].saturating_add(1);
            if prev.is_some_and(|p| p + 1 == i) {
                tf[n + i - 1] = tf[n + i - 1].saturating_add(1);
            }
        }
        prev = t;
    });
    len
}

fn mask(tf: &[u16; UNITS]) -> u32 {
    tf.iter()
        .enumerate()
        .filter(|(_, c)| **c > 0)
        .fold(0u32, |m, (k, _)| m | (1 << k))
}

impl Facets {
    /// `locator` 是网址拆成的词（主机名和路径），`markdown` 是正文。
    pub(crate) fn of(
        query: &pagesift_extract::Query,
        title: &str,
        locator: &str,
        markdown: &str,
    ) -> Self {
        let mut about = [0u16; UNITS];
        count(query, title, &mut about);
        count(query, locator, &mut about);
        let mut head = [0u16; UNITS];
        for line in markdown.lines() {
            let line = line.trim_start();
            if line.starts_with('#') {
                count(query, line.trim_start_matches('#'), &mut head);
            }
        }
        let mut tf = [0u16; UNITS];
        let len = count(query, markdown, &mut tf);
        Self {
            about: mask(&about),
            head: mask(&head),
            tf,
            len,
        }
    }

    /// 一个词也没有（没有页面可看的信号）。
    pub(crate) fn is_empty(&self) -> bool {
        self.len == 0 && self.about == 0
    }

    /// 这一页上出现过的单位（标题、网址、小标题、正文任一处）。
    fn present(&self) -> u32 {
        self.about | self.head | mask(&self.tf)
    }
}

/// 本次爬取已抓页面的统计量。
pub(crate) struct FacetStats {
    terms: usize,
    pages: u32,
    /// 每个单位出现过的页数。
    df: [u32; UNITS],
    /// 正文总词数。
    tokens: u64,
    /// 正文里每个单位的总次数（词组单位是紧挨着出现的次数）。
    cf: [u64; UNITS],
    weights: [f32; UNITS],
}

impl FacetStats {
    pub(crate) fn new(terms: usize) -> Self {
        let terms = terms.min(TERMS);
        let mut s = Self {
            terms,
            pages: 0,
            df: [0; UNITS],
            tokens: 0,
            cf: [0; UNITS],
            weights: [0.0; UNITS],
        };
        s.weights[..terms].fill(1.0);
        s
    }

    pub(crate) fn observe(&mut self, f: &Facets) {
        self.pages += 1;
        let present = f.present();
        for k in 0..UNITS {
            if present & (1 << k) != 0 {
                self.df[k] += 1;
            }
            self.cf[k] += u64::from(f.tf[k]);
        }
        self.tokens += u64::from(f.len);
        if self.pages < MIN_PAGES {
            return;
        }
        let n = f64::from(self.pages);
        let idf = |df: u32| ((n + 1.0) / (f64::from(df) + 0.5)).ln().max(0.0) as f32;
        let t = self.terms;
        for k in 0..t {
            self.weights[k] = idf(self.df[k]);
        }
        let tokens = self.tokens.max(1) as f64;
        for i in 1..t {
            let k = t + i - 1;
            let expected = self.cf[i - 1] as f64 * self.cf[i] as f64 / tokens;
            let observed = self.cf[k] as f64;
            self.weights[k] = if observed > 0.0
                && observed > expected + COLLOCATION_SIGMA * (expected + 1.0).sqrt()
            {
                idf(self.df[k])
            } else {
                0.0
            };
        }
    }

    /// 0 到 1 的相关度（见模块说明）。
    pub(crate) fn relevance(&self, f: &Facets) -> f32 {
        let avg = if self.pages > 0 && self.tokens > 0 {
            self.tokens as f32 / self.pages as f32
        } else {
            f.len.max(1) as f32
        };
        let (mut num, mut den) = (0.0f32, 0.0f32);
        for (k, &w) in self.weights.iter().enumerate() {
            if w <= 0.0 {
                continue;
            }
            let bit = 1 << k;
            let tf = f32::from(f.tf[k]);
            let body = tf / (tf + 1.0 + f.len as f32 / avg);
            let about = if f.about & bit != 0 { 1.0 } else { 0.0 };
            let head = if f.head & bit != 0 { 1.0 } else { 0.0 };
            num += w * (about + head + body) / 3.0;
            den += w;
        }
        if den > 0.0 { num / den } else { 0.0 }
    }

    /// 当前各单位的权重（归一到和为 1），用来判断要不要重排队列。
    pub(crate) fn shares(&self) -> impl Iterator<Item = f32> + '_ {
        let total: f32 = self.weights.iter().sum();
        self.weights
            .iter()
            .map(move |&w| if total > 0.0 { w / total } else { 0.0 })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use pagesift_extract::Query;

    fn page(q: &Query, title: &str, body: &str) -> Facets {
        Facets::of(q, title, "", body)
    }

    #[test]
    fn a_page_about_the_rare_facet_beats_one_that_collects_common_words() {
        let q = Query::new("computer science department faculty research");
        let mut s = FacetStats::new(q.len());
        let filler = "The department and faculty support research across the university.";
        // 站上每页都有 department、faculty、research；computer science 只在一个系的页面上
        let pages: Vec<Facets> = (0..20)
            .map(|i| {
                if i % 5 == 0 {
                    page(
                        &q,
                        "Computer Science",
                        &format!("{filler} The computer science department lists its faculty."),
                    )
                } else {
                    page(
                        &q,
                        "Biology",
                        &format!("{filler} Biology research and faculty news."),
                    )
                }
            })
            .collect();
        for p in &pages {
            s.observe(p);
        }
        let cs = s.relevance(&pages[0]);
        let bio = s.relevance(&pages[1]);
        assert!(cs > 2.0 * bio, "cs {cs} bio {bio}");
        assert!((0.0..=1.0).contains(&cs) && (0.0..=1.0).contains(&bio));
    }

    #[test]
    fn adjacent_query_words_that_go_together_on_the_site_become_a_phrase() {
        let q = Query::new("student housing");
        let mut s = FacetStats::new(q.len());
        for i in 0..12 {
            let body = if i % 2 == 0 {
                "Apply for student housing. Student housing costs and student housing halls."
            } else {
                "Every student gets news. Housing market report for the city."
            };
            s.observe(&page(&q, "", body));
        }
        assert!(s.weights[2] > 0.0, "{:?}", &s.weights[..3]);
        // 没有搭配关系的相邻词不成词组
        let q = Query::new("library hours");
        let mut s = FacetStats::new(q.len());
        for _ in 0..12 {
            s.observe(&page(&q, "", "The library is large. Office hours vary."));
        }
        assert_eq!(s.weights[2], 0.0);
    }

    #[test]
    fn before_enough_pages_terms_count_equally() {
        let q = Query::new("refund policy");
        let s = FacetStats::new(q.len());
        let both = page(&q, "Refund policy", "Our refund policy.");
        let one = page(&q, "", "A refund.");
        assert!(s.relevance(&both) > s.relevance(&one));
        assert!(s.relevance(&page(&q, "", "nothing")) == 0.0);
    }
}
