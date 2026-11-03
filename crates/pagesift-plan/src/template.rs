//! 族内模板概率：一个块在同一路径族其他页面上出现的比例，用 Beta 后验估计。
//!
//! 导航、页脚、订阅框这类模板块几乎出现在族里的每一页；正文块只出现在自己那一页。
//! 对第 `i` 页的块 `b`：`k` = 族里**其他**页面中含 `b` 的页数，`n` = 族里其他页面数，
//! 模板概率 = 后验均值 `(k + 1) / (n + 2)`（Beta(1,1) 先验，Laplace 1814 的接续法则）。
//! 排除当前页，页面自己的正文不会因为"出现在自己身上"被算成模板；族里还没有其他页面时
//! 返回 `None`（不知道），调用方按"不是模板"处理。
//!
//! 按族分片加锁，分片数随核数增长。

use std::collections::{HashMap, HashSet};
use std::sync::Mutex;

#[derive(Default)]
struct Family {
    pages: u64,
    /// 指纹 → 含它的页数。
    seen: HashMap<u64, u64>,
}

pub struct TemplateModel {
    shards: Box<[Mutex<HashMap<u64, Family>>]>,
}

impl Default for TemplateModel {
    fn default() -> Self {
        // 分片数取不小于核数的 2 的幂
        let n = std::thread::available_parallelism()
            .map_or(1, |n| n.get())
            .next_power_of_two();
        Self {
            shards: (0..n).map(|_| Mutex::new(HashMap::new())).collect(),
        }
    }
}

impl TemplateModel {
    fn with<R>(&self, family: u64, f: impl FnOnce(&mut Family) -> R) -> R {
        let shard = &self.shards[(family as usize) & (self.shards.len() - 1)];
        f(shard
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .entry(family)
            .or_default())
    }

    /// 记一页：这一页的块指纹（重复的只算一次）。
    pub fn observe(&self, family: u64, fingerprints: &[u64]) {
        let unique: HashSet<u64> = fingerprints.iter().copied().collect();
        self.with(family, |f| {
            f.pages += 1;
            for fp in unique {
                *f.seen.entry(fp).or_default() += 1;
            }
        });
    }

    /// 已经 [`observe`](Self::observe) 过的一页里，各块的模板概率（排除这一页自己）。
    pub fn probs_observed(&self, family: u64, fingerprints: &[u64]) -> Vec<Option<f64>> {
        self.with(family, |f| {
            let n = f.pages.saturating_sub(1);
            fingerprints
                .iter()
                .map(|fp| {
                    let k = f.seen.get(fp).copied().unwrap_or(0).saturating_sub(1);
                    (n > 0).then(|| (k as f64 + 1.0) / (n as f64 + 2.0))
                })
                .collect()
        })
    }

    /// 还没记过的一页里，各块的模板概率。
    pub fn probs(&self, family: u64, fingerprints: &[u64]) -> Vec<Option<f64>> {
        self.with(family, |f| {
            let n = f.pages;
            fingerprints
                .iter()
                .map(|fp| {
                    let k = f.seen.get(fp).copied().unwrap_or(0);
                    (n > 0).then(|| (k as f64 + 1.0) / (n as f64 + 2.0))
                })
                .collect()
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn repeated_blocks_become_template_and_own_content_does_not() {
        let m = TemplateModel::default();
        // 每页：导航 1、页脚 2、自己的正文 100+i
        for i in 0..20u64 {
            m.observe(7, &[1, 2, 100 + i]);
        }
        let p = m.probs_observed(7, &[1, 2, 105]);
        assert!(p[0].unwrap() > 0.9 && p[1].unwrap() > 0.9);
        assert!(p[2].unwrap() < 0.1);
        // 新的一页：导航仍是模板，没见过的正文不是
        let q = m.probs(7, &[1, 999]);
        assert!(q[0].unwrap() > 0.9 && q[1].unwrap() < 0.1);
        // 没见过的族：不知道
        assert_eq!(m.probs(8, &[1]), vec![None]);
        // 族里只有一页时，排除自己后也是不知道
        m.observe(9, &[1]);
        assert_eq!(m.probs_observed(9, &[1]), vec![None]);
    }
}
