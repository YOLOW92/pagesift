//! 爬取前沿的质量规则：哪些 URL 是同一页的不同写法，哪些不值得抓，哪些是陷阱。
//!
//! - URL 清洗：去掉跟踪参数（utm_*、fbclid、gclid…）和会话 ID（`;jsessionid=`、PHPSESSID…）。
//!   参数名单取自 courlan 的跟踪参数规则和 Heritrix 的会话 ID 规则，只收确定是跟踪或会话的名字，
//!   `ref`、`source`、`id` 这类也可能决定页面内容的参数不动。
//! - 抓取前过滤：图片、音视频、压缩包、字体、脚本样式这类转不成文本的文件；
//!   同一段路径连续重复三次以上（Heritrix 的 PathologicalPath 规则，典型是坏掉的相对链接
//!   `more/` 越叠越长）、路径层级过深、URL 过长。
//! - 按 URL 模板学习陷阱（自研）：把 URL 归成模板（路径里的数字和长 ID 换成占位符，
//!   查询只留参数名），记下每个模板抓到的页面有多少是重复内容（全文相同，或者只差数字，
//!   比如日历的"下一月"、翻到头的空列表页、排序筛选的组合）。一个模板抓了几页、大多是重复，
//!   就把它降级：它的 URL 排到所有正常 URL 之后。只降级不丢弃，正常页面抓完、上限还没到时
//!   仍然会抓它们；模板后来抓到新内容，比例下降，又会恢复正常。
//! - URL 改写规则学习（DUST，Bar-Yossef 等 "Do not crawl in the DUST"，以及 Dasgupta 等
//!   KDD 2008 的 URL 改写规则去重）：两个 URL 的正文完全相同时，比较它们的差别——多了一个查询参数
//!   （`print=1`），或者路径末尾多了一段（`index.html`、`more/`）——记成候选规则；同一条规则
//!   被两对不同的页面证实后生效，之后发现的 URL 先按规则改写再去重，同一页的其他写法不再抓。
//!   参数规则默认只去掉特定的值（`page=1` 和不带参数相同，不代表 `page=2` 也相同）；
//!   同一个参数名有两种以上取值都被证实，才去掉整个参数。

use std::collections::{HashMap, HashSet, VecDeque};

use pagesift_net::Url;

mod clean;
mod learn;
#[cfg(test)]
mod tests;

pub(crate) use clean::*;
pub(crate) use learn::*;

/// 广度优先的两层队列：模板降级的 URL 排在降级层，正常层空了才出队。
/// 正常层按主机分队列（同 Heritrix 的每主机 work queue），主机之间轮流出队，跳过在途请求已满的主机
/// （同 Scrapy 的 DownloaderAwarePriorityQueue），一个主机排得再长也不会挡住其他主机。
/// 总长度由调用方按爬取上限控制；满了以后新来的正常 URL 可以挤掉降级层最后面的一个。
#[derive(Default)]
pub(crate) struct Tiered {
    normal: HashMap<String, VecDeque<(Url, usize, u64)>>,
    /// 正常层里还有 URL 的主机，轮流出队的顺序
    ring: VecDeque<String>,
    normal_len: usize,
    low: VecDeque<(Url, usize, u64)>,
    /// 上次把正常层里已降级的 URL 挪走时，学习器的 generation
    sorted_at: u64,
}

fn host_of(u: &Url) -> String {
    u.host_str().unwrap_or_default().to_string()
}

impl Tiered {
    pub(crate) fn len(&self) -> usize {
        self.normal_len + self.low.len()
    }

    pub(crate) fn push(&mut self, learner: &Learner, url: Url, depth: usize) {
        let key = shape_key(&url);
        if learner.demoted_key(key) {
            self.low.push_back((url, depth, key));
            return;
        }
        let host = host_of(&url);
        let q = self.normal.entry(host.clone()).or_default();
        if q.is_empty() {
            self.ring.push_back(host);
        }
        q.push_back((url, depth, key));
        self.normal_len += 1;
    }

    /// 下一个要抓的 URL，第三项表示是否来自降级层。正常层里模板已降级的顺手挪到降级层。
    /// `busy(host)` 为真的主机这次跳过（在途请求已到上限）。
    /// `allow_low` 为 false 时只出正常层：还有正常页面在抓或等重试，它们可能发现新的正常链接，
    /// 这时候抓降级的 URL 会白白占掉上限。
    pub(crate) fn pop(
        &mut self,
        learner: &Learner,
        allow_low: bool,
        busy: impl Fn(&str) -> bool,
    ) -> Option<(Url, usize, bool)> {
        for _ in 0..self.ring.len() {
            let host = self.ring.pop_front()?;
            if busy(&host) {
                self.ring.push_back(host);
                continue;
            }
            let q = self.normal.get_mut(&host).expect("ring hosts have queues");
            let mut found = None;
            while let Some(item) = q.pop_front() {
                self.normal_len -= 1;
                if learner.demoted_key(item.2) {
                    self.low.push_back(item);
                } else {
                    found = Some((item.0, item.1, false));
                    break;
                }
            }
            if q.is_empty() {
                self.normal.remove(&host);
            } else {
                self.ring.push_back(host);
            }
            if found.is_some() {
                return found;
            }
        }
        if !allow_low || self.low.front().is_some_and(|i| busy(&host_of(&i.0))) {
            return None;
        }
        self.low.pop_front().map(|(u, d, _)| (u, d, true))
    }

    /// 队列满了、又来了一个正常 URL：从降级层末尾挤掉一个，返回被挤掉的 URL（调用方从去重表里删掉它，
    /// 以后还能再进来）。降级层是空的就返回 `None`，新 URL 不入队。
    pub(crate) fn evict_low(&mut self, learner: &Learner) -> Option<Url> {
        if self.sorted_at != learner.generation {
            self.sorted_at = learner.generation;
            for q in self.normal.values_mut() {
                let before = q.len();
                let mut keep = VecDeque::with_capacity(before);
                for item in q.drain(..) {
                    if learner.demoted_key(item.2) {
                        self.low.push_back(item);
                    } else {
                        keep.push_back(item);
                    }
                }
                self.normal_len -= before - keep.len();
                *q = keep;
            }
            self.normal.retain(|_, q| !q.is_empty());
            let normal = &self.normal;
            self.ring.retain(|h| normal.contains_key(h));
        }
        self.low.pop_back().map(|(u, _, _)| u)
    }

    pub(crate) fn iter(&self) -> impl Iterator<Item = (&Url, usize)> {
        self.ring
            .iter()
            .flat_map(|h| self.normal[h].iter())
            .chain(&self.low)
            .map(|(u, d, _)| (u, *d))
    }
}
