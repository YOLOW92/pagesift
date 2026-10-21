//! 带查询的整站爬取：相关性优先的调度队列（best-first）、链接打分和自适应停止。
//!
//! 链接分数 = 锚文本、URL 路径、链接所在那一行各自覆盖了多少查询词（加权），
//! 再加上所在页面本身的相关度，最后按深度衰减。分数相同的链接按发现顺序出队，
//! 所以完全不相关的站点上，顺序退化成广度优先。

use std::cmp::Reverse;
use std::collections::{BTreeMap, HashMap, HashSet};
use std::sync::LazyLock;

use pagesift_api::types::AdaptiveOptions;
use pagesift_extract::Query;
use pagesift_net::Url;
use regex::Regex;

const W_ANCHOR: f32 = 2.0;
const W_PATH: f32 = 1.5;
const W_CONTEXT: f32 = 1.0;
const W_PARENT: f32 = 0.5;
/// 每深一层，分数乘以它。
const DEPTH_DECAY: f32 = 0.8;
/// 链接上下文最多取多少字节（一行很长时只看链接附近）。
const CONTEXT_BYTES: usize = 400;

/// 查询和从它派生的打分规则。
pub(crate) struct Focus {
    pub query: Query,
}

impl Focus {
    pub(crate) fn new(query: &str) -> Self {
        Self {
            query: Query::new(query),
        }
    }

    /// 页面和查询的相关度，0 到 1：标题和正文各占一半。
    pub(crate) fn page_relevance(&self, title: Option<&str>, markdown: &str) -> f32 {
        0.5 * self.query.coverage(title.unwrap_or_default()) + 0.5 * self.query.coverage(markdown)
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
        let path = match url.query() {
            Some(q) => format!("{} {q}", url.path()),
            None => url.path().to_string(),
        };
        let raw = W_ANCHOR * self.query.coverage(anchor.unwrap_or_default())
            + W_PATH * self.query.coverage(&path)
            + W_CONTEXT * self.query.coverage(context.unwrap_or_default())
            + W_PARENT * parent;
        raw * DEPTH_DECAY.powi(depth.min(64) as i32)
    }
}

/// 一个链接在正文 Markdown 里的锚文本和所在行（链接语法换成锚文本）。
pub(crate) struct LinkContext {
    pub anchor: String,
    pub line: String,
}

static MD_LINK: LazyLock<Regex> = LazyLock::new(|| {
    Regex::new(r"\[([^\[\]]*)\]\(([^()\s]+)(?:\s+\x22[^\x22]*\x22)?\)").expect("valid regex")
});

/// 从正文 Markdown 里找出每个链接（去掉 fragment 的绝对地址）的锚文本和上下文。
/// 导航栏等不在正文里的链接没有上下文，只按 URL 路径打分。同一链接取第一次出现。
pub(crate) fn link_contexts(markdown: &str) -> HashMap<String, LinkContext> {
    let mut out: HashMap<String, LinkContext> = HashMap::new();
    for line in markdown.lines() {
        if !line.contains("](") {
            continue;
        }
        let plain = MD_LINK.replace_all(line, "$1");
        for cap in MD_LINK.captures_iter(line) {
            let url = cap[2].split('#').next().unwrap_or_default();
            if url.is_empty() || out.contains_key(url) {
                continue;
            }
            let anchor = cap[1].trim().to_string();
            out.insert(
                url.to_string(),
                LinkContext {
                    line: around(&plain, &anchor),
                    anchor,
                },
            );
        }
    }
    out
}

/// 一行里锚文本附近的 [`CONTEXT_BYTES`] 字节；行不长时就是整行。
fn around(line: &str, anchor: &str) -> String {
    if line.len() <= CONTEXT_BYTES {
        return line.to_string();
    }
    let at = line.find(anchor).unwrap_or(0);
    let mut lo = at.saturating_sub(CONTEXT_BYTES / 2);
    let mut hi = (lo + CONTEXT_BYTES).min(line.len());
    while !line.is_char_boundary(lo) {
        lo -= 1;
    }
    while !line.is_char_boundary(hi) {
        hi += 1;
    }
    line[lo..hi].to_string()
}

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

fn score_bits(score: f32) -> u32 {
    if score.is_nan() {
        0
    } else {
        score.max(0.0).to_bits()
    }
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
    pub(crate) fn pop(&mut self) -> Option<(Url, usize, f32)> {
        let ((bits, _), (hash, url, depth)) = self.order.pop_last()?;
        self.index.remove(&hash);
        Some((url, depth, f32::from_bits(bits)))
    }
}

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

    /// 记下一页的正文；信息够了时返回停止原因。
    pub(crate) fn observe(&mut self, query: &Query, markdown: &str) -> Option<&'static str> {
        use std::hash::{BuildHasher, BuildHasherDefault, DefaultHasher};
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
        let bad = f.link_score(&u("https://e.com/about"), Some("About us"), None, 0.0, 1);
        assert!(good > ctx && ctx > bad, "{good} {ctx} {bad}");
        assert_eq!(bad, 0.0);
        let deep = f.link_score(
            &u("https://e.com/help/refund-policy"),
            Some("Refund policy"),
            None,
            0.0,
            3,
        );
        assert!(deep < good);
        let parent = f.link_score(&u("https://e.com/about"), Some("About us"), None, 1.0, 1);
        assert!(parent > bad);
        assert!(
            f.page_relevance(Some("Refund policy"), "refund") > f.page_relevance(None, "refund")
        );
    }

    #[test]
    fn link_contexts_come_from_markdown_lines() {
        let md = "# T\n\nRead the [refund rules](https://e.com/r#top) before buying.\n\n- [About](https://e.com/a \"title\")\n- [Refund again](https://e.com/r)\n\n![img](https://e.com/i.png)\n";
        let c = link_contexts(md);
        let r = &c["https://e.com/r"];
        assert_eq!(r.anchor, "refund rules");
        assert_eq!(r.line, "Read the refund rules before buying.");
        assert_eq!(c["https://e.com/a"].line, "- About");
        assert!(
            c.contains_key("https://e.com/i.png"),
            "images are harmless extras"
        );
        let long = format!(
            "{} [x](https://e.com/x) {}",
            "a".repeat(1000),
            "é".repeat(500)
        );
        let c = link_contexts(&long);
        assert!(c["https://e.com/x"].line.len() <= CONTEXT_BYTES + 4);
        assert!(c["https://e.com/x"].line.contains('x'));
    }

    #[test]
    fn best_first_pops_highest_score_then_discovery_order() {
        let mut q = BestFirst::default();
        assert_eq!(q.offer(1, u("https://e.com/1"), 1, 0.0, 10), Offer::Queued);
        assert_eq!(q.offer(2, u("https://e.com/2"), 1, 0.0, 10), Offer::Queued);
        assert_eq!(q.offer(3, u("https://e.com/3"), 1, 2.0, 10), Offer::Queued);
        assert_eq!(
            q.offer(4, u("https://e.com/4"), 1, f32::NAN, 10),
            Offer::Queued
        );
        // 再次发现时分数更高就前移
        assert_eq!(
            q.offer(2, u("https://e.com/2"), 1, 3.0, 10),
            Offer::AlreadyQueued
        );
        assert_eq!(
            q.offer(3, u("https://e.com/3"), 1, 0.5, 10),
            Offer::AlreadyQueued
        );
        let order: Vec<String> = std::iter::from_fn(|| q.pop())
            .map(|(u, _, _)| u.path().to_string())
            .collect();
        assert_eq!(order, ["/2", "/3", "/1", "/4"]);
        assert!(!q.contains(2));
    }

    #[test]
    fn best_first_is_bounded_and_evicts_the_worst() {
        let mut q = BestFirst::default();
        for i in 0..3 {
            q.offer(i, u(&format!("https://e.com/{i}")), 1, i as f32, 3);
        }
        assert_eq!(q.offer(9, u("https://e.com/9"), 1, 0.0, 3), Offer::Rejected);
        assert_eq!(
            q.offer(8, u("https://e.com/8"), 1, 5.0, 3),
            Offer::Evicted(0)
        );
        assert_eq!(q.len(), 3);
        assert!(!q.contains(0) && q.contains(8));
        // 容量随已调度的页数变小
        assert_eq!(
            q.offer(7, u("https://e.com/7"), 1, 9.0, 1),
            Offer::Evicted(1)
        );
        assert_eq!(q.pop().unwrap().0.path(), "/7");
    }

    #[test]
    fn adaptive_stops_on_coverage() {
        let query = Query::new("refund policy");
        let mut a = Adaptive::new(
            AdaptiveOptions {
                evidence_per_term: 2,
                coverage_threshold: 1.0,
                ..Default::default()
            },
            query.len(),
        );
        assert_eq!(
            a.observe(&query, "Refund window is 30 days.\n\nNothing else."),
            None
        );
        assert_eq!(a.coverage(), 0.25);
        assert_eq!(
            a.observe(&query, "Our policy on refunds.\n\nThe policy is simple."),
            Some("coverage_reached")
        );
        assert_eq!(a.evidence(), [2, 2]);
    }

    #[test]
    fn adaptive_stops_when_pages_add_nothing_new() {
        let query = Query::new("refund");
        let mut a = Adaptive::new(
            AdaptiveOptions {
                patience: 2,
                evidence_per_term: 100,
                ..Default::default()
            },
            query.len(),
        );
        assert_eq!(a.observe(&query, "Refunds take a week.\n\nFooter"), None);
        // 重复的段落和无关页面都不算新信息
        assert_eq!(a.observe(&query, "Refunds take a week.\n\nFooter"), None);
        assert_eq!(a.observe(&query, "Unrelated page"), Some("saturated"));
        // 有新信息时重新计数
        let mut b = Adaptive::new(
            AdaptiveOptions {
                patience: 2,
                evidence_per_term: 100,
                ..Default::default()
            },
            1,
        );
        b.observe(&query, "refund one");
        b.observe(&query, "nothing");
        assert_eq!(
            b.observe(&query, "refund two is a different paragraph"),
            None
        );
        assert_eq!(b.observe(&query, "nothing"), None);
    }
}
