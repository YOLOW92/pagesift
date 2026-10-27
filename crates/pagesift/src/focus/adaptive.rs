//! 自适应停止：每个查询词攒够证据、或者连续几页没有新信息就停。
//! 证据只从相关度过线的页面里取（见 [`MIN_RELEVANCE`]）：新闻页提一句 "students" 不算证据，
//! 否则 12 页预算会在垃圾上提前"覆盖完成"。

use std::collections::HashSet;

use pagesift_api::types::AdaptiveOptions;
use pagesift_extract::Query;

/// 相关度低于它的页面不计入证据，也不算"没有新信息"的一页。
pub(crate) const MIN_RELEVANCE: f32 = 0.3;

/// 自适应爬取的状态：每个查询词的证据数、已经见过的相关段落、连续没有新信息的页数。
pub(crate) struct Adaptive {
    opts: AdaptiveOptions,
    evidence: Vec<usize>,
    /// 相关段落内容的哈希，同样的段落（导航、页脚、转载）只算一次。
    seen: HashSet<u64>,
    /// 已收集的相关内容，按 token 估算。
    collected: usize,
    stale: usize,
}

impl Adaptive {
    pub(crate) fn new(opts: AdaptiveOptions, terms: usize) -> Self {
        Self {
            opts,
            evidence: vec![0; terms],
            seen: HashSet::new(),
            collected: 0,
            stale: 0,
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

    /// 连续没有新信息的页数。
    pub(crate) fn stale(&self) -> usize {
        self.stale
    }

    /// 撤销"连续没有新信息"的计数：队列里还有分数高的链接没抓时，不能因为先抓完的那几页没用就停。
    pub(crate) fn forgive(&mut self) {
        self.stale = 0;
    }

    /// 记下一页的正文（`relevance` 是这一页的相关度）；信息够了时返回停止原因。
    pub(crate) fn observe(
        &mut self,
        query: &Query,
        markdown: &str,
        relevance: f32,
    ) -> Option<&'static str> {
        use std::hash::{BuildHasher, BuildHasherDefault, DefaultHasher};
        if relevance < MIN_RELEVANCE {
            // 还没找到任何相关内容时，无关页不算数（门户、新闻集群挡在前面是常态）；
            // 已经有收获之后连着抓到无关页，说明相关的部分抓完了
            if self.collected > 0 {
                self.stale += 1;
                if self.stale >= self.opts.patience.max(1) {
                    return Some("saturated");
                }
            }
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
        let gain = fresh as f64 / self.collected.max(1) as f64;
        if gain < self.opts.min_gain || fresh == 0 {
            self.stale += 1;
        } else {
            self.stale = 0;
        }
        if self.coverage() >= self.opts.coverage_threshold {
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
    fn adaptive_stops_on_coverage() {
        let q = Query::new("refund policy");
        let mut a = Adaptive::new(opts(1.0, 3, 5), q.len());
        assert_eq!(
            a.observe(&q, "Our refund policy.\n\nAnother refund policy line.", 1.0),
            None
        );
        assert!(a.coverage() < 1.0);
        assert_eq!(
            a.observe(&q, "The policy covers every refund.", 1.0),
            Some("coverage_reached")
        );
    }

    #[test]
    fn adaptive_stops_when_pages_add_nothing_new() {
        let q = Query::new("refund policy");
        let mut a = Adaptive::new(opts(1.0, 100, 2), q.len());
        assert_eq!(a.observe(&q, "Refund policy text.", 1.0), None);
        assert_eq!(a.observe(&q, "Refund policy text.", 1.0), None);
        assert_eq!(a.observe(&q, "Refund policy text.", 1.0), Some("saturated"));
    }

    #[test]
    fn irrelevant_pages_neither_count_as_evidence_nor_as_stale() {
        let q = Query::new("refund policy");
        let mut a = Adaptive::new(opts(1.0, 1, 1), q.len());
        // 新闻页提了一句 refund：相关度低，不算证据，也不算"没有新信息"
        assert_eq!(
            a.observe(&q, "A refund was mentioned in passing.", 0.1),
            None
        );
        assert_eq!(a.evidence(), &[0, 0]);
        assert_eq!(a.observe(&q, "Nothing here.", 0.0), None);
        assert_eq!(
            a.observe(&q, "Refund policy in full.", 0.9),
            Some("coverage_reached")
        );
        // 有了收获之后连着几页无关：相关的部分抓完了
        let mut a = Adaptive::new(opts(1.0, 100, 2), q.len());
        assert_eq!(a.observe(&q, "Refund policy in full.", 0.9), None);
        assert_eq!(a.observe(&q, "Team page.", 0.0), None);
        assert_eq!(a.observe(&q, "Careers page.", 0.0), Some("saturated"));
    }
}
