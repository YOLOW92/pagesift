//! 分词和建树合在一起：分词器用 memchr 在源字符串里跳着找 `<`，直接把标签、文本交给建树器，
//! 中间不产生 token 对象。建树器实现 HTML5 树构建规则里影响文本结构的部分：
//! 隐式的 html/head/body、`<p>` `<li>` `<dd>` `<option>` 等的自动闭合、作用域边界、
//! 表格的隐式 tbody/tr 和 foster parenting、SVG/MathML 外部内容和跳出规则、
//! raw text 和 RCDATA 元素、`<pre>` 开头的换行。
//! 没实现的：adoption agency 重建格式元素（`<b>` 跨段落时后一段不再加粗）、quirks 模式、
//! `<template>` 的独立文档片段（内容作为普通子节点）。

use std::borrow::Cow;

use memchr::memmem;

use crate::Tag;
use crate::dom::{Attr, Data, Html, NONE, Node, Span};
use crate::entity::decode_into;

/// 打开元素栈的最大深度。更深的元素照样插入但不入栈，树的深度因此有上限，
/// 递归遍历这棵树的代码不会因为恶意的深层嵌套栈溢出。
const MAX_DEPTH: usize = 512;

pub(crate) fn parse(html: &str) -> Html<'_> {
    let src = normalize(html);
    let (buf, nodes, attrs) = {
        let mut tb = Builder::new(&src);
        tokenize(&src, &mut tb);
        (tb.buf, tb.nodes, tb.attrs)
    };
    Html {
        src,
        buf,
        nodes,
        attrs,
    }
}

/// 去掉 BOM，`\r\n` 和单独的 `\r` 变成 `\n`，丢掉 NUL（正文里的 NUL 规范上本来就被忽略）。
fn normalize(html: &str) -> Cow<'_, str> {
    let html = html.strip_prefix('\u{feff}').unwrap_or(html);
    let bytes = html.as_bytes();
    let Some(first) = memchr::memchr2(b'\r', b'\0', bytes) else {
        return Cow::Borrowed(html);
    };
    let mut out = String::with_capacity(html.len());
    let mut last = 0;
    let mut i = first;
    loop {
        out.push_str(&html[last..i]);
        if bytes[i] == b'\r' {
            out.push('\n');
            if bytes.get(i + 1) == Some(&b'\n') {
                i += 1;
            }
        }
        last = i + 1;
        match memchr::memchr2(b'\r', b'\0', &bytes[last..]) {
            Some(off) => i = last + off,
            None => break,
        }
    }
    out.push_str(&html[last..]);
    Cow::Owned(out)
}

// ---------------------------------------------------------------- 分词

/// 开始标签之后分词器要切换到的状态。
#[derive(Clone, Copy, PartialEq, Eq)]
enum Next {
    Data,
    /// `<style>`、`<script>` 等：内容原样作为文本，直到对应的结束标签。
    RawText,
    /// `<title>`、`<textarea>`：同上，但解码字符引用。
    RcData,
    /// `<plaintext>`：剩下的全部是文本。
    PlainText,
}

fn is_ws(c: u8) -> bool {
    matches!(c, b'\t' | b'\n' | b'\x0c' | b' ' | b'\r')
}

/// 分词器找到的属性：名字和值在源字符串里的范围。
type RawAttr = ((usize, usize), (usize, usize));

struct TagToken {
    name: (usize, usize),
    self_closing: bool,
    end: usize,
}

fn tokenize(src: &str, tb: &mut Builder<'_>) {
    let b = src.as_bytes();
    let mut pos = 0;
    let mut text_start = 0;
    while let Some(off) = memchr::memchr(b'<', &b[pos..]) {
        let lt = pos + off;
        match b.get(lt + 1) {
            Some(c) if c.is_ascii_alphabetic() => {
                tb.text_range(text_start, lt, true);
                let Some(t) = scan_tag(b, lt + 1, &mut tb.scratch) else {
                    return;
                };
                let next = tb.start_tag(t.name, t.self_closing);
                pos = t.end;
                if next != Next::Data {
                    let name = src[t.name.0..t.name.1].to_ascii_lowercase();
                    pos = raw_text(src, pos, &name, next, tb);
                }
                text_start = pos;
            }
            Some(b'/') => match b.get(lt + 2) {
                Some(c) if c.is_ascii_alphabetic() => {
                    tb.text_range(text_start, lt, true);
                    let Some(t) = scan_tag(b, lt + 2, &mut tb.scratch) else {
                        return;
                    };
                    tb.end_tag(t.name);
                    pos = t.end;
                    text_start = pos;
                }
                Some(b'>') => {
                    tb.text_range(text_start, lt, true);
                    pos = lt + 3;
                    text_start = pos;
                }
                None => break,
                Some(_) => {
                    tb.text_range(text_start, lt, true);
                    pos = bogus_comment(b, lt + 2, tb);
                    text_start = pos;
                }
            },
            Some(b'!') => {
                tb.text_range(text_start, lt, true);
                pos = markup_declaration(b, lt + 2, tb);
                text_start = pos;
            }
            Some(b'?') => {
                tb.text_range(text_start, lt, true);
                pos = bogus_comment(b, lt + 1, tb);
                text_start = pos;
            }
            // 不是标签的 `<`（比如 "a < b"）留在文本里
            _ => pos = lt + 1,
        }
    }
    tb.text_range(text_start, b.len(), true);
    tb.finish();
}

/// 从标签名的第一个字符开始扫描一个标签，属性放进 `attrs`。到文件末尾还没结束的标签按规范丢弃。
fn scan_tag(b: &[u8], mut i: usize, attrs: &mut Vec<RawAttr>) -> Option<TagToken> {
    let name_start = i;
    while i < b.len() && !is_ws(b[i]) && b[i] != b'/' && b[i] != b'>' {
        i += 1;
    }
    let name = (name_start, i);
    attrs.clear();
    loop {
        while i < b.len() && is_ws(b[i]) {
            i += 1;
        }
        match b.get(i)? {
            b'>' => {
                return Some(TagToken {
                    name,
                    self_closing: false,
                    end: i + 1,
                });
            }
            b'/' => {
                i += 1;
                if b.get(i) == Some(&b'>') {
                    return Some(TagToken {
                        name,
                        self_closing: true,
                        end: i + 1,
                    });
                }
                continue;
            }
            _ => {}
        }
        // 属性名的第一个字符可以是 `=`
        let an_start = i;
        i += 1;
        while i < b.len() && !is_ws(b[i]) && !matches!(b[i], b'/' | b'>' | b'=') {
            i += 1;
        }
        let an = (an_start, i);
        while i < b.len() && is_ws(b[i]) {
            i += 1;
        }
        let mut value = (i, i);
        if b.get(i) == Some(&b'=') {
            i += 1;
            while i < b.len() && is_ws(b[i]) {
                i += 1;
            }
            match *b.get(i)? {
                q @ (b'"' | b'\'') => {
                    let vs = i + 1;
                    let ve = vs + memchr::memchr(q, &b[vs..])?;
                    value = (vs, ve);
                    i = ve + 1;
                }
                b'>' => value = (i, i),
                _ => {
                    let vs = i;
                    while i < b.len() && !is_ws(b[i]) && b[i] != b'>' {
                        i += 1;
                    }
                    value = (vs, i);
                }
            }
        }
        attrs.push((an, value));
    }
}

/// raw text / RCDATA 元素的内容：找到对应的结束标签为止。返回结束标签之后的位置。
fn raw_text(src: &str, start: usize, name: &str, next: Next, tb: &mut Builder<'_>) -> usize {
    let b = src.as_bytes();
    if next == Next::PlainText {
        tb.raw_text(start, b.len(), false);
        return b.len();
    }
    let finder = memmem::Finder::new(b"</");
    let mut i = start;
    while let Some(off) = finder.find(&b[i..]) {
        let lt = i + off;
        let ns = lt + 2;
        let ne = ns + name.len();
        if ne < b.len()
            && b[ns..ne].eq_ignore_ascii_case(name.as_bytes())
            && (is_ws(b[ne]) || b[ne] == b'/' || b[ne] == b'>')
        {
            tb.raw_text(start, lt, next == Next::RcData);
            let Some(t) = scan_tag(b, ns, &mut tb.scratch) else {
                return b.len();
            };
            tb.end_tag(t.name);
            return t.end;
        }
        i = lt + 2;
    }
    tb.raw_text(start, b.len(), next == Next::RcData);
    b.len()
}

/// `<!` 之后：注释、DOCTYPE、CDATA（只在 SVG/MathML 里）或者伪注释。
fn markup_declaration(b: &[u8], i: usize, tb: &mut Builder<'_>) -> usize {
    let rest = &b[i..];
    if rest.starts_with(b"--") {
        let start = i + 2;
        if b.get(start) == Some(&b'>') {
            tb.comment(start, start);
            return start + 1;
        }
        if b[start..].starts_with(b"->") {
            tb.comment(start, start);
            return start + 2;
        }
        return match memmem::find(&b[start..], b"-->") {
            Some(off) => {
                tb.comment(start, start + off);
                start + off + 3
            }
            None => {
                tb.comment(start, b.len());
                b.len()
            }
        };
    }
    if rest.len() >= 7 && rest[..7].eq_ignore_ascii_case(b"doctype") {
        let end = memchr::memchr(b'>', rest).map_or(b.len(), |o| i + o + 1);
        tb.doctype();
        return end;
    }
    if rest.starts_with(b"[CDATA[") && tb.in_foreign() {
        let start = i + 7;
        return match memmem::find(&b[start..], b"]]>") {
            Some(off) => {
                tb.raw_text(start, start + off, false);
                start + off + 3
            }
            None => {
                tb.raw_text(start, b.len(), false);
                b.len()
            }
        };
    }
    bogus_comment(b, i, tb)
}

fn bogus_comment(b: &[u8], start: usize, tb: &mut Builder<'_>) -> usize {
    match memchr::memchr(b'>', &b[start..]) {
        Some(off) => {
            tb.comment(start, start + off);
            start + off + 1
        }
        None => {
            tb.comment(start, b.len());
            b.len()
        }
    }
}

// ---------------------------------------------------------------- 建树

#[derive(Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
enum Mode {
    Initial,
    BeforeHead,
    InHead,
    AfterHead,
    InBody,
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum Ns {
    Html,
    /// SVG / MathML 元素。
    Foreign,
    /// SVG 的 foreignObject、desc、title，MathML 的 mi、mo、mn、ms、mtext、annotation-xml：
    /// 外部内容里重新按 HTML 解析的地方。
    Integration,
}

#[derive(Clone, Copy)]
struct Open {
    id: u32,
    tag: Tag,
    ns: Ns,
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum Scope {
    Default,
    ListItem,
    Button,
    Table,
}

struct Builder<'a> {
    src: &'a str,
    buf: String,
    nodes: Vec<Node>,
    attrs: Vec<Attr>,
    stack: Vec<Open>,
    mode: Mode,
    html: u32,
    head: u32,
    body: u32,
    /// `<pre>`、`<listing>`、`<textarea>` 之后紧跟的一个换行不算内容。
    skip_lf: bool,
    /// 分词器放属性位置的地方，每个标签复用。
    scratch: Vec<RawAttr>,
    lower: String,
    /// 活动格式元素列表（`<b>`、`<a>` 等），[`NONE`] 是标记（单元格、`<object>` 等的边界）。
    afe: Vec<u32>,
    /// 当前打开的 `<form>`；打开时再出现的 `<form>` 被忽略。
    form: u32,
}

fn formatting(tag: Tag) -> bool {
    use Tag::*;
    matches!(
        tag,
        A | B | Big | Code | Em | Font | I | Nobr | S | Small | Strike | Strong | Tt | U
    )
}

/// 入栈时在活动格式元素列表里放标记的元素。
fn marker(tag: Tag) -> bool {
    use Tag::*;
    matches!(
        tag,
        Applet | Caption | Marquee | Object | Td | Th | Template
    )
}

fn void(tag: Tag) -> bool {
    use Tag::*;
    matches!(
        tag,
        Area | Base
            | Basefont
            | Bgsound
            | Br
            | Col
            | Embed
            | Frame
            | Hr
            | Img
            | Input
            | Keygen
            | Link
            | Meta
            | Param
            | Source
            | Track
            | Wbr
    )
}

fn special(tag: Tag) -> bool {
    use Tag::*;
    matches!(
        tag,
        Address
            | Applet
            | Area
            | Article
            | Aside
            | Base
            | Basefont
            | Bgsound
            | Blockquote
            | Body
            | Br
            | Button
            | Caption
            | Center
            | Col
            | Colgroup
            | Dd
            | Details
            | Dir
            | Div
            | Dl
            | Dt
            | Embed
            | Fieldset
            | Figcaption
            | Figure
            | Footer
            | Form
            | Frame
            | Frameset
            | H1
            | H2
            | H3
            | H4
            | H5
            | H6
            | Head
            | Header
            | Hgroup
            | Hr
            | Html
            | Iframe
            | Img
            | Input
            | Keygen
            | Li
            | Link
            | Listing
            | Main
            | Marquee
            | Menu
            | Meta
            | Nav
            | Noembed
            | Noframes
            | Noscript
            | Object
            | Ol
            | P
            | Param
            | Plaintext
            | Pre
            | Script
            | Search
            | Section
            | Select
            | Source
            | Style
            | Summary
            | Table
            | Tbody
            | Td
            | Template
            | Textarea
            | Tfoot
            | Th
            | Thead
            | Title
            | Tr
            | Track
            | Ul
            | Wbr
            | Xmp
    )
}

fn heading(tag: Tag) -> bool {
    matches!(
        tag,
        Tag::H1 | Tag::H2 | Tag::H3 | Tag::H4 | Tag::H5 | Tag::H6
    )
}

/// 当前节点是这些元素时，非表格内容要 foster parenting 到表格前面。
fn table_context(tag: Tag) -> bool {
    matches!(
        tag,
        Tag::Table | Tag::Tbody | Tag::Thead | Tag::Tfoot | Tag::Tr
    )
}

/// 外部内容里遇到这些 HTML 标签时跳出 SVG/MathML。
fn breaks_out_of_foreign(tag: Tag) -> bool {
    use Tag::*;
    matches!(
        tag,
        B | Big
            | Blockquote
            | Body
            | Br
            | Center
            | Code
            | Dd
            | Div
            | Dl
            | Dt
            | Em
            | Embed
            | H1
            | H2
            | H3
            | H4
            | H5
            | H6
            | Head
            | Hr
            | I
            | Img
            | Li
            | Listing
            | Menu
            | Meta
            | Nobr
            | Ol
            | P
            | Pre
            | Ruby
            | S
            | Small
            | Span
            | Strong
            | Strike
            | Sub
            | Sup
            | Table
            | Tt
            | U
            | Ul
            | Var
    )
}

fn integration_point(name: &str) -> bool {
    matches!(
        name,
        "foreignobject" | "desc" | "title" | "mi" | "mo" | "mn" | "ms" | "mtext" | "annotation-xml"
    )
}

impl<'a> Builder<'a> {
    fn new(src: &'a str) -> Self {
        Builder {
            src,
            buf: String::new(),
            nodes: {
                let mut v = Vec::with_capacity(src.len() / 40 + 16);
                v.push(Node::new(Data::Document));
                v
            },
            attrs: Vec::with_capacity(src.len() / 80 + 16),
            stack: Vec::with_capacity(64),
            mode: Mode::Initial,
            html: NONE,
            head: NONE,
            body: NONE,
            skip_lf: false,
            scratch: Vec::new(),
            lower: String::new(),
            afe: Vec::new(),
            form: NONE,
        }
    }

    fn str(&self, s: Span) -> &str {
        if s.is_owned() {
            &self.buf[s.offset()..s.end()]
        } else {
            &self.src[s.offset()..s.end()]
        }
    }

    fn in_foreign(&self) -> bool {
        self.stack.last().is_some_and(|o| o.ns == Ns::Foreign)
    }

    fn current(&self) -> Option<Open> {
        self.stack.last().copied()
    }

    fn current_is(&self, tag: Tag) -> bool {
        self.current()
            .is_some_and(|o| o.tag == tag && o.ns != Ns::Foreign)
    }

    // ---- 节点操作

    fn new_node(&mut self, data: Data) -> u32 {
        let id = self.nodes.len() as u32;
        self.nodes.push(Node::new(data));
        id
    }

    fn append(&mut self, parent: u32, child: u32) {
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

    fn insert_before(&mut self, parent: u32, child: u32, before: u32) {
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
    fn place(&mut self, child: u32, foster: bool) {
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
    fn foster(&mut self, child: u32, below: usize) {
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
    fn add_text(&mut self, span: Span, foster: bool) {
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

    fn concat(&mut self, a: Span, b: Span) -> Span {
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
    fn lower_span(&mut self, (s, e): (usize, usize)) -> Span {
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

    fn value_span(&mut self, (s, e): (usize, usize)) -> Span {
        let raw = &self.src[s..e];
        if memchr::memchr(b'&', raw.as_bytes()).is_none() {
            return Span::source(s, e);
        }
        let start = self.buf.len();
        decode_into(&mut self.buf, raw, true);
        Span::owned(start, self.buf.len())
    }

    /// 把分词器暂存的属性写进属性表，同名的只留第一个。
    fn take_attrs(&mut self) -> (u32, u32) {
        let start = self.attrs.len() as u32;
        let scratch = std::mem::take(&mut self.scratch);
        for &(n, v) in &scratch {
            let name = self.lower_span(n);
            let dup = self.attrs[start as usize..]
                .iter()
                .any(|a| self.str(a.name) == self.str(name));
            if dup {
                continue;
            }
            let value = self.value_span(v);
            self.attrs.push(Attr { name, value });
        }
        self.scratch = scratch;
        (start, self.attrs.len() as u32 - start)
    }

    fn create(&mut self, tag: Tag, name: (usize, usize)) -> u32 {
        let name = if tag == Tag::Other {
            self.lower_span(name)
        } else {
            Span::default()
        };
        let (attrs, attrs_len) = self.take_attrs();
        self.new_node(Data::Element {
            tag,
            name,
            attrs,
            attrs_len,
        })
    }

    /// 创建元素、放进树里，需要时入栈。
    fn insert(&mut self, tag: Tag, name: (usize, usize), ns: Ns, push: bool, foster: bool) -> u32 {
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

    fn insert_html(&mut self, tag: Tag, name: (usize, usize)) {
        let push = !void(tag);
        let foster = !matches!(
            tag,
            Tag::Script | Tag::Style | Tag::Template | Tag::Form | Tag::Input
        );
        self.insert(tag, name, Ns::Html, push, foster);
    }

    /// 重复出现的 `<html>`、`<body>`：把还没有的属性补到已有元素上。
    fn merge_attrs(&mut self, target: u32) {
        if target == NONE {
            return;
        }
        let Data::Element {
            tag,
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
        let added: Vec<Attr> = self.attrs[new_start as usize..]
            .iter()
            .copied()
            .filter(|a| !old.iter().any(|o| self.str(o.name) == self.str(a.name)))
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
            name,
            attrs: start,
            attrs_len: self.attrs.len() as u32 - start,
        };
    }

    // ---- 栈操作

    fn in_scope(&self, target: Tag, scope: Scope) -> Option<usize> {
        self.find_in_scope(|t| t == target, scope)
    }

    fn find_in_scope(&self, pred: impl Fn(Tag) -> bool, scope: Scope) -> Option<usize> {
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

    fn boundary(&self, o: Open, scope: Scope) -> bool {
        use Tag::*;
        match o.ns {
            Ns::Integration => true,
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

    fn pop_to(&mut self, i: usize) {
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

    fn pop(&mut self) {
        if let Some(n) = self.stack.len().checked_sub(1) {
            self.pop_to(n);
        }
    }

    fn on_stack(&self, id: u32) -> Option<usize> {
        self.stack.iter().rposition(|o| o.id == id)
    }

    fn node_tag(&self, id: u32) -> Tag {
        match self.nodes[id as usize].data {
            Data::Element { tag, .. } => tag,
            _ => Tag::Other,
        }
    }

    fn clone_element(&mut self, id: u32) -> u32 {
        let data = self.nodes[id as usize].data;
        self.new_node(data)
    }

    fn detach(&mut self, id: u32) {
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

    fn move_children(&mut self, from: u32, to: u32) {
        let mut c = self.nodes[from as usize].first_child;
        while c != NONE {
            let next = self.nodes[c as usize].next;
            self.detach(c);
            self.append(to, c);
            c = next;
        }
    }

    /// 两个元素标签、名字和属性都相同（Noah's Ark 规则用）。
    fn same_element(&self, a: u32, b: u32) -> bool {
        let (
            Data::Element {
                tag: ta,
                name: na,
                attrs: aa,
                attrs_len: la,
            },
            Data::Element {
                tag: tb,
                name: nb,
                attrs: ab,
                attrs_len: lb,
            },
        ) = (self.nodes[a as usize].data, self.nodes[b as usize].data)
        else {
            return false;
        };
        if ta != tb || la != lb || self.str(na) != self.str(nb) {
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

    fn afe_after_marker(&self) -> usize {
        self.afe
            .iter()
            .rposition(|&e| e == NONE)
            .map_or(0, |i| i + 1)
    }

    fn push_formatting(&mut self, id: u32) {
        let start = self.afe_after_marker();
        let same: Vec<usize> = (start..self.afe.len())
            .filter(|&i| self.same_element(self.afe[i], id))
            .collect();
        if same.len() >= 3 {
            self.afe.remove(same[0]);
        }
        self.afe.push(id);
    }

    /// 重建活动格式元素：`<p><b>x<p>y` 里 y 也是粗体。
    fn reconstruct(&mut self) {
        let Some(&last) = self.afe.last() else {
            return;
        };
        if last == NONE || self.on_stack(last).is_some() || self.stack.len() >= MAX_DEPTH {
            return;
        }
        let mut i = self.afe.len() - 1;
        while i > 0 {
            let e = self.afe[i - 1];
            if e == NONE || self.on_stack(e).is_some() {
                break;
            }
            i -= 1;
        }
        for j in i..self.afe.len() {
            let new = self.clone_element(self.afe[j]);
            self.place(new, true);
            let tag = self.node_tag(new);
            self.stack.push(Open {
                id: new,
                tag,
                ns: Ns::Html,
            });
            self.afe[j] = new;
        }
    }

    fn special_entry(o: Open) -> bool {
        match o.ns {
            Ns::Html => special(o.tag),
            Ns::Integration => true,
            Ns::Foreign => false,
        }
    }

    /// Adoption agency：格式元素的结束标签。`<a><div>x</a>y` 变成
    /// `<a></a><div><a>x</a>y</div>`，链接只包住它本来包住的文字。
    /// 返回 false 时调用方按普通结束标签处理。
    fn adoption_agency(&mut self, tag: Tag) -> bool {
        if let Some(cur) = self.current()
            && cur.ns == Ns::Html
            && cur.tag == tag
            && !self.afe.contains(&cur.id)
        {
            self.pop();
            return true;
        }
        for _ in 0..8 {
            let start = self.afe_after_marker();
            let Some(fe_pos) = (start..self.afe.len())
                .rev()
                .find(|&i| self.node_tag(self.afe[i]) == tag)
            else {
                return false;
            };
            let fe = self.afe[fe_pos];
            let Some(fe_idx) = self.on_stack(fe) else {
                self.afe.remove(fe_pos);
                return true;
            };
            if self.stack[fe_idx + 1..]
                .iter()
                .any(|&o| self.boundary(o, Scope::Default))
            {
                return true;
            }
            let Some(fb_idx) =
                (fe_idx + 1..self.stack.len()).find(|&i| Self::special_entry(self.stack[i]))
            else {
                self.pop_to(fe_idx);
                self.afe.remove(fe_pos);
                return true;
            };
            let fb = self.stack[fb_idx].id;
            let common = self.stack[fe_idx - 1].id;
            let mut bookmark = fe_pos;
            let mut node_idx = fb_idx;
            let mut last = fb;
            let mut counter = 0;
            loop {
                counter += 1;
                node_idx -= 1;
                let node = self.stack[node_idx].id;
                if node == fe {
                    break;
                }
                let mut pos = self.afe.iter().position(|&e| e == node);
                if counter > 3
                    && let Some(p) = pos
                {
                    self.afe.remove(p);
                    if p < bookmark {
                        bookmark -= 1;
                    }
                    pos = None;
                }
                let Some(p) = pos else {
                    self.stack.remove(node_idx);
                    continue;
                };
                let new = self.clone_element(node);
                self.afe[p] = new;
                self.stack[node_idx].id = new;
                if last == fb {
                    bookmark = p + 1;
                }
                self.detach(last);
                self.append(new, last);
                last = new;
            }
            self.detach(last);
            if table_context(self.node_tag(common)) {
                self.foster(last, fe_idx);
            } else {
                self.append(common, last);
            }
            let e = self.clone_element(fe);
            self.move_children(fb, e);
            self.append(fb, e);
            if let Some(p) = self.afe.iter().position(|&x| x == fe) {
                self.afe.remove(p);
                if p < bookmark {
                    bookmark -= 1;
                }
            }
            self.afe.insert(bookmark.min(self.afe.len()), e);
            if let Some(i) = self.on_stack(fe) {
                self.stack.remove(i);
            }
            let at = self.on_stack(fb).map_or(self.stack.len(), |i| i + 1);
            self.stack.insert(
                at,
                Open {
                    id: e,
                    tag,
                    ns: Ns::Html,
                },
            );
        }
        true
    }

    fn finish(&mut self) {
        if self.body == NONE {
            self.ensure_body();
        }
    }

    fn in_template(&self) -> bool {
        self.stack
            .iter()
            .any(|o| o.tag == Tag::Template && o.ns == Ns::Html)
    }

    fn close_p(&mut self) {
        if let Some(i) = self.in_scope(Tag::P, Scope::Button) {
            self.pop_to(i);
        }
    }

    /// `<li>`、`<dd>`、`<dt>` 关掉同类的未闭合元素，遇到其他"特殊"元素就停。
    fn close_list_item(&mut self, same: &[Tag]) {
        for i in (0..self.stack.len()).rev() {
            let o = self.stack[i];
            if o.ns == Ns::Html && same.contains(&o.tag) {
                self.pop_to(i);
                return;
            }
            let stops = match o.ns {
                Ns::Html => special(o.tag) && !matches!(o.tag, Tag::Address | Tag::Div | Tag::P),
                Ns::Integration => true,
                Ns::Foreign => false,
            };
            if stops {
                return;
            }
        }
    }

    fn pop_implied(&mut self, except_rtc: bool) {
        while let Some(o) = self.current() {
            let implied = o.ns == Ns::Html
                && matches!(
                    o.tag,
                    Tag::Dd
                        | Tag::Dt
                        | Tag::Li
                        | Tag::Optgroup
                        | Tag::Option
                        | Tag::P
                        | Tag::Rb
                        | Tag::Rp
                        | Tag::Rt
                        | Tag::Rtc
                )
                && !(except_rtc && o.tag == Tag::Rtc);
            if !implied {
                return;
            }
            self.pop();
        }
    }

    // ---- 文档结构

    fn ensure_html(&mut self) {
        if self.html == NONE {
            self.html = self.new_node(Data::Element {
                tag: Tag::Html,
                name: Span::default(),
                attrs: 0,
                attrs_len: 0,
            });
            self.append(0, self.html);
            self.stack.push(Open {
                id: self.html,
                tag: Tag::Html,
                ns: Ns::Html,
            });
        }
        if self.mode < Mode::BeforeHead {
            self.mode = Mode::BeforeHead;
        }
    }

    fn ensure_head(&mut self) {
        self.ensure_html();
        if self.head == NONE {
            self.head = self.new_node(Data::Element {
                tag: Tag::Head,
                name: Span::default(),
                attrs: 0,
                attrs_len: 0,
            });
            self.append(self.html, self.head);
            self.stack.push(Open {
                id: self.head,
                tag: Tag::Head,
                ns: Ns::Html,
            });
            self.mode = Mode::InHead;
        }
    }

    fn close_head(&mut self) {
        if self.mode <= Mode::InHead {
            self.ensure_head();
            if let Some(i) = self.stack.iter().rposition(|o| o.id == self.head) {
                self.pop_to(i);
            }
            self.mode = Mode::AfterHead;
        }
    }

    fn ensure_body(&mut self) {
        if self.mode < Mode::AfterHead {
            self.close_head();
        }
        if self.body == NONE {
            self.body = self.new_node(Data::Element {
                tag: Tag::Body,
                name: Span::default(),
                attrs: 0,
                attrs_len: 0,
            });
            self.append(self.html, self.body);
            self.stack.push(Open {
                id: self.body,
                tag: Tag::Body,
                ns: Ns::Html,
            });
        }
        self.mode = Mode::InBody;
    }

    // ---- token

    fn doctype(&mut self) {
        if self.mode == Mode::Initial {
            let id = self.new_node(Data::Doctype);
            self.append(0, id);
        }
    }

    fn comment(&mut self, s: usize, e: usize) {
        self.skip_lf = false;
        let id = self.new_node(Data::Comment(Span::source(s, e)));
        if self.mode == Mode::Initial || self.stack.is_empty() {
            self.append(0, id);
        } else {
            self.place(id, false);
        }
    }

    /// 普通文本（data 状态）：解码字符引用，按插入模式放置。
    fn text_range(&mut self, s: usize, e: usize, decode: bool) {
        if s >= e {
            return;
        }
        let span = self.text_span(s, e, decode);
        let Some(span) = self.drop_leading_lf(span) else {
            return;
        };
        let mut span = span;
        if self.mode < Mode::InBody && !self.in_template() {
            // body 之前：开头的空白不算内容，其余的文字隐式打开 body
            let lead = self.str(span).bytes().take_while(|&c| is_ws(c)).count();
            if lead == span.end() - span.offset() {
                return;
            }
            span = span.skip(lead);
            self.ensure_body();
        }
        let table_ws = self
            .current()
            .is_some_and(|o| o.ns == Ns::Html && table_context(o.tag))
            && self.str(span).bytes().all(is_ws);
        if !self.in_foreign() && !table_ws {
            self.reconstruct();
        }
        self.add_text(span, true);
    }

    /// raw text / RCDATA 元素的内容，直接放进当前元素。
    fn raw_text(&mut self, s: usize, e: usize, decode: bool) {
        if s >= e {
            self.skip_lf = false;
            return;
        }
        let span = self.text_span(s, e, decode);
        let Some(span) = self.drop_leading_lf(span) else {
            return;
        };
        self.add_text(span, false);
    }

    fn text_span(&mut self, s: usize, e: usize, decode: bool) -> Span {
        let raw = &self.src[s..e];
        if decode && memchr::memchr(b'&', raw.as_bytes()).is_some() {
            let start = self.buf.len();
            decode_into(&mut self.buf, raw, false);
            Span::owned(start, self.buf.len())
        } else {
            Span::source(s, e)
        }
    }

    fn drop_leading_lf(&mut self, span: Span) -> Option<Span> {
        let span = if std::mem::take(&mut self.skip_lf) && self.str(span).starts_with('\n') {
            span.skip(1)
        } else {
            span
        };
        (!span.is_empty()).then_some(span)
    }

    fn tag_of(&mut self, (s, e): (usize, usize)) -> Tag {
        let raw = &self.src.as_bytes()[s..e];
        if raw.len() > 16 {
            return Tag::Other;
        }
        if raw.iter().any(u8::is_ascii_uppercase) {
            self.lower.clear();
            self.lower
                .extend(raw.iter().map(|c| c.to_ascii_lowercase() as char));
            Tag::from_name(&self.lower)
        } else {
            Tag::from_name(&self.src[s..e])
        }
    }

    fn start_tag(&mut self, name: (usize, usize), self_closing: bool) -> Next {
        self.skip_lf = false;
        let tag = self.tag_of(name);
        if self.mode == Mode::Initial {
            if tag == Tag::Html {
                self.ensure_html();
                self.merge_attrs(self.html);
                return Next::Data;
            }
            self.ensure_html();
        }
        if self.mode == Mode::BeforeHead {
            match tag {
                Tag::Html => {
                    self.merge_attrs(self.html);
                    return Next::Data;
                }
                Tag::Head => {
                    self.head = self.create(Tag::Head, name);
                    self.append(self.html, self.head);
                    self.stack.push(Open {
                        id: self.head,
                        tag: Tag::Head,
                        ns: Ns::Html,
                    });
                    self.mode = Mode::InHead;
                    return Next::Data;
                }
                _ => self.ensure_head(),
            }
        }
        if self.mode == Mode::InHead && self.in_template() && self.head_element_tag(tag).is_none() {
            return self.body_start_tag(tag, name, self_closing);
        }
        if self.mode == Mode::InHead {
            if let Some(next) = self.head_element(tag, name) {
                return next;
            }
            if tag == Tag::Head {
                return Next::Data;
            }
            self.close_head();
        }
        if self.mode == Mode::AfterHead {
            match tag {
                Tag::Html => {
                    self.merge_attrs(self.html);
                    return Next::Data;
                }
                Tag::Body => {
                    self.body = self.create(Tag::Body, name);
                    self.append(self.html, self.body);
                    self.stack.push(Open {
                        id: self.body,
                        tag: Tag::Body,
                        ns: Ns::Html,
                    });
                    self.mode = Mode::InBody;
                    return Next::Data;
                }
                Tag::Head => return Next::Data,
                _ => {}
            }
            // head 之后、body 之前的 head 类元素仍然放进 head
            let head_like = matches!(
                tag,
                Tag::Base
                    | Tag::Basefont
                    | Tag::Bgsound
                    | Tag::Link
                    | Tag::Meta
                    | Tag::Noframes
                    | Tag::Script
                    | Tag::Style
                    | Tag::Template
                    | Tag::Title
            );
            if head_like && self.head != NONE {
                self.stack.push(Open {
                    id: self.head,
                    tag: Tag::Head,
                    ns: Ns::Html,
                });
                let next = self.head_element(tag, name).unwrap_or(Next::Data);
                if let Some(i) = self.stack.iter().rposition(|o| o.id == self.head) {
                    self.stack.remove(i);
                }
                return next;
            }
            self.ensure_body();
        }
        self.body_start_tag(tag, name, self_closing)
    }

    fn head_element_tag(&self, tag: Tag) -> Option<()> {
        matches!(
            tag,
            Tag::Html
                | Tag::Base
                | Tag::Basefont
                | Tag::Bgsound
                | Tag::Link
                | Tag::Meta
                | Tag::Title
                | Tag::Noscript
                | Tag::Noframes
                | Tag::Style
                | Tag::Script
                | Tag::Template
        )
        .then_some(())
    }

    /// head 里允许的元素；不是的返回 `None`。
    fn head_element(&mut self, tag: Tag, name: (usize, usize)) -> Option<Next> {
        let next = match tag {
            Tag::Html => {
                self.merge_attrs(self.html);
                Next::Data
            }
            Tag::Base | Tag::Basefont | Tag::Bgsound | Tag::Link | Tag::Meta => {
                self.insert(tag, name, Ns::Html, false, false);
                Next::Data
            }
            Tag::Title => {
                self.insert(tag, name, Ns::Html, true, false);
                Next::RcData
            }
            Tag::Noscript | Tag::Noframes | Tag::Style | Tag::Script => {
                self.insert(tag, name, Ns::Html, true, false);
                Next::RawText
            }
            Tag::Template => {
                self.insert(tag, name, Ns::Html, true, false);
                Next::Data
            }
            _ => return None,
        };
        Some(next)
    }

    fn body_start_tag(&mut self, tag: Tag, name: (usize, usize), self_closing: bool) -> Next {
        // 规范：HTML 里的 `<image>` 当作 `<img>`；SVG 里的 `<image>` 是 SVG 元素，不能触发跳出
        let tag = if tag == Tag::Other
            && !self.in_foreign()
            && self.src[name.0..name.1].eq_ignore_ascii_case("image")
        {
            Tag::Img
        } else {
            tag
        };
        if self.in_foreign() {
            let font_breaks = tag == Tag::Font
                && self.scratch.iter().any(|&((s, e), _)| {
                    let n = &self.src[s..e];
                    ["color", "face", "size"]
                        .iter()
                        .any(|k| n.eq_ignore_ascii_case(k))
                });
            if breaks_out_of_foreign(tag) || font_breaks {
                while self.in_foreign() {
                    self.pop();
                }
            } else {
                self.insert_foreign(tag, name, self_closing);
                return Next::Data;
            }
        }
        use Tag::*;
        match tag {
            Html => self.merge_attrs(self.html),
            Body => self.merge_attrs(self.body),
            Head | Frameset | Frame => {}
            Base | Basefont | Bgsound | Link | Meta | Param | Source | Track => {
                self.insert_html(tag, name)
            }
            Area | Br | Embed | Img | Keygen | Wbr | Input => {
                self.reconstruct();
                self.insert_html(tag, name);
            }
            Hr => {
                self.close_p();
                self.insert_html(tag, name);
            }
            Title => {
                self.insert_html(tag, name);
                return Next::RcData;
            }
            Textarea => {
                self.insert_html(tag, name);
                self.skip_lf = true;
                return Next::RcData;
            }
            Xmp => {
                self.close_p();
                self.insert_html(tag, name);
                return Next::RawText;
            }
            Style | Noframes | Noscript | Iframe | Noembed | Script => {
                self.insert_html(tag, name);
                return Next::RawText;
            }
            Plaintext => {
                self.close_p();
                self.insert_html(tag, name);
                return Next::PlainText;
            }
            Address | Article | Aside | Blockquote | Center | Details | Dialog | Dir | Div | Dl
            | Fieldset | Figcaption | Figure | Footer | Header | Hgroup | Main | Menu | Nav
            | Ol | P | Search | Section | Summary | Ul => {
                self.close_p();
                self.insert_html(tag, name);
            }
            Form => {
                if self.form != NONE && !self.in_template() {
                    self.scratch.clear();
                    return Next::Data;
                }
                if self
                    .current()
                    .is_some_and(|o| o.ns == Ns::Html && table_context(o.tag))
                {
                    // 表格里的 form 是个空元素，内容不进 form
                    self.form = self.insert(tag, name, Ns::Html, false, false);
                } else {
                    self.close_p();
                    self.form = self.insert(tag, name, Ns::Html, true, true);
                }
            }
            H1 | H2 | H3 | H4 | H5 | H6 => {
                self.close_p();
                if self
                    .current()
                    .is_some_and(|o| o.ns == Ns::Html && heading(o.tag))
                {
                    self.pop();
                }
                self.insert_html(tag, name);
            }
            Pre | Listing => {
                self.close_p();
                self.insert_html(tag, name);
                self.skip_lf = true;
            }
            Li => {
                self.close_list_item(&[Li]);
                self.close_p();
                self.insert_html(tag, name);
            }
            Dd | Dt => {
                self.close_list_item(&[Dd, Dt]);
                self.close_p();
                self.insert_html(tag, name);
            }
            Button => {
                if let Some(i) = self.in_scope(Button, Scope::Default) {
                    self.pop_to(i);
                }
                self.insert_html(tag, name);
            }
            A => {
                // 上一个 `<a>` 没关：先按 `</a>` 处理
                let start = self.afe_after_marker();
                if let Some(old) = self.afe[start..]
                    .iter()
                    .copied()
                    .find(|&e| self.node_tag(e) == A)
                {
                    let attrs = std::mem::take(&mut self.scratch);
                    self.adoption_agency(A);
                    self.scratch = attrs;
                    self.afe.retain(|&e| e != old);
                    if let Some(i) = self.on_stack(old) {
                        self.stack.remove(i);
                    }
                }
                self.reconstruct();
                let id = self.insert(tag, name, Ns::Html, true, true);
                self.push_formatting(id);
            }
            Nobr => {
                self.reconstruct();
                if self.in_scope(Nobr, Scope::Default).is_some() {
                    let attrs = std::mem::take(&mut self.scratch);
                    self.adoption_agency(Nobr);
                    self.scratch = attrs;
                    self.reconstruct();
                }
                let id = self.insert(tag, name, Ns::Html, true, true);
                self.push_formatting(id);
            }
            B | Big | Code | Em | Font | I | S | Small | Strike | Strong | Tt | U => {
                self.reconstruct();
                let id = self.insert(tag, name, Ns::Html, true, true);
                self.push_formatting(id);
            }
            Applet | Marquee | Object => {
                self.reconstruct();
                self.insert_html(tag, name);
            }
            Select => {
                self.reconstruct();
                self.insert_html(tag, name);
            }
            Table => {
                let in_table = self
                    .current()
                    .is_some_and(|o| o.ns == Ns::Html && table_context(o.tag));
                if in_table && let Some(i) = self.in_scope(Table, Scope::Table) {
                    self.pop_to(i);
                } else {
                    self.close_p();
                }
                self.insert(tag, name, Ns::Html, true, false);
            }
            Caption | Colgroup | Col | Tbody | Thead | Tfoot | Tr | Td | Th => {
                self.table_part(tag, name)
            }
            Option | Optgroup => {
                if self.current_is(Option) {
                    self.pop();
                }
                self.reconstruct();
                self.insert_html(tag, name);
            }
            Rb | Rtc => {
                if self.in_scope(Ruby, Scope::Default).is_some() {
                    self.pop_implied(false);
                }
                self.insert_html(tag, name);
            }
            Rp | Rt => {
                if self.in_scope(Ruby, Scope::Default).is_some() {
                    self.pop_implied(true);
                }
                self.insert_html(tag, name);
            }
            Svg | Math => {
                self.reconstruct();
                let id = self.create(tag, name);
                self.place(id, true);
                if !self_closing && self.stack.len() < MAX_DEPTH {
                    self.stack.push(Open {
                        id,
                        tag,
                        ns: Ns::Foreign,
                    });
                }
            }
            _ => {
                self.reconstruct();
                self.insert_html(tag, name);
            }
        }
        Next::Data
    }

    /// SVG/MathML 里的元素：名字照存，自闭合的不入栈。
    fn insert_foreign(&mut self, tag: Tag, name: (usize, usize), self_closing: bool) {
        let tag = if matches!(tag, Tag::Svg | Tag::Math) {
            tag
        } else {
            Tag::Other
        };
        let id = self.create(tag, name);
        self.place(id, false);
        if self_closing || self.stack.len() >= MAX_DEPTH {
            return;
        }
        let lname = self.src[name.0..name.1].to_ascii_lowercase();
        let ns = if integration_point(&lname) {
            Ns::Integration
        } else {
            Ns::Foreign
        };
        self.stack.push(Open { id, tag, ns });
    }

    /// 表格结构元素：补上省略的 tbody、tr，关掉未闭合的单元格和行。不在表格里时忽略。
    fn table_part(&mut self, tag: Tag, name: (usize, usize)) {
        use Tag::*;
        let Some(table) = self.in_scope(Table, Scope::Table) else {
            self.scratch.clear();
            return;
        };
        let clear_to_table = |b: &mut Self| b.pop_to(table + 1);
        match tag {
            Caption | Colgroup | Tbody | Thead | Tfoot => {
                clear_to_table(self);
                self.insert(tag, name, Ns::Html, true, false);
            }
            Col => {
                if !self.current_is(Colgroup) {
                    clear_to_table(self);
                    self.implied(Colgroup);
                }
                self.insert(tag, name, Ns::Html, false, false);
            }
            Tr | Td | Th => {
                if let Some(i) = self.find_in_scope(|t| matches!(t, Td | Th), Scope::Table) {
                    self.pop_to(i);
                }
                // 清到行（单元格）或表格主体（行）的上下文，缺的 tbody、tr 补上
                let stop: &[Tag] = if tag == Tr {
                    &[Tbody, Thead, Tfoot, Table]
                } else {
                    &[Tr, Tbody, Thead, Tfoot, Table]
                };
                while let Some(o) = self.current() {
                    if o.ns == Ns::Html && stop.contains(&o.tag) {
                        break;
                    }
                    self.pop();
                }
                if self.current_is(Table) {
                    self.implied(Tbody);
                }
                if tag != Tr && !self.current_is(Tr) {
                    self.implied(Tr);
                }
                self.insert(tag, name, Ns::Html, true, false);
            }
            _ => unreachable!(),
        }
    }

    fn implied(&mut self, tag: Tag) {
        let id = self.new_node(Data::Element {
            tag,
            name: Span::default(),
            attrs: self.attrs.len() as u32,
            attrs_len: 0,
        });
        self.place(id, false);
        self.stack.push(Open {
            id,
            tag,
            ns: Ns::Html,
        });
    }

    fn end_tag(&mut self, name: (usize, usize)) {
        self.skip_lf = false;
        self.scratch.clear();
        let tag = self.tag_of(name);
        // 外部内容：按名字（不分大小写）在外部元素里找，碰到 HTML 元素就按 HTML 规则处理
        if self.stack.last().is_some_and(|o| o.ns != Ns::Html) {
            let raw = &self.src[name.0..name.1];
            for i in (0..self.stack.len()).rev() {
                let o = self.stack[i];
                if o.ns == Ns::Html {
                    break;
                }
                if self.element_name_eq(o.id, raw) {
                    self.pop_to(i);
                    return;
                }
            }
        }
        // 最常见的情况：关的就是当前元素
        if let Some(cur) = self.current()
            && cur.ns != Ns::Foreign
            && cur.tag == tag
            && (tag != Tag::Other || self.element_name_eq(cur.id, &self.src[name.0..name.1]))
            && !matches!(tag, Tag::Html | Tag::Body)
            && !formatting(tag)
        {
            if cur.id == self.head {
                self.mode = Mode::AfterHead;
            }
            if cur.id == self.form {
                self.form = NONE;
            }
            self.pop();
            return;
        }
        use Tag::*;
        match self.mode {
            Mode::InBody => {}
            _ => match tag {
                Head if self.mode == Mode::InHead => {
                    self.close_head();
                    return;
                }
                Body | Html | Br => {
                    self.ensure_body();
                    if tag != Br {
                        return;
                    }
                }
                _ => return,
            },
        }
        match tag {
            Body | Html | Head => {}
            // `</form>` 只把 form 从栈里拿掉，里面没闭合的元素继续开着
            Form => {
                let form = std::mem::replace(&mut self.form, NONE);
                if let Some(i) = self.in_scope(Form, Scope::Default)
                    && self.stack[i].id == form
                {
                    self.stack.remove(i);
                }
            }
            t if formatting(t) => {
                if !self.adoption_agency(tag) {
                    self.any_other_end(tag, name);
                }
            }
            Br => {
                self.reconstruct();
                self.insert_html(Br, name);
            }
            P => match self.in_scope(P, Scope::Button) {
                Some(i) => self.pop_to(i),
                // 没有打开的 `<p>`：规范要求补一个空段落
                None => {
                    self.insert(P, name, Ns::Html, false, true);
                }
            },
            Li => {
                if let Some(i) = self.in_scope(Li, Scope::ListItem) {
                    self.pop_to(i);
                }
            }
            H1 | H2 | H3 | H4 | H5 | H6 => {
                if let Some(i) = self.find_in_scope(heading, Scope::Default) {
                    self.pop_to(i);
                }
            }
            Table | Tbody | Thead | Tfoot | Tr | Td | Th | Caption | Colgroup => {
                if let Some(i) = self.in_scope(tag, Scope::Table) {
                    self.pop_to(i);
                }
            }
            t if void(t) => {}
            t if special(t) => {
                if let Some(i) = self.in_scope(t, Scope::Default) {
                    self.pop_to(i);
                }
            }
            _ => self.any_other_end(tag, name),
        }
    }

    /// 一般的结束标签：从栈顶往下找同名元素，碰到"特殊"元素就放弃。
    fn any_other_end(&mut self, tag: Tag, name: (usize, usize)) {
        let raw = &self.src[name.0..name.1];
        for i in (0..self.stack.len()).rev() {
            let o = self.stack[i];
            let same = o.tag == tag && (tag != Tag::Other || self.element_name_eq(o.id, raw));
            if same && o.ns != Ns::Foreign {
                self.pop_to(i);
                return;
            }
            let stop = match o.ns {
                Ns::Html => special(o.tag),
                Ns::Integration => true,
                Ns::Foreign => false,
            };
            if stop {
                return;
            }
        }
    }

    fn element_name_eq(&self, id: u32, raw: &str) -> bool {
        match self.nodes[id as usize].data {
            Data::Element {
                tag: Tag::Other,
                name,
                ..
            } => self.str(name).eq_ignore_ascii_case(raw),
            Data::Element { tag, .. } => tag.name().eq_ignore_ascii_case(raw),
            _ => false,
        }
    }
}
