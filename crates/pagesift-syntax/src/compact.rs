//! 标记-整理回收：在解析主循环的安全点丢掉不可达的栈节点与子树，让峰值内存跟随“活的”部分而不是累计分配量。
//!
//! 栈节点每个词都会新增、大部分很快就不可达，所以单独频繁整理（代价只与活的栈成正比）；
//! 子树竞技场只在翻倍后才标记，而且活的比例太高时不重建（重建会让新旧两份同时在内存里）。

use crate::parser::Parser;
use crate::stack::SNode;
use crate::tree::{Arena, NONE};

const MIN_TRIGGER: usize = 200_000;

impl Parser<'_> {
    /// 只能在没有悬空节点编号的时刻调用（主循环两轮之间）。
    pub fn maybe_compact(&mut self) {
        if self.st.nodes.len() > MIN_TRIGGER.max(self.live_st_after.saturating_mul(2)) {
            self.compact_stack();
        }
        if self.a.nodes.len() > MIN_TRIGGER.max(self.live_after.saturating_mul(2)) {
            self.compact_arena();
        }
    }

    /// 回收不可达的栈节点，子树编号不变。基节点（0 号）始终保留。
    pub fn compact_stack(&mut self) {
        let sn = self.st.nodes.len();
        let mut smap = vec![NONE; sn];
        let mut order: Vec<u32> = Vec::new();
        let mut work: Vec<u32> = vec![0];
        work.extend(self.st.heads.iter().map(|h| h.node));
        while let Some(n) = work.pop() {
            if smap[n as usize] != NONE {
                continue;
            }
            smap[n as usize] = 0;
            order.push(n);
            work.extend(self.st.nodes[n as usize].links.iter().map(|l| l.node));
        }
        order.sort_unstable();
        for (i, &o) in order.iter().enumerate() {
            smap[o as usize] = i as u32;
        }
        let mut old = std::mem::take(&mut self.st.nodes);
        self.st.nodes.reserve(order.len());
        for &o in &order {
            let mut n = std::mem::replace(
                &mut old[o as usize],
                SNode {
                    state: 0,
                    pos: Default::default(),
                    links: Vec::new(),
                    error_cost: 0,
                    node_count: 0,
                    dprec: 0,
                    depth: 0,
                },
            );
            for l in &mut n.links {
                l.node = smap[l.node as usize];
            }
            self.st.nodes.push(n);
        }
        for h in &mut self.st.heads {
            h.node = smap[h.node as usize];
        }
        self.live_st_after = self.st.nodes.len();
    }

    /// 回收不可达的子树；活的占九成以上就不重建。
    pub fn compact_arena(&mut self) {
        let an = self.a.nodes.len();
        let mut live = vec![false; an];
        let mut roots: Vec<u32> = Vec::new();
        for n in &self.st.nodes {
            roots.extend(n.links.iter().map(|l| l.tree).filter(|t| *t != NONE));
        }
        roots.extend(
            self.st
                .heads
                .iter()
                .map(|h| h.paused_lookahead)
                .filter(|t| *t != NONE),
        );
        if self.finished != NONE {
            roots.push(self.finished);
        }
        let mut count = 0usize;
        while let Some(t) = roots.pop() {
            if live[t as usize] {
                continue;
            }
            live[t as usize] = true;
            count += 1;
            roots.extend_from_slice(self.a.children(t));
        }
        if count * 10 >= an * 9 {
            self.live_after = an;
            return;
        }
        let mut amap = vec![NONE; an];
        let mut next = 0u32;
        for (i, &l) in live.iter().enumerate() {
            if l {
                amap[i] = next;
                next += 1;
            }
        }
        let old = std::mem::take(&mut self.a);
        let mut old = old;
        let mut na = Arena {
            nodes: Vec::with_capacity(count),
            ..Default::default()
        };
        na.ext = std::mem::take(&mut old.ext);
        na.ext_map = std::mem::take(&mut old.ext_map);
        na.ext_total = old.ext_total;
        for (i, n) in old.nodes.iter().enumerate() {
            if !live[i] {
                continue;
            }
            let mut c = n.clone();
            if c.child_count > 0 {
                c.first_child = na.kids.len() as u32;
                na.kids
                    .extend(old.children(i as u32).iter().map(|&k| amap[k as usize]));
            }
            na.nodes.push(c);
        }
        drop(old);
        self.a = na;
        for n in &mut self.st.nodes {
            for l in &mut n.links {
                if l.tree != NONE {
                    l.tree = amap[l.tree as usize];
                }
            }
        }
        for h in &mut self.st.heads {
            if h.paused_lookahead != NONE {
                h.paused_lookahead = amap[h.paused_lookahead as usize];
            }
        }
        if self.finished != NONE {
            self.finished = amap[self.finished as usize];
        }
        self.live_after = self.a.nodes.len();
    }
}
