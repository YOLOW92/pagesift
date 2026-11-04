//! 子树竞技场：所有节点放在一个向量里，用 u32 下标互相引用（不可变共享，没有递归释放）。

use crate::table::{Language, SYM_ERROR, SYM_ERROR_REPEAT};

pub const ERROR_COST_PER_RECOVERY: u32 = 500;
pub const ERROR_COST_PER_MISSING_TREE: u32 = 110;
pub const ERROR_COST_PER_SKIPPED_TREE: u32 = 100;
pub const ERROR_COST_PER_SKIPPED_LINE: u32 = 30;
pub const ERROR_COST_PER_SKIPPED_CHAR: u32 = 1;

/// 位置/长度：字节数与行数（换行个数）。
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct Len {
    pub bytes: u32,
    pub rows: u32,
}

impl Len {
    #[allow(clippy::should_implement_trait)]
    pub fn add(self, o: Len) -> Len {
        Len {
            bytes: self.bytes.wrapping_add(o.bytes),
            rows: self.rows.wrapping_add(o.rows),
        }
    }
    #[allow(clippy::should_implement_trait)]
    pub fn sub(self, o: Len) -> Len {
        Len {
            bytes: self.bytes.wrapping_sub(o.bytes),
            rows: self.rows.wrapping_sub(o.rows),
        }
    }
}

pub const NONE: u32 = u32::MAX;

pub const F_VISIBLE: u8 = 1;
pub const F_NAMED: u8 = 2;
pub const F_EXTRA: u8 = 4;
pub const F_MISSING: u8 = 8;
pub const F_KEYWORD: u8 = 16;

#[derive(Clone, Debug)]
pub struct Node {
    pub sym: u16,
    pub flags: u8,
    pub prod: u16,
    pub padding: Len,
    pub size: Len,
    pub first_child: u32,
    pub child_count: u32,
    pub error_cost: u32,
    pub dprec: i32,
    pub visible_descendants: u32,
    pub visible_children: u32,
    pub named_children: u32,
    /// 词法跳过的非法字符（ERROR 叶子）。
    pub lchar: u32,
    /// 最后一个外部记号之后的扫描器状态编号（`Arena::ext_bytes` 的下标），子树里没有外部记号为 NONE。
    pub ext: u32,
}

impl Node {
    #[inline]
    pub fn extra(&self) -> bool {
        self.flags & F_EXTRA != 0
    }
    #[inline]
    pub fn visible(&self) -> bool {
        self.flags & F_VISIBLE != 0
    }
    #[inline]
    pub fn named(&self) -> bool {
        self.flags & F_NAMED != 0
    }
    #[inline]
    pub fn missing(&self) -> bool {
        self.flags & F_MISSING != 0
    }
    #[inline]
    pub fn is_error(&self) -> bool {
        self.sym == SYM_ERROR
    }
    #[inline]
    pub fn total(&self) -> Len {
        self.padding.add(self.size)
    }
    /// 官方 `ts_subtree_error_cost`：缺失节点固定代价。
    #[inline]
    pub fn cost(&self) -> u32 {
        if self.missing() {
            ERROR_COST_PER_MISSING_TREE + ERROR_COST_PER_RECOVERY
        } else {
            self.error_cost
        }
    }
}

#[derive(Default)]
pub struct Arena {
    pub nodes: Vec<Node>,
    pub kids: Vec<u32>,
    /// 去重后的扫描器状态；编号 0 固定表示空状态，编号 i>0 对应 `ext[i-1]`。
    pub ext: Vec<Vec<u8>>,
    pub ext_map: std::collections::HashMap<Vec<u8>, u32>,
    pub ext_total: usize,
}

fn extent_cost(size: Len) -> u32 {
    ERROR_COST_PER_RECOVERY
        .wrapping_add(ERROR_COST_PER_SKIPPED_CHAR.wrapping_mul(size.bytes))
        .wrapping_add(ERROR_COST_PER_SKIPPED_LINE.wrapping_mul(size.rows))
}

impl Arena {
    #[inline]
    pub fn n(&self, i: u32) -> &Node {
        &self.nodes[i as usize]
    }
    #[inline]
    pub fn n_mut(&mut self, i: u32) -> &mut Node {
        &mut self.nodes[i as usize]
    }
    /// 登记一份扫描器状态，相同内容共用同一个编号。
    pub fn intern_ext(&mut self, bytes: &[u8]) -> u32 {
        if bytes.is_empty() {
            return 0;
        }
        if let Some(&i) = self.ext_map.get(bytes) {
            return i;
        }
        self.ext.push(bytes.to_vec());
        let i = self.ext.len() as u32;
        self.ext_map.insert(bytes.to_vec(), i);
        self.ext_total += bytes.len() * 2;
        i
    }
    pub fn ext_bytes(&self, i: u32) -> &[u8] {
        if i == 0 || i == NONE {
            &[]
        } else {
            self.ext.get(i as usize - 1).map_or(&[], |v| v.as_slice())
        }
    }
    pub fn children(&self, i: u32) -> &[u32] {
        let n = &self.nodes[i as usize];
        &self.kids[n.first_child as usize..(n.first_child + n.child_count) as usize]
    }

    pub fn new_leaf(&mut self, lang: &Language, sym: u16, padding: Len, size: Len) -> u32 {
        let mut flags = 0;
        if lang.visible(sym) {
            flags |= F_VISIBLE;
        }
        if lang.named(sym) {
            flags |= F_NAMED;
        }
        if sym == 0 {
            flags |= F_EXTRA; // 输入结束记号本身是 extra
        }
        self.push(Node {
            sym,
            flags,
            prod: 0,
            padding,
            size,
            first_child: 0,
            child_count: 0,
            error_cost: 0,
            dprec: 0,
            visible_descendants: 0,
            visible_children: 0,
            named_children: 0,
            lchar: 0,
            ext: NONE,
        })
    }

    pub fn new_missing_leaf(&mut self, lang: &Language, sym: u16) -> u32 {
        let i = self.new_leaf(lang, sym, Len::default(), Len::default());
        self.n_mut(i).flags |= F_MISSING;
        i
    }

    fn push(&mut self, n: Node) -> u32 {
        self.nodes.push(n);
        (self.nodes.len() - 1) as u32
    }

    /// 官方 `ts_subtree_new_node` + `summarize_children`。
    pub fn new_node(&mut self, lang: &Language, sym: u16, children: &[u32], prod: u16) -> u32 {
        let first_child = self.kids.len() as u32;
        self.kids.extend_from_slice(children);
        let mut node = Node {
            sym,
            flags: 0,
            prod,
            padding: Len::default(),
            size: Len::default(),
            first_child,
            child_count: children.len() as u32,
            error_cost: 0,
            dprec: 0,
            visible_descendants: 0,
            visible_children: 0,
            named_children: 0,
            lchar: 0,
            ext: NONE,
        };
        if lang.visible(sym) {
            node.flags |= F_VISIBLE;
        }
        if lang.named(sym) {
            node.flags |= F_NAMED;
        }
        let is_err_parent = sym == SYM_ERROR || sym == SYM_ERROR_REPEAT;
        let alias = lang.alias_sequence(prod);
        let mut structural = 0usize;
        for (i, &c) in children.iter().enumerate() {
            let ch = &self.nodes[c as usize];
            if i == 0 {
                node.padding = ch.padding;
                node.size = ch.size;
            } else {
                node.size = node.size.add(ch.total());
            }
            let gc = ch.child_count;
            if ch.sym == SYM_ERROR_REPEAT {
                node.error_cost = node
                    .error_cost
                    .wrapping_add(ch.error_cost.wrapping_sub(extent_cost(ch.size)));
            } else {
                node.error_cost = node.error_cost.wrapping_add(ch.cost());
                if is_err_parent && !ch.extra() && !(ch.is_error() && gc == 0) {
                    if ch.visible() {
                        node.error_cost += ERROR_COST_PER_SKIPPED_TREE;
                    } else if gc > 0 {
                        node.error_cost += ERROR_COST_PER_SKIPPED_TREE * ch.visible_children;
                    }
                }
            }
            node.dprec += ch.dprec;
            if ch.ext != NONE {
                node.ext = ch.ext;
            }
            node.visible_descendants += ch.visible_descendants;
            let aliased = !ch.extra()
                && ch.sym != 0
                && alias.is_some_and(|a| a.get(structural).copied().unwrap_or(0) != 0);
            if aliased {
                node.visible_descendants += 1;
                node.visible_children += 1;
                if lang.named(alias.unwrap()[structural]) {
                    node.named_children += 1;
                }
            } else if ch.visible() {
                node.visible_descendants += 1;
                node.visible_children += 1;
                if ch.named() {
                    node.named_children += 1;
                }
            } else if gc > 0 {
                node.visible_children += ch.visible_children;
                node.named_children += ch.named_children;
            }
            if !ch.extra() {
                structural += 1;
            }
        }
        if is_err_parent {
            node.error_cost = node.error_cost.wrapping_add(extent_cost(node.size));
        }
        self.push(node)
    }

    pub fn new_error_node(&mut self, lang: &Language, children: &[u32], extra: bool) -> u32 {
        let i = self.new_node(lang, SYM_ERROR, children, 0);
        if extra {
            self.n_mut(i).flags |= F_EXTRA;
        }
        i
    }

    /// 官方 `ts_subtree_compare`：按符号、子节点数逐个比较，返回 -1/0/1。
    pub fn compare(&self, a: u32, b: u32) -> i32 {
        let mut work = vec![(a, b)];
        while let Some((l, r)) = work.pop() {
            let (ln, rn) = (self.n(l), self.n(r));
            let res = if ln.sym < rn.sym {
                -1
            } else if rn.sym < ln.sym {
                1
            } else if ln.child_count < rn.child_count {
                -1
            } else if rn.child_count < ln.child_count {
                1
            } else {
                0
            };
            if res != 0 {
                return res;
            }
            let (lc, rc) = (self.children(l), self.children(r));
            for i in (0..lc.len()).rev() {
                work.push((lc[i], rc[i]));
            }
        }
        0
    }
}
