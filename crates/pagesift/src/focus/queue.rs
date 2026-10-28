//! 相关性优先的待爬队列（best-first），容量有界，出队时按簇分散。

use std::cmp::Reverse;
use std::collections::{BTreeMap, HashMap};

use pagesift_net::Url;

/// 相关性优先的待爬队列。容量由调用方按"还能再抓几页"给出，满了以后新链接只有比队里最差的
/// 好才挤进来：队列长度不超过 limit，和广度优先一样内存有界。
#[derive(Default)]
pub(crate) struct BestFirst {
    /// (分数的位表示, 发现顺序取反) → (去重哈希, URL, 深度)。分数非负，位表示的大小顺序和数值一致；
    /// 最后一项是分数最高、同分里最早发现的。
    order: BTreeMap<(u32, Reverse<u64>), (u64, Url, usize)>,
    index: HashMap<u64, (u32, Reverse<u64>)>,
    seq: u64,
}

#[derive(Debug, PartialEq, Eq)]
pub(crate) enum Offer {
    Queued,
    /// 已经在队里，按更高的分数调整了位置（或者分数没更高，保持不变）。
    AlreadyQueued,
    /// 队列满了，挤掉了这个哈希对应的链接；调用方要把它从去重表里删掉，以后还能再进来。
    Evicted(u64),
    Rejected,
}

/// 出队时的分散规则：一个簇（主机 + 第一段路径）已经调度了 `cap` 页后，
/// 只要别的簇里有分数不低于最高分 [`DIVERSITY_RATIO`] 倍的候选，就先出它。
pub(crate) const DIVERSITY_RATIO: f32 = 0.5;

fn score_bits(score: f32) -> u32 {
    if score.is_nan() {
        0
    } else {
        score.max(0.0).to_bits()
    }
}

/// 簇的键：主机加第一段路径（`news`、`admissions`、`exhibition`…）。
/// 各个主机的首页（`/`、`/en/`、`/index.html`）同属一个簇：大学首页链着几十个院系子站，
/// 每个子站首页都是不同的主机，不归到一起的话按簇分散反而会把预算摊给它们。
pub(crate) fn cluster_key(u: &Url) -> String {
    let segs: Vec<&str> = u.path().split('/').filter(|s| !s.is_empty()).collect();
    // 首页：没有路径，或者只有一段语言代码（`en`、`en-gb`）或 `index`（可以带扩展名）
    let home_segment = |seg: &str| {
        let stem = seg
            .split_once('.')
            .map_or(seg, |(s, _)| s)
            .to_ascii_lowercase();
        let lang = |p: &str| p.len() == 2 && p.bytes().all(|b| b.is_ascii_alphabetic());
        stem == "index"
            || lang(&stem)
            || stem
                .split_once('-')
                .is_some_and(|(a, b)| lang(a) && (2..=4).contains(&b.len()))
    };
    let root = match segs.as_slice() {
        [] => true,
        [one] => home_segment(one),
        _ => false,
    };
    if root {
        return "*/".to_string();
    }
    format!("{}/{}", u.host_str().unwrap_or_default(), segs[0])
}

impl BestFirst {
    pub(crate) fn len(&self) -> usize {
        self.order.len()
    }

    pub(crate) fn is_empty(&self) -> bool {
        self.order.is_empty()
    }

    pub(crate) fn contains(&self, hash: u64) -> bool {
        self.index.contains_key(&hash)
    }

    /// 放入一个链接。`room` 是队列现在最多能有多少项（limit 减去已经调度的页数）。
    pub(crate) fn offer(
        &mut self,
        hash: u64,
        url: Url,
        depth: usize,
        score: f32,
        room: usize,
    ) -> Offer {
        let bits = score_bits(score);
        if let Some(&old) = self.index.get(&hash) {
            if bits > old.0
                && let Some(entry) = self.order.remove(&old)
            {
                self.order.insert((bits, old.1), entry);
                self.index.insert(hash, (bits, old.1));
            }
            return Offer::AlreadyQueued;
        }
        let mut evicted = None;
        if self.order.len() >= room {
            match self.order.first_key_value() {
                Some((&worst, _)) if worst.0 < bits => {
                    let (h, _, _) = self.order.remove(&worst).expect("present");
                    self.index.remove(&h);
                    evicted = Some(h);
                }
                _ => return Offer::Rejected,
            }
        }
        let key = (bits, Reverse(self.seq));
        self.seq += 1;
        self.order.insert(key, (hash, url, depth));
        self.index.insert(hash, key);
        evicted.map_or(Offer::Queued, Offer::Evicted)
    }

    /// 取出分数最高的链接：(URL, 深度, 分数)。
    #[cfg(test)]
    pub(crate) fn pop(&mut self) -> Option<(Url, usize, f32)> {
        let key = *self.order.last_key_value()?.0;
        self.take(key)
    }

    /// 按簇分散地出队：分数最高的链接所在的簇已经调度了 `cap` 页时，改出别的簇里分数不低于
    /// 最高分一半的最好链接；没有这样的链接就还是出最高分的。`scheduled` 是每个簇已调度的页数。
    pub(crate) fn pop_diverse(
        &mut self,
        scheduled: &HashMap<String, usize>,
        cap: usize,
    ) -> Option<(Url, usize, f32)> {
        let (&top_key, (_, top_url, _)) = self.order.last_key_value()?;
        let top_score = f32::from_bits(top_key.0);
        let full = |u: &Url| scheduled.get(&cluster_key(u)).copied().unwrap_or(0) >= cap;
        if !full(top_url) {
            return self.take(top_key);
        }
        let floor = top_score * DIVERSITY_RATIO;
        let pick = self
            .order
            .iter()
            .rev()
            .take_while(|(k, _)| f32::from_bits(k.0) >= floor)
            .find(|(_, (_, u, _))| !full(u))
            .map(|(k, _)| *k)
            .unwrap_or(top_key);
        self.take(pick)
    }

    /// 队首（分数最高）的分数。
    pub(crate) fn peek_score(&self) -> Option<f32> {
        self.order
            .last_key_value()
            .map(|(k, _)| f32::from_bits(k.0))
    }

    fn take(&mut self, key: (u32, Reverse<u64>)) -> Option<(Url, usize, f32)> {
        let (hash, url, depth) = self.order.remove(&key)?;
        self.index.remove(&hash);
        Some((url, depth, f32::from_bits(key.0)))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn u(s: &str) -> Url {
        Url::parse(s).unwrap()
    }

    #[test]
    fn best_first_pops_highest_score_then_discovery_order() {
        let mut q = BestFirst::default();
        q.offer(1, u("https://e.com/a"), 1, 0.5, 10);
        q.offer(2, u("https://e.com/b"), 1, 2.0, 10);
        q.offer(3, u("https://e.com/c"), 1, 0.5, 10);
        assert_eq!(q.pop().unwrap().0.path(), "/b");
        assert_eq!(q.pop().unwrap().0.path(), "/a");
        assert_eq!(q.pop().unwrap().0.path(), "/c");
        assert!(q.pop().is_none());
    }

    #[test]
    fn best_first_is_bounded_and_evicts_the_worst() {
        let mut q = BestFirst::default();
        assert_eq!(q.offer(1, u("https://e.com/a"), 1, 0.1, 2), Offer::Queued);
        assert_eq!(q.offer(2, u("https://e.com/b"), 1, 0.2, 2), Offer::Queued);
        assert_eq!(
            q.offer(3, u("https://e.com/c"), 1, 0.05, 2),
            Offer::Rejected
        );
        assert_eq!(
            q.offer(4, u("https://e.com/d"), 1, 0.3, 2),
            Offer::Evicted(1)
        );
        assert_eq!(q.len(), 2);
        assert!(!q.contains(1) && q.contains(4));
        assert_eq!(
            q.offer(4, u("https://e.com/d"), 1, 0.9, 2),
            Offer::AlreadyQueued
        );
        assert_eq!(q.pop().unwrap().2, 0.9);
    }

    #[test]
    fn diverse_pop_leaves_a_saturated_cluster_for_a_close_rival() {
        let mut q = BestFirst::default();
        q.offer(1, u("https://e.com/news/1"), 1, 1.0, 10);
        q.offer(2, u("https://e.com/news/2"), 1, 0.9, 10);
        q.offer(3, u("https://e.com/study/intl"), 1, 0.6, 10);
        q.offer(4, u("https://e.com/about"), 1, 0.2, 10);
        let mut counts = HashMap::new();
        counts.insert("e.com/news".to_string(), 3);
        // news 已经抓了 3 页：先出 study（0.6 ≥ 1.0 × 0.5），再回到 news
        assert_eq!(q.pop_diverse(&counts, 3).unwrap().0.path(), "/study/intl");
        assert_eq!(q.pop_diverse(&counts, 3).unwrap().0.path(), "/news/1");
        // 剩下的 about 分太低，不够资格插队
        assert_eq!(q.pop_diverse(&counts, 3).unwrap().0.path(), "/news/2");
        assert_eq!(cluster_key(&u("https://e.com/")), "*/");
        assert_eq!(cluster_key(&u("https://cs.e.com/en/")), "*/");
        assert_eq!(cluster_key(&u("https://e.com/en.html")), "*/");
        assert_eq!(cluster_key(&u("https://e.com/en-gb/")), "*/");
        assert_eq!(cluster_key(&u("https://e.com/news/")), "e.com/news");
        assert_eq!(cluster_key(&u("https://e.com/study")), "e.com/study");
    }
}
