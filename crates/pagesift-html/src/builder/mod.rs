//! 分词和建树合在一起：分词器用 memchr 在源字符串里跳着找 `<`，直接把标签、文本交给建树器，
//! 中间不产生 token 对象。建树器实现 HTML5 树构建规则里影响文本结构的部分：
//! 隐式的 html/head/body、`<p>` `<li>` `<dd>` `<option>` 等的自动闭合、作用域边界、
//! 表格的隐式 tbody/tr 和 foster parenting、SVG/MathML 外部内容和跳出规则、
//! raw text 和 RCDATA 元素、`<pre>` 开头的换行、脚本的转义状态、quirks 模式（只影响
//! `<table>` 是否关掉 `<p>`）、select 和列组的内容规则。
//! 和规范有意不同的：`<template>` 不建独立的文档片段（内容作为普通子节点）；插入模式按当前节点
//! 推断，不单独记录（frameset 文档的三种模式除外）。和 html5lib-tests 的逐条对照见 `tests/html5lib`。

use std::borrow::Cow;

use crate::Tag;
use crate::dom::{Attr, Data, Html, NONE, Namespace, Node, ParseScratch, Span};

/// 打开元素栈的最大深度。更深的元素照样插入但不入栈，树的深度因此有上限，
/// 递归遍历这棵树的代码不会因为恶意的深层嵌套栈溢出。
const MAX_DEPTH: usize = 512;

pub(crate) fn parse(html: &str) -> Html<'_> {
    parse_with_scratch(html, &mut ParseScratch::default())
}

pub(crate) fn parse_with_scratch<'s>(html: &'s str, scratch: &mut ParseScratch) -> Html<'s> {
    let src = normalize(html);
    let (buf, nodes, attrs) = {
        let mut tb = Builder::with_buffers(
            &src,
            std::mem::take(&mut scratch.buf),
            std::mem::take(&mut scratch.nodes),
            std::mem::take(&mut scratch.attrs),
        );
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

mod end;
mod format;
mod frameset;
mod quirks;
mod script;
mod tags;
mod tokenize;
mod tree;

use tokenize::{Next, RawAttr, is_ws, tokenize};

// ---------------------------------------------------------------- 建树

#[derive(Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
enum Mode {
    Initial,
    BeforeHead,
    InHead,
    AfterHead,
    InBody,
    /// frameset 文档的三种模式，见 `frameset` 模块。
    InFrameset,
    AfterFrameset,
    AfterAfterFrameset,
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum Ns {
    Html,
    /// SVG / MathML 元素。
    Foreign,
    /// SVG 的 foreignObject、desc、title，MathML 的 mi、mo、mn、ms、mtext，`encoding` 是 HTML 的
    /// annotation-xml：外部内容里重新按 HTML 解析的地方。
    Integration,
    /// 其他 annotation-xml：内容仍是 MathML，但和集成点一样是"特殊"元素、作用域边界。
    Annotation,
}

impl Ns {
    /// 按外部内容的规则分词、建树（SVG/MathML 元素，集成点除外）。
    fn foreign(self) -> bool {
        matches!(self, Ns::Foreign | Ns::Annotation)
    }
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
    /// quirks 模式（没有 DOCTYPE 或老式 DOCTYPE），见 `quirks` 模块。
    quirks: bool,
    /// 规范的 frameset-ok 标志：body 里出现正文或某些元素之后，`<frameset>` 不再取代 body。
    frameset_ok: bool,
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

/// 外部元素在栈上的类别：HTML 集成点、MathML 文本集成点、annotation-xml 或普通外部元素。
/// `encoding_html` 是元素的 `encoding` 属性为 `text/html` 或 `application/xhtml+xml`。
fn foreign_kind(ns: Namespace, name: &str, encoding_html: bool) -> Ns {
    let is = |n: &str| name.eq_ignore_ascii_case(n);
    match ns {
        Namespace::Svg if is("foreignobject") || is("desc") || is("title") => Ns::Integration,
        Namespace::MathMl if is("mi") || is("mo") || is("mn") || is("ms") || is("mtext") => {
            Ns::Integration
        }
        Namespace::MathMl if is("annotation-xml") => {
            if encoding_html {
                Ns::Integration
            } else {
                Ns::Annotation
            }
        }
        _ => Ns::Foreign,
    }
}

impl<'a> Builder<'a> {
    fn with_buffers(
        src: &'a str,
        mut buf: String,
        mut nodes: Vec<Node>,
        mut attrs: Vec<Attr>,
    ) -> Self {
        buf.clear();
        nodes.clear();
        attrs.clear();
        let node_capacity = src.len() / 40 + 16;
        if nodes.capacity() < node_capacity {
            nodes.reserve(node_capacity - nodes.capacity());
        }
        let attr_capacity = src.len() / 80 + 16;
        if attrs.capacity() < attr_capacity {
            attrs.reserve(attr_capacity - attrs.capacity());
        }
        nodes.push(Node::new(Data::Document));
        Builder {
            src,
            buf,
            nodes,
            attrs,
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
            quirks: true,
            frameset_ok: true,
        }
    }

    pub(super) fn str(&self, s: Span) -> &str {
        if s.is_owned() {
            &self.buf[s.offset()..s.end()]
        } else {
            &self.src[s.offset()..s.end()]
        }
    }

    pub(super) fn in_foreign(&self) -> bool {
        self.stack.last().is_some_and(|o| o.ns.foreign())
    }

    pub(super) fn current(&self) -> Option<Open> {
        self.stack.last().copied()
    }

    pub(super) fn current_is(&self, tag: Tag) -> bool {
        self.current()
            .is_some_and(|o| o.tag == tag && !o.ns.foreign())
    }
}
