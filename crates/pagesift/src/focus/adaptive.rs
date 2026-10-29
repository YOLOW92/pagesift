//! 自适应停止：每个查询词攒够证据、或者连续几页没有新信息就停；一直找不到相关内容、
//! 能走的又都是没有线索的链接时也停（见 [`Adaptive::observe`]）。
//! 证据只从相关度过线的页面里取（见 [`MIN_RELEVANCE`]）：新闻页提一句 "students" 不算证据，
//! 否则 12 页预算会在垃圾上提前"覆盖完成"。

use std::collections::HashSet;

use pagesift_api::types::AdaptiveOptions;
use pagesift_extract::Query;

/// 相关度低于它的页面算无关：不计入证据，而且找到相关内容之后连着出现就算"抓完了"。
/// 附件收割也用它。0.81 换成按方面打分的相关度（[`super::facet`]）后，两条门槛按 1,833 个
/// 人工判过的页面换算：保留下来的有用页比例和换算前一样（0.12 → 0.05 都约 90%，
/// 0.3 → 0.15 都约 52%），保留下来的页面里有用的比例略高。
pub(crate) const MIN_RELEVANCE: f32 = 0.05;
/// 相关度不低于它的页面才贡献证据、才参与"有没有新信息"的判断；两者之间的页面不算数
/// （沾了点边的页面：既不能让覆盖度提前达标，也不能把相关的部分判成抓完了）。
pub(crate) const EVIDENCE_RELEVANCE: f32 = 0.15;

/// 还没有收获时，连续盲探多少倍 patience 页才放弃。在 69 所大学 × 5 个方向、上限 100 的结果上
/// 模拟（benchmarks/history/0.80.10）：2 倍（默认 10 页）省下约四分之一的页数，丢掉的有用页
/// 不到 5%，丢的都是盲探了几十页才偶然撞上内容的站点。
const COLD_PATIENCE: usize = 2;

/// 覆盖度达标至少要有这么多页贡献过证据。清华的住宿方向在第 2 页（一张"住宿与餐饮"综合页）
/// 就"覆盖完成"停下，后面的申请、收费页都没抓。
const MIN_SOURCES: usize = 2;

/// 自适应爬取的状态：每个查询词的证据数、已经见过的相关段落、连续没有新信息的页数。
pub(crate) struct Adaptive {
    opts: AdaptiveOptions,
    evidence: Vec<usize>,
    /// 相关段落内容的哈希，同样的段落（导航、页脚、转载）只算一次。
    seen: HashSet<u64>,
    /// 已收集的相关内容，按 token 估算。
    collected: usize,
    stale: usize,
    /// 还没有收获时，连续"盲探"（见 [`Adaptive::observe`]）而且无关的页数。
    cold: usize,
    /// 贡献过新证据的页数。
    sources: usize,
    /// 上一页带来了新的切题内容。
    last_fresh: bool,
}

impl Adaptive {
    pub(crate) fn new(opts: AdaptiveOptions, terms: usize) -> Self {
        Self {
            opts,
            evidence: vec![0; terms],
            seen: HashSet::new(),
            collected: 0,
            stale: 0,
            cold: 0,
            sources: 0,
            last_fresh: false,
        }
    }

    /// 覆盖度：各查询词 `min(1, 证据 / evidencePerTerm)` 的平均值。
    pub(crate) fn coverage(&self) -> f64 {
        if self.evidence.is_empty() {
            return 0.0;
        }
        let target = self.opts.evidence_per_term.max(1) as f64;
        self.evidence
            .iter()
            .map(|&e| (e as f64 / target).min(1.0))
            .sum::<f64>()
            / self.evidence.len() as f64
    }

    pub(crate) fn evidence(&self) -> &[usize] {
        &self.evidence
    }

    /// 已经有过收获（某一页带来了新的切题内容）。
    pub(crate) fn has_found(&self) -> bool {
        self.collected > 0
    }

    /// 查询已经覆盖：覆盖度到阈值，而且至少两页贡献过证据。
    pub(crate) fn covered(&self) -> bool {
        self.coverage() >= self.opts.coverage_threshold && self.sources >= MIN_SOURCES
    }

    /// 上一次 [`Self::observe`] 的页面带来了新的切题内容（有收获）。
    pub(crate) fn last_fresh(&self) -> bool {
        self.last_fresh
    }

    /// 连续没有新信息的页数。
    pub(crate) fn stale(&self) -> usize {
        self.stale
    }

    /// 撤销"连续没有新信息"的计数：队列里还有分数高的链接没抓时，不能因为先抓完的那几页没用就停。
    pub(crate) fn forgive(&mut self) {
        self.stale = 0;
    }

    /// 记下一页的正文（`relevance` 是这一页的相关度）；信息够了时返回停止原因。
    /// `blind` 说这一页是不是"盲探"来的：链接本身没有查询词的线索，分数只是栏目保底分
    /// （见 [`super::is_blind`]）。
    ///
    /// 还没有收获时，无关页不算"没有新信息"（门户、新闻集群挡在前面是常态），但连着
    /// 2 × patience 页都是盲探、都无关，就是站点上找不到查询的线索：停（`no_relevant_pages`）。
    /// 一路有线索的链接可走时不停，那是内容藏得深，不是没有内容。
    pub(crate) fn observe(
        &mut self,
        query: &Query,
        markdown: &str,
        relevance: f32,
        blind: bool,
    ) -> Option<&'static str> {
        self.last_fresh = false;
        use std::hash::{BuildHasher, BuildHasherDefault, DefaultHasher};
        if self.collected == 0 {
            if blind && relevance < MIN_RELEVANCE {
                self.cold += 1;
                if self.cold >= COLD_PATIENCE * self.opts.patience.max(1) {
                    return Some("no_relevant_pages");
                }
            } else {
                self.cold = 0;
            }
        }
        if relevance < MIN_RELEVANCE {
            // 已经有收获之后连着抓到无关页，说明相关的部分抓完了
            if self.collected > 0 {
                self.stale += 1;
                return self.verdict();
            }
            return None;
        }
        if relevance < EVIDENCE_RELEVANCE {
            return None;
        }
        let hasher = BuildHasherDefault::<DefaultHasher>::default();
        let mut fresh = 0usize;
        for block in markdown.split("\n\n") {
            let block = block.trim();
            if block.is_empty() {
                continue;
            }
            let mask = query.hit_mask(block);
            if mask == 0 || !self.seen.insert(hasher.hash_one(block)) {
                continue;
            }
            for (i, e) in self.evidence.iter_mut().enumerate() {
                if mask & (1 << i) != 0 {
                    *e += 1;
                }
            }
            fresh += pagesift_content::estimate_tokens(block).max(1);
        }
        self.collected += fresh;
        self.sources += usize::from(fresh > 0);
        let gain = fresh as f64 / self.collected.max(1) as f64;
        if gain < self.opts.min_gain || fresh == 0 {
            self.stale += 1;
        } else {
            self.stale = 0;
            self.last_fresh = true;
        }
        self.verdict()
    }

    /// 按现在的覆盖度和"连续没有新信息"的页数决定停不停。
    /// 覆盖够了也不马上停：页面还在带来新的相关内容（曼大图书馆一个个分馆的开放时间）就接着抓，
    /// 连着几页没有新东西才停，比没覆盖时的耐心短一半。一页就可能把每个词都提够次数
    /// （栏目首页、综合介绍页），所以覆盖要有两页以上的证据才算。
    fn verdict(&self) -> Option<&'static str> {
        let covered = self.covered();
        if covered && self.stale >= self.opts.patience.div_ceil(2).max(1) {
            Some("coverage_reached")
        } else if self.stale >= self.opts.patience.max(1) {
            Some("saturated")
        } else {
            None
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn opts(threshold: f64, per_term: usize, patience: usize) -> AdaptiveOptions {
        AdaptiveOptions {
            coverage_threshold: threshold,
            evidence_per_term: per_term,
            patience,
            min_gain: 0.05,
        }
    }

    #[test]
    fn adaptive_stops_once_covered_and_nothing_new_comes() {
        let q = Query::new("refund policy");
        let mut a = Adaptive::new(opts(1.0, 3, 5), q.len());
        assert_eq!(
            a.observe(
                &q,
                "Our refund policy.\n\nAnother refund policy line.",
                1.0,
                false
            ),
            None
        );
        assert!(a.coverage() < 1.0);
        // 覆盖够了，但页面还在带来新内容：接着抓
        assert_eq!(
            a.observe(&q, "The policy covers every refund.", 1.0, false),
            None
        );
        assert!(a.coverage() >= 1.0);
        assert_eq!(
            a.observe(&q, "Refunds for gift cards: policy.", 1.0, false),
            None
        );
        // 覆盖之后连着 ceil(5 / 2) = 3 页没有新东西才停
        assert_eq!(a.observe(&q, "Our refund policy.", 1.0, false), None);
        assert_eq!(a.observe(&q, "Our refund policy.", 1.0, false), None);
        assert_eq!(
            a.observe(&q, "Our refund policy.", 1.0, false),
            Some("coverage_reached")
        );
    }

    #[test]
    fn adaptive_stops_when_pages_add_nothing_new() {
        let q = Query::new("refund policy");
        let mut a = Adaptive::new(opts(1.0, 100, 2), q.len());
        assert_eq!(a.observe(&q, "Refund policy text.", 1.0, false), None);
        assert_eq!(a.observe(&q, "Refund policy text.", 1.0, false), None);
        assert_eq!(
            a.observe(&q, "Refund policy text.", 1.0, false),
            Some("saturated")
        );
    }

    #[test]
    fn irrelevant_pages_neither_count_as_evidence_nor_as_stale() {
        let q = Query::new("refund policy");
        let mut a = Adaptive::new(opts(1.0, 1, 1), q.len());
        // 新闻页提了一句 refund：相关度低，不算证据，也不算"没有新信息"
        assert_eq!(
            a.observe(&q, "A refund was mentioned in passing.", 0.03, false),
            None
        );
        assert_eq!(a.evidence(), &[0, 0]);
        assert_eq!(a.observe(&q, "Nothing here.", 0.0, false), None);
        // 一页就提够了每个词：还要第二页的证据；覆盖之后一页没有新东西（patience 1）就停
        assert_eq!(a.observe(&q, "Refund policy in full.", 0.9, false), None);
        assert_eq!(a.observe(&q, "The refund policy, again.", 0.9, false), None);
        assert_eq!(
            a.observe(&q, "Refund policy in full.", 0.9, false),
            Some("coverage_reached")
        );
        // 有了收获之后连着几页无关：相关的部分抓完了；沾点边的页面（两个门槛之间）不算数
        let mut a = Adaptive::new(opts(1.0, 100, 2), q.len());
        assert_eq!(a.observe(&q, "Refund policy in full.", 0.9, false), None);
        assert_eq!(a.observe(&q, "Team page.", 0.0, false), None);
        assert_eq!(a.observe(&q, "Refund mentioned once.", 0.1, false), None);
        assert_eq!(a.evidence(), &[1, 1]);
        assert_eq!(
            a.observe(&q, "Careers page.", 0.0, false),
            Some("saturated")
        );
    }

    #[test]
    fn gives_up_when_only_blind_links_are_left_and_nothing_was_found() {
        let q = Query::new("refund policy");
        let mut a = Adaptive::new(opts(1.0, 100, 2), q.len());
        // 有线索的链接抓回来没用：不算盲探
        for _ in 0..10 {
            assert_eq!(a.observe(&q, "Team page.", 0.0, false), None);
        }
        // 盲探 3 页、中间一页沾边：重新数
        for _ in 0..3 {
            assert_eq!(a.observe(&q, "About us.", 0.0, true), None);
        }
        assert_eq!(a.observe(&q, "Refund mentioned once.", 0.1, true), None);
        for _ in 0..3 {
            assert_eq!(a.observe(&q, "Careers.", 0.0, true), None);
        }
        assert_eq!(
            a.observe(&q, "Contact.", 0.0, true),
            Some("no_relevant_pages")
        );
        // 已经有收获时不放弃，按"没有新信息"算
        let mut a = Adaptive::new(opts(1.0, 100, 5), q.len());
        assert_eq!(a.observe(&q, "Refund policy in full.", 0.9, false), None);
        for _ in 0..4 {
            assert_eq!(a.observe(&q, "Careers.", 0.0, true), None);
        }
    }
}
