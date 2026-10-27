//! 查询词的权重：按已抓页面里的文档频率给 IDF 式权重。
//!
//! 大学站点上每一页的导航和页脚都有 "students"、"international"，这两个词对任何链接都是
//! 常数，分不出招生页和新闻页；"visa"、"scholarship" 只在真正相关的页面出现。抓了几页之后
//! 让常见词的权重降下去、少见词升上来，覆盖率就从"凑词"变成"找证据"。

/// 攒够这么多页之后才用频率权重；之前所有词同权（样本太小会把偶然的词当成常见词）。
const MIN_PAGES: u32 = 3;
/// 权重下限：一个词在每一页都出现也还有一点分量（标题里出现它仍然是信号）。
const MIN_WEIGHT: f32 = 0.15;

pub(crate) struct TermWeights {
    df: Vec<u32>,
    pages: u32,
    weights: Vec<f32>,
}

impl TermWeights {
    pub(crate) fn new(terms: usize) -> Self {
        Self {
            df: vec![0; terms],
            pages: 0,
            weights: vec![1.0; terms],
        }
    }

    /// 记下一页正文命中了哪些词（`mask` 第 i 位对应第 i 个词），更新权重。
    pub(crate) fn observe(&mut self, mask: u64) {
        self.pages += 1;
        for (i, d) in self.df.iter_mut().enumerate() {
            if mask & (1 << i) != 0 {
                *d += 1;
            }
        }
        if self.pages < MIN_PAGES {
            return;
        }
        let n = self.pages as f32;
        for (w, &d) in self.weights.iter_mut().zip(&self.df) {
            // ln((N + 1) / (df + 0.5))，再按 ln(N + 1) 归一到 (0, 1]
            let idf = ((n + 1.0) / (d as f32 + 0.5)).ln() / (n + 1.0).ln();
            *w = idf.clamp(MIN_WEIGHT, 1.0);
        }
    }

    /// 命中位图的加权覆盖率，0 到 1。
    pub(crate) fn coverage(&self, mask: u64) -> f32 {
        let total: f32 = self.weights.iter().sum();
        if total <= 0.0 {
            return 0.0;
        }
        let hit: f32 = self
            .weights
            .iter()
            .enumerate()
            .filter(|(i, _)| mask & (1 << i) != 0)
            .map(|(_, w)| w)
            .sum();
        hit / total
    }

    #[cfg(test)]
    pub(crate) fn weights(&self) -> &[f32] {
        &self.weights
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn common_terms_lose_weight_after_a_few_pages() {
        let mut w = TermWeights::new(3);
        // 词 0 每页都有，词 1 偶尔有，词 2 从未出现
        for page in 0..6u64 {
            w.observe(0b001 | if page % 3 == 0 { 0b010 } else { 0 });
        }
        let ws = w.weights();
        assert!(ws[0] < ws[1] && ws[1] < ws[2], "{ws:?}");
        assert_eq!(ws[2], 1.0);
        assert!(ws[0] >= MIN_WEIGHT);
        // 只命中常见词的文本覆盖率很低；命中稀有词的高
        assert!(w.coverage(0b001) < w.coverage(0b100));
        assert_eq!(w.coverage(0b111), 1.0);
    }

    #[test]
    fn before_enough_pages_all_terms_are_equal() {
        let mut w = TermWeights::new(2);
        w.observe(0b01);
        w.observe(0b01);
        assert_eq!(w.weights(), &[1.0, 1.0]);
        assert_eq!(w.coverage(0b01), 0.5);
    }
}
