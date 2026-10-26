//! 文档树：所有节点放在一个 `Vec` 里，用 u32 下标互相引用；文本和属性值尽量直接指向源字符串。

use std::borrow::Cow;

use crate::Tag;

pub(crate) const NONE: u32 = u32::MAX;
/// [`Span`] 起点的最高位：为 1 时指向解码缓冲区，否则指向源字符串。
const OWNED: u32 = 1 << 31;

/// 一段字符串：源 HTML 里的一段，或者解码缓冲区里的一段（含字符引用、需要改写的内容）。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub(crate) struct Span {
    start: u32,
    len: u32,
}

impl Span {
    pub(crate) fn source(start: usize, end: usize) -> Span {
        Span {
            start: start as u32,
            len: (end - start) as u32,
        }
    }

    pub(crate) fn owned(start: usize, end: usize) -> Span {
        Span {
            start: start as u32 | OWNED,
            len: (end - start) as u32,
        }
    }

    pub(crate) fn is_owned(self) -> bool {
        self.start & OWNED != 0
    }

    pub(crate) fn offset(self) -> usize {
        (self.start & !OWNED) as usize
    }

    pub(crate) fn end(self) -> usize {
        self.offset() + self.len as usize
    }

    pub(crate) fn is_empty(self) -> bool {
        self.len == 0
    }

    pub(crate) fn skip(self, n: usize) -> Span {
        Span {
            start: self.start + n as u32,
            len: self.len - n as u32,
        }
    }
}

#[derive(Debug, Clone, Copy)]
pub(crate) struct Attr {
    pub(crate) name: Span,
    pub(crate) value: Span,
}

/// 元素的命名空间（规范的 HTML、SVG、MathML 三种）。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum Namespace {
    #[default]
    Html,
    Svg,
    MathMl,
}

#[derive(Debug, Clone, Copy)]
pub(crate) enum Data {
    Document,
    Doctype,
    Element {
        tag: Tag,
        ns: Namespace,
        /// 只有 [`Tag::Other`] 才有名字；已知标签的名字来自 [`Tag::name`]。
        name: Span,
        attrs: u32,
        attrs_len: u32,
    },
    Text(Span),
    Comment(Span),
}

#[derive(Debug, Clone, Copy)]
pub(crate) struct Node {
    pub(crate) parent: u32,
    pub(crate) first_child: u32,
    pub(crate) last_child: u32,
    pub(crate) prev: u32,
    pub(crate) next: u32,
    pub(crate) data: Data,
}

impl Node {
    pub(crate) fn new(data: Data) -> Node {
        Node {
            parent: NONE,
            first_child: NONE,
            last_child: NONE,
            prev: NONE,
            next: NONE,
            data,
        }
    }
}

/// 解析好的文档。借用源字符串（输入含 `\r` 或 NUL 时持有规范化后的副本）。
pub struct Html<'s> {
    pub(crate) src: Cow<'s, str>,
    pub(crate) buf: String,
    pub(crate) nodes: Vec<Node>,
    pub(crate) attrs: Vec<Attr>,
}

/// Reusable allocation storage for parsing consecutive documents on one
/// worker. The parsed [`Html`] owns the buffers while it is in use; call
/// [`ParseScratch::recycle`] after all node references have been dropped.
#[derive(Default)]
pub struct ParseScratch {
    pub(crate) buf: String,
    pub(crate) nodes: Vec<Node>,
    pub(crate) attrs: Vec<Attr>,
}

impl ParseScratch {
    /// Return a completed document's backing allocations to this scratch
    /// object. Source HTML is never retained.
    pub fn recycle(&mut self, mut html: Html<'_>) {
        html.buf.clear();
        html.nodes.clear();
        html.attrs.clear();
        self.buf = if html.buf.capacity() <= 1024 * 1024 {
            html.buf
        } else {
            String::new()
        };
        self.nodes = if html.nodes.capacity() <= 16 * 1024 {
            html.nodes
        } else {
            Vec::new()
        };
        self.attrs = if html.attrs.capacity() <= 32 * 1024 {
            html.attrs
        } else {
            Vec::new()
        };
    }
}

impl<'s> Html<'s> {
    /// 解析整个 HTML 文档。永不失败：任何输入都会得到一棵树。
    pub fn parse(html: &'s str) -> Html<'s> {
        crate::builder::parse(html)
    }

    /// Parse while reusing node, attribute, and decoded-text allocations from
    /// the preceding document handled by this worker.
    pub fn parse_with_scratch(html: &'s str, scratch: &mut ParseScratch) -> Html<'s> {
        crate::builder::parse_with_scratch(html, scratch)
    }

    /// 文档根节点（不是 `<html>` 元素，是它的父节点）。
    pub fn root(&self) -> NodeRef<'_> {
        NodeRef { doc: self, id: 0 }
    }

    /// 节点总数。节点 id 的范围是 `0..len()`，可以用来开按节点下标索引的数组。
    pub fn len(&self) -> usize {
        self.nodes.len()
    }

    pub fn is_empty(&self) -> bool {
        self.nodes.len() <= 1
    }

    pub fn get(&self, id: NodeId) -> NodeRef<'_> {
        NodeRef {
            doc: self,
            id: id.0,
        }
    }

    /// 第一个 `<body>`，没有时返回根节点。
    pub fn body(&self) -> NodeRef<'_> {
        self.root()
            .descendants()
            .find(|n| n.tag() == Some(Tag::Body))
            .unwrap_or_else(|| self.root())
    }

    pub(crate) fn str(&self, s: Span) -> &str {
        let (start, end) = (s.offset(), s.end());
        if s.is_owned() {
            &self.buf[start..end]
        } else {
            &self.src[start..end]
        }
    }
}

impl std::fmt::Debug for Html<'_> {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Html")
            .field("nodes", &self.nodes.len())
            .finish()
    }
}

/// 节点 id，在同一个文档里唯一，取值 `0..Html::len()`。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub struct NodeId(u32);

impl NodeId {
    pub fn index(self) -> usize {
        self.0 as usize
    }
}

/// 节点的引用。可以复制，比较时比的是节点身份。
#[derive(Clone, Copy)]
pub struct NodeRef<'a> {
    doc: &'a Html<'a>,
    id: u32,
}

impl PartialEq for NodeRef<'_> {
    fn eq(&self, other: &Self) -> bool {
        self.id == other.id && std::ptr::eq(self.doc, other.doc)
    }
}

impl Eq for NodeRef<'_> {}

impl std::fmt::Debug for NodeRef<'_> {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self.node().data {
            Data::Document => write!(f, "#document"),
            Data::Doctype => write!(f, "<!DOCTYPE>"),
            Data::Element { .. } => write!(f, "<{}>", self.as_element().unwrap().name()),
            Data::Text(s) => write!(f, "{:?}", self.doc.str(s)),
            Data::Comment(s) => write!(f, "<!--{}-->", self.doc.str(s)),
        }
    }
}

impl<'a> NodeRef<'a> {
    fn node(&self) -> &'a Node {
        &self.doc.nodes[self.id as usize]
    }

    fn at(&self, id: u32) -> Option<NodeRef<'a>> {
        (id != NONE).then_some(NodeRef { doc: self.doc, id })
    }

    pub fn id(&self) -> NodeId {
        NodeId(self.id)
    }

    pub fn document(&self) -> &'a Html<'a> {
        self.doc
    }

    pub fn parent(&self) -> Option<NodeRef<'a>> {
        self.at(self.node().parent)
    }

    pub fn first_child(&self) -> Option<NodeRef<'a>> {
        self.at(self.node().first_child)
    }

    pub fn last_child(&self) -> Option<NodeRef<'a>> {
        self.at(self.node().last_child)
    }

    pub fn next_sibling(&self) -> Option<NodeRef<'a>> {
        self.at(self.node().next)
    }

    pub fn prev_sibling(&self) -> Option<NodeRef<'a>> {
        self.at(self.node().prev)
    }

    pub fn children(&self) -> Children<'a> {
        Children {
            doc: self.doc,
            front: self.node().first_child,
            back: self.node().last_child,
        }
    }

    /// 自身加所有后代，先序（文档顺序）。
    pub fn descendants(&self) -> Descendants<'a> {
        Descendants {
            doc: self.doc,
            root: self.id,
            next: self.id,
        }
    }

    /// 父节点、祖父节点……直到根，不含自身。
    pub fn ancestors(&self) -> Ancestors<'a> {
        Ancestors {
            doc: self.doc,
            next: self.node().parent,
        }
    }

    pub fn is_element(&self) -> bool {
        matches!(self.node().data, Data::Element { .. })
    }

    pub fn is_text(&self) -> bool {
        matches!(self.node().data, Data::Text(_))
    }

    pub fn is_comment(&self) -> bool {
        matches!(self.node().data, Data::Comment(_))
    }

    pub fn is_document(&self) -> bool {
        matches!(self.node().data, Data::Document)
    }

    pub fn as_element(&self) -> Option<Element<'a>> {
        match self.node().data {
            Data::Element {
                tag,
                ns,
                name,
                attrs,
                attrs_len,
            } => Some(Element {
                doc: self.doc,
                tag,
                ns,
                name,
                attrs: &self.doc.attrs[attrs as usize..(attrs + attrs_len) as usize],
            }),
            _ => None,
        }
    }

    /// 元素的标签；不是元素时为 `None`。
    pub fn tag(&self) -> Option<Tag> {
        match self.node().data {
            Data::Element { tag, .. } => Some(tag),
            _ => None,
        }
    }

    pub fn as_text(&self) -> Option<&'a str> {
        match self.node().data {
            Data::Text(s) => Some(self.doc.str(s)),
            _ => None,
        }
    }

    pub fn as_comment(&self) -> Option<&'a str> {
        match self.node().data {
            Data::Comment(s) => Some(self.doc.str(s)),
            _ => None,
        }
    }

    /// 子树里所有文本节点按顺序拼起来。
    pub fn text(&self) -> String {
        let mut out = String::new();
        for d in self.descendants() {
            if let Some(t) = d.as_text() {
                out.push_str(t);
            }
        }
        out
    }
}

/// 元素：标签、名字和属性。
#[derive(Clone, Copy)]
pub struct Element<'a> {
    doc: &'a Html<'a>,
    tag: Tag,
    ns: Namespace,
    name: Span,
    attrs: &'a [Attr],
}

impl<'a> Element<'a> {
    /// 元素的命名空间：`<svg>`、`<math>` 和它们里面的外部元素不是 HTML。
    pub fn namespace(&self) -> Namespace {
        self.ns
    }

    pub fn tag(&self) -> Tag {
        self.tag
    }

    /// 小写标签名。
    pub fn name(&self) -> &'a str {
        match self.tag {
            Tag::Other => self.doc.str(self.name),
            t => t.name(),
        }
    }

    /// 属性值，`name` 用小写。同名属性只保留第一个（规范如此）。
    pub fn attr(&self, name: &str) -> Option<&'a str> {
        self.attrs
            .iter()
            .find(|a| a.name.len as usize == name.len() && self.doc.str(a.name) == name)
            .map(|a| self.doc.str(a.value))
    }

    pub fn attrs(&self) -> impl Iterator<Item = (&'a str, &'a str)> + 'a {
        let doc = self.doc;
        self.attrs
            .iter()
            .map(move |a| (doc.str(a.name), doc.str(a.value)))
    }
}

impl std::fmt::Debug for Element<'_> {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "<{}", self.name())?;
        for (k, v) in self.attrs() {
            write!(f, " {k}={v:?}")?;
        }
        write!(f, ">")
    }
}

pub struct Children<'a> {
    doc: &'a Html<'a>,
    front: u32,
    back: u32,
}

impl<'a> Iterator for Children<'a> {
    type Item = NodeRef<'a>;
    fn next(&mut self) -> Option<NodeRef<'a>> {
        if self.front == NONE {
            return None;
        }
        let id = self.front;
        if id == self.back {
            self.front = NONE;
            self.back = NONE;
        } else {
            self.front = self.doc.nodes[id as usize].next;
        }
        Some(NodeRef { doc: self.doc, id })
    }
}

impl DoubleEndedIterator for Children<'_> {
    fn next_back(&mut self) -> Option<Self::Item> {
        if self.back == NONE {
            return None;
        }
        let id = self.back;
        if id == self.front {
            self.front = NONE;
            self.back = NONE;
        } else {
            self.back = self.doc.nodes[id as usize].prev;
        }
        Some(NodeRef { doc: self.doc, id })
    }
}

pub struct Descendants<'a> {
    doc: &'a Html<'a>,
    root: u32,
    next: u32,
}

impl<'a> Iterator for Descendants<'a> {
    type Item = NodeRef<'a>;
    fn next(&mut self) -> Option<NodeRef<'a>> {
        if self.next == NONE {
            return None;
        }
        let id = self.next;
        let nodes = &self.doc.nodes;
        let n = &nodes[id as usize];
        self.next = if n.first_child != NONE {
            n.first_child
        } else {
            // 没有子节点：找自己或最近祖先的下一个兄弟，到子树根为止
            let mut cur = id;
            loop {
                if cur == self.root {
                    break NONE;
                }
                let c = &nodes[cur as usize];
                if c.next != NONE {
                    break c.next;
                }
                cur = c.parent;
                if cur == NONE {
                    break NONE;
                }
            }
        };
        Some(NodeRef { doc: self.doc, id })
    }
}

pub struct Ancestors<'a> {
    doc: &'a Html<'a>,
    next: u32,
}

impl<'a> Iterator for Ancestors<'a> {
    type Item = NodeRef<'a>;
    fn next(&mut self) -> Option<NodeRef<'a>> {
        if self.next == NONE {
            return None;
        }
        let id = self.next;
        self.next = self.doc.nodes[id as usize].parent;
        Some(NodeRef { doc: self.doc, id })
    }
}
