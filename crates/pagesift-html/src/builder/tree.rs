use std::collections::HashSet;

use crate::Tag;
use crate::dom::{Attr, Data, NONE, Namespace, Node, Span};
use crate::entity::decode_into;

use super::{Builder, MAX_DEPTH, Ns, Open, Scope, is_ws, marker, table_context, void};

/// 属性去重时线性比较的上限。真实页面里几乎所有元素的属性都不超过这个数，逐个比较名字比建
/// 哈希表快；超过后一次性把已有名字放进哈希集合，避免一个标签挂几万个属性时退化成平方复杂度
/// （4 万个属性曾要 3 秒，可被恶意页面拿来拖垮解析）。
const LINEAR_DEDUP_MAX: usize = 16;

impl<'a> Builder<'a> {
    // ---- 节点操作

    pub(super) fn new_node(&mut self, data: Data) -> u32 {
        let id = self.nodes.len() as u32;
        self.nodes.push(Node::new(data));
        id
    }

    pub(super) fn append(&mut self, parent: u32, child: u32) {
        let last = self.nodes[parent as usize].last_child;
        {
            let c = &mut self.nodes[child as usize];
            c.parent = parent;
            c.prev = last;
        }
        if last == NONE {
            self.nodes[parent as usize].first_child = child;
        } else {
            self.nodes[last as usize].next = child;
        }
        self.nodes[parent as usize].last_child = child;
    }

    pub(super) fn insert_before(&mut self, parent: u32, child: u32, before: u32) {
        let prev = self.nodes[before as usize].prev;
        {
            let c = &mut self.nodes[child as usize];
            c.parent = parent;
            c.prev = prev;
            c.next = before;
        }
        self.nodes[before as usize].prev = child;
        if prev == NONE {
            self.nodes[parent as usize].first_child = child;
        } else {
            self.nodes[prev as usize].next = child;
        }
    }

    /// 插入位置：当前节点；表格上下文里的非表格内容放到最近那个表格的前面。
    pub(super) fn place(&mut self, child: u32, foster: bool) {
        let Some(cur) = self.current() else {
            let parent = if self.html != NONE { self.html } else { 0 };
            self.append(parent, child);
            return;
        };
        if foster && cur.ns == Ns::Html && table_context(cur.tag) {
            self.foster(child, self.stack.len());
        } else {
            self.append(cur.id, child);
        }
    }

    /// Foster parenting：放到栈里 `stack[..below]` 范围内最后一个表格的前面。
    pub(super) fn foster(&mut self, child: u32, below: usize) {
        let Some(i) = self.stack[..below]
            .iter()
            .rposition(|o| o.tag == Tag::Table && o.ns == Ns::Html)
        else {
            let parent = self.stack.first().map_or(0, |o| o.id);
            self.append(parent, child);
            return;
        };
        let table = self.stack[i].id;
        let parent = self.nodes[table as usize].parent;
        if parent != NONE {
            self.insert_before(parent, child, table);
        } else {
            let below = self.stack[i.saturating_sub(1)].id;
            self.append(below, child);
        }
    }

    /// 文本插到 `parent` 的最后（或 `before` 之前），和相邻的文本节点合并。
    pub(super) fn add_text(&mut self, span: Span, foster: bool) {
        // 先确定落点和落点前面的兄弟
        let (parent, before) = match self.current() {
            None => (if self.html != NONE { self.html } else { 0 }, NONE),
            Some(cur) => {
                let fostered = foster
                    && cur.ns == Ns::Html
                    && table_context(cur.tag)
                    && !self.str(span).bytes().all(is_ws);
                match fostered
                    .then(|| {
                        self.stack
                            .iter()
                            .rposition(|o| o.tag == Tag::Table && o.ns == Ns::Html)
                    })
                    .flatten()
                {
                    Some(i) => {
                        let table = self.stack[i].id;
                        let p = self.nodes[table as usize].parent;
                        if p != NONE {
                            (p, table)
                        } else {
                            (self.stack[i.saturating_sub(1)].id, NONE)
                        }
                    }
                    None => (cur.id, NONE),
                }
            }
        };
        let prev = if before == NONE {
            self.nodes[parent as usize].last_child
        } else {
            self.nodes[before as usize].prev
        };
        if prev != NONE
            && let Data::Text(old) = self.nodes[prev as usize].data
        {
            let merged = self.concat(old, span);
            self.nodes[prev as usize].data = Data::Text(merged);
            return;
        }
        let id = self.new_node(Data::Text(span));
        if before == NONE {
            self.append(parent, id);
        } else {
            self.insert_before(parent, id, before);
        }
    }

    pub(super) fn concat(&mut self, a: Span, b: Span) -> Span {
        if !a.is_owned() && !b.is_owned() && a.end() == b.offset() {
            return Span::source(a.offset(), b.end());
        }
        let start = if a.is_owned() && a.end() == self.buf.len() {
            a.offset()
        } else {
            let s = self.str(a).to_string();
            let start = self.buf.len();
            self.buf.push_str(&s);
            start
        };
        let s = self.str(b).to_string();
        self.buf.push_str(&s);
        Span::owned(start, self.buf.len())
    }

    /// 名字小写化：源里已经是小写就直接引用，否则写进缓冲区。
    pub(super) fn lower_span(&mut self, (s, e): (usize, usize)) -> Span {
        let raw = &self.src[s..e];
        if !raw.bytes().any(|c| c.is_ascii_uppercase()) {
            return Span::source(s, e);
        }
        let start = self.buf.len();
        for c in raw.chars() {
            self.buf.push(c.to_ascii_lowercase());
        }
        Span::owned(start, self.buf.len())
    }

    pub(super) fn value_span(&mut self, (s, e): (usize, usize)) -> Span {
        let raw = &self.src[s..e];
        if memchr::memchr(b'&', raw.as_bytes()).is_none() {
            return Span::source(s, e);
        }
        let start = self.buf.len();
        decode_into(&mut self.buf, raw, true);
        Span::owned(start, self.buf.len())
    }

    /// 把分词器暂存的属性写进属性表，同名的只留第一个。
    pub(super) fn take_attrs(&mut self) -> (u32, u32) {
        let start = self.attrs.len() as u32;
        let scratch = std::mem::take(&mut self.scratch);
        // 属性多于 LINEAR_DEDUP_MAX 时才建，之后都查它
        let mut seen: Option<HashSet<String>> = None;
        for &(n, v) in &scratch {
            let name = self.lower_span(n);
            let dup = match &mut seen {
                Some(set) => !set.insert(self.str(name).to_owned()),
                None => {
                    let kept = &self.attrs[start as usize..];
                    let dup = kept.iter().any(|a| self.str(a.name) == self.str(name));
                    if !dup && kept.len() >= LINEAR_DEDUP_MAX {
                        let mut set: HashSet<String> =
                            kept.iter().map(|a| self.str(a.name).to_owned()).collect();
                        set.insert(self.str(name).to_owned());
                        seen = Some(set);
                    }
                    dup
                }
            };
            if dup {
                continue;
            }
            let value = self.value_span(v);
            self.attrs.push(Attr { name, value });
        }
        self.scratch = scratch;
        (start, self.attrs.len() as u32 - start)
    }

    pub(super) fn create(&mut self, tag: Tag, name: (usize, usize)) -> u32 {
        self.create_ns(tag, name, Namespace::Html)
    }

    pub(super) fn create_ns(&mut self, tag: Tag, name: (usize, usize), ns: Namespace) -> u32 {
        let name = if tag == Tag::Other {
            self.lower_span(name)
        } else {
            Span::default()
        };
        let (attrs, attrs_len) = self.take_attrs();
        self.new_node(Data::Element {
            tag,
            ns,
            name,
            attrs,
            attrs_len,
        })
    }

    /// 创建元素、放进树里，需要时入栈。
    pub(super) fn insert(
        &mut self,
        tag: Tag,
        name: (usize, usize),
        ns: Ns,
        push: bool,
        foster: bool,
    ) -> u32 {
        let id = self.create(tag, name);
        self.place(id, foster);
        if push && self.stack.len() < MAX_DEPTH {
            self.stack.push(Open { id, tag, ns });
            if ns == Ns::Html && marker(tag) {
                self.afe.push(NONE);
            }
        }
        id
    }

    pub(super) fn insert_html(&mut self, tag: Tag, name: (usize, usize)) {
        let push = !void(tag);
        // 表格里只有 `type=hidden` 的 input 留在表格里，别的和普通内容一样挪到表格前面
        let foster = !matches!(tag, Tag::Script | Tag::Style | Tag::Template | Tag::Form)
            && !(tag == Tag::Input && self.hidden_input());
        self.insert(tag, name, Ns::Html, push, foster);
    }

    /// 分词器暂存的属性里有 `type=hidden`（不分大小写，不去空白）。
    pub(super) fn hidden_input(&self) -> bool {
        self.scratch.iter().any(|&((ns, ne), (vs, ve))| {
            self.src[ns..ne].eq_ignore_ascii_case("type")
                && self.src[vs..ve].eq_ignore_ascii_case("hidden")
        })
    }

    /// 重复出现的 `<html>`、`<body>`：把还没有的属性补到已有元素上。
    pub(super) fn merge_attrs(&mut self, target: u32) {
        if target == NONE {
            return;
        }
        let Data::Element {
            tag,
            ns,
            name,
            attrs,
            attrs_len,
        } = self.nodes[target as usize].data
        else {
            return;
        };
        let (new_start, new_len) = self.take_attrs();
        if new_len == 0 {
            return;
        }
        let old: Vec<Attr> = self.attrs[attrs as usize..(attrs + attrs_len) as usize].to_vec();
        // 新旧两边都可能很长，旧属性多时同样换成哈希集合，避免平方复杂度
        let old_names: Option<HashSet<&str>> =
            (old.len() > LINEAR_DEDUP_MAX).then(|| old.iter().map(|o| self.str(o.name)).collect());
        let added: Vec<Attr> = self.attrs[new_start as usize..]
            .iter()
            .copied()
            .filter(|a| match &old_names {
                Some(set) => !set.contains(self.str(a.name)),
                None => !old.iter().any(|o| self.str(o.name) == self.str(a.name)),
            })
            .collect();
        self.attrs.truncate(new_start as usize);
        if added.is_empty() {
            return;
        }
        let start = self.attrs.len() as u32;
        self.attrs.extend(old);
        self.attrs.extend(added);
        self.nodes[target as usize].data = Data::Element {
            tag,
            ns,
            name,
            attrs: start,
            attrs_len: self.attrs.len() as u32 - start,
        };
    }

    // ---- 栈操作

    pub(super) fn in_scope(&self, target: Tag, scope: Scope) -> Option<usize> {
        self.find_in_scope(|t| t == target, scope)
    }

    pub(super) fn find_in_scope(&self, pred: impl Fn(Tag) -> bool, scope: Scope) -> Option<usize> {
        for (i, &o) in self.stack.iter().enumerate().rev() {
            if o.ns == Ns::Html && pred(o.tag) {
                return Some(i);
            }
            if self.boundary(o, scope) {
                return None;
            }
        }
        None
    }

    pub(super) fn boundary(&self, o: Open, scope: Scope) -> bool {
        use Tag::*;
        match o.ns {
            // 表格作用域的边界只有 html、table、template，外部内容的集成点挡不住
            Ns::Integration | Ns::Annotation => scope != Scope::Table,
            Ns::Foreign => false,
            Ns::Html => match scope {
                Scope::Table => matches!(o.tag, Html | Table | Template),
                _ => {
                    matches!(
                        o.tag,
                        Applet | Caption | Html | Table | Td | Th | Marquee | Object | Template
                    ) || (scope == Scope::ListItem && matches!(o.tag, Ol | Ul))
                        || (scope == Scope::Button && o.tag == Button)
                }
            },
        }
    }

    pub(super) fn pop_to(&mut self, i: usize) {
        for k in i..self.stack.len() {
            let o = self.stack[k];
            if o.ns == Ns::Html && marker(o.tag) {
                while let Some(e) = self.afe.pop() {
                    if e == NONE {
                        break;
                    }
                }
            }
        }
        self.stack.truncate(i);
    }

    pub(super) fn pop(&mut self) {
        if let Some(n) = self.stack.len().checked_sub(1) {
            self.pop_to(n);
        }
    }

    pub(super) fn on_stack(&self, id: u32) -> Option<usize> {
        self.stack.iter().rposition(|o| o.id == id)
    }

    pub(super) fn node_tag(&self, id: u32) -> Tag {
        match self.nodes[id as usize].data {
            Data::Element { tag, .. } => tag,
            _ => Tag::Other,
        }
    }

    pub(super) fn node_ns(&self, id: u32) -> Namespace {
        match self.nodes[id as usize].data {
            Data::Element { ns, .. } => ns,
            _ => Namespace::Html,
        }
    }

    pub(super) fn clone_element(&mut self, id: u32) -> u32 {
        let data = self.nodes[id as usize].data;
        self.new_node(data)
    }

    pub(super) fn detach(&mut self, id: u32) {
        let Node {
            parent, prev, next, ..
        } = self.nodes[id as usize];
        if parent == NONE {
            return;
        }
        if prev == NONE {
            self.nodes[parent as usize].first_child = next;
        } else {
            self.nodes[prev as usize].next = next;
        }
        if next == NONE {
            self.nodes[parent as usize].last_child = prev;
        } else {
            self.nodes[next as usize].prev = prev;
        }
        let n = &mut self.nodes[id as usize];
        n.parent = NONE;
        n.prev = NONE;
        n.next = NONE;
    }

    pub(super) fn move_children(&mut self, from: u32, to: u32) {
        let mut c = self.nodes[from as usize].first_child;
        while c != NONE {
            let next = self.nodes[c as usize].next;
            self.detach(c);
            self.append(to, c);
            c = next;
        }
    }

    /// 两个元素标签、名字和属性都相同（Noah's Ark 规则用）。
    pub(super) fn same_element(&self, a: u32, b: u32) -> bool {
        let (
            Data::Element {
                tag: ta,
                ns: sa,
                name: na,
                attrs: aa,
                attrs_len: la,
            },
            Data::Element {
                tag: tb,
                ns: sb,
                name: nb,
                attrs: ab,
                attrs_len: lb,
            },
        ) = (self.nodes[a as usize].data, self.nodes[b as usize].data)
        else {
            return false;
        };
        if ta != tb || sa != sb || la != lb || self.str(na) != self.str(nb) {
            return false;
        }
        let xs = &self.attrs[aa as usize..(aa + la) as usize];
        let ys = &self.attrs[ab as usize..(ab + lb) as usize];
        xs.iter().all(|x| {
            ys.iter().any(|y| {
                self.str(x.name) == self.str(y.name) && self.str(x.value) == self.str(y.value)
            })
        })
    }
}
