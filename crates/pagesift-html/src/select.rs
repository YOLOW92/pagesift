//! CSS 选择器：解析成从右往左排列的复合选择器链，匹配时沿父节点、前兄弟回溯，过程中不分配内存。
//!
//! 支持的语法：类型、`*`、`#id`、`.class`、属性（`[a]` `=` `~=` `|=` `^=` `$=` `*=`，带 `i`/`s` 标志）、
//! 后代/子/相邻兄弟/通用兄弟组合器、选择器列表、`:not()` `:is()` `:where()` `:has()`、
//! `:nth-child()` `:nth-last-child()`（可带 `of S`）`:nth-of-type()` `:nth-last-of-type()`、
//! `:first-child` `:last-child` `:only-child` `:first-of-type` `:last-of-type` `:only-of-type`、
//! `:empty` `:root` `:scope`。
//! 命名空间、伪元素和依赖浏览器状态的伪类（`:hover` 等）不支持，解析时报错。
//!
//! 语义和 scraper（Servo 的 selectors）一致：class 和 id 区分大小写（no-quirks），
//! HTML 规范列出的一些属性（`type`、`lang` 等）的值默认不区分大小写。
//!
//! ```
//! use pagesift_html::{Html, Selector};
//!
//! let doc = Html::parse(r#"<ul><li class="a">x</li><li class="b">y</li></ul>"#);
//! let sel = Selector::parse("ul > li:not(.a)").unwrap();
//! let hits: Vec<String> = doc.select(&sel).map(|n| n.text()).collect();
//! assert_eq!(hits, ["y"]);
//! ```

use std::fmt;
use std::str::FromStr;

use crate::{Descendants, Element, Html, NodeId, NodeRef, Tag};

/// 函数式伪类（`:not(` 等）的最大嵌套层数，限制解析器的递归深度。
const MAX_NESTING: u32 = 32;
/// 匹配时最深的递归层数（复合选择器个数加上嵌套），限制匹配器的栈深度。
const MAX_DEPTH: u32 = 256;

/// 解析好的选择器列表（逗号分隔）。解析一次，可以在多个文档、多个线程上复用。
#[derive(Debug, Clone)]
pub struct Selector {
    list: Box<[Complex]>,
}

/// 复杂选择器，例如 `ul.nav > li a`。`parts[0]` 是最右边的复合选择器（匹配主体），
/// `parts[i].comb` 是 `parts[i]` 和它左边的 `parts[i + 1]` 之间的组合器。
#[derive(Debug, Clone)]
struct Complex {
    parts: Box<[Compound]>,
}

#[derive(Debug, Clone)]
struct Compound {
    /// 含已知标签的类型选择器时就是那个标签：匹配前先比节点的标签，多数节点不用构造 [`Element`]。
    tag: Option<Tag>,
    simples: Box<[Simple]>,
    comb: Comb,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Comb {
    Descendant,
    Child,
    Next,
    Later,
}

#[derive(Debug, Clone)]
enum Simple {
    /// 类型选择器。已知标签比枚举；`Other` 和外部内容（SVG 等）里的元素比小写名字。
    Type(Tag, Box<str>),
    Id(Box<str>),
    Class(Box<str>),
    Attr(Box<AttrSel>),
    Nth(Box<Nth>),
    Empty,
    Root,
    Scope,
    /// `:has()` 里相对选择器的锚点，即正在判断 `:has()` 的那个元素。
    Anchor,
    Not(Selector),
    Is(Selector),
    Has(Selector),
}

impl Simple {
    /// 复合选择器里先检查便宜的条件。
    fn cost(&self) -> u8 {
        match self {
            Simple::Type(..) => 0,
            Simple::Id(_) => 1,
            Simple::Class(_) => 2,
            Simple::Attr(_) => 3,
            Simple::Empty | Simple::Root | Simple::Scope | Simple::Anchor => 4,
            Simple::Nth(_) => 5,
            Simple::Not(_) | Simple::Is(_) => 6,
            Simple::Has(_) => 7,
        }
    }
}

#[derive(Debug, Clone)]
struct AttrSel {
    name: Box<str>,
    op: AttrOp,
    value: Box<str>,
    /// 值按 ASCII 不区分大小写比较。
    ci: bool,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum AttrOp {
    Exists,
    Equals,
    Includes,
    Dash,
    Prefix,
    Suffix,
    Substring,
    /// 永远不匹配：`~=` 的值为空或含空白，`^=` `$=` `*=` 的值为空。
    Never,
}

/// `:nth-*()`：位置满足 `a*n + b`（n ≥ 0）。
#[derive(Debug, Clone)]
struct Nth {
    a: i32,
    b: i32,
    /// 从后往前数。
    last: bool,
    /// 只数同类型的兄弟。
    of_type: bool,
    /// `of S`：只数匹配 S 的兄弟，自身也要匹配 S。
    of: Option<Selector>,
}

impl Nth {
    fn simple(a: i32, b: i32, last: bool, of_type: bool) -> Simple {
        Simple::Nth(Box::new(Nth {
            a,
            b,
            last,
            of_type,
            of: None,
        }))
    }
}

/// 选择器语法错误：出错位置（字节偏移）和原因。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SelectorError {
    offset: usize,
    message: String,
}

impl SelectorError {
    /// 出错位置在选择器字符串里的字节偏移。
    pub fn offset(&self) -> usize {
        self.offset
    }

    pub fn message(&self) -> &str {
        &self.message
    }
}

impl fmt::Display for SelectorError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{} at offset {}", self.message, self.offset)
    }
}

impl std::error::Error for SelectorError {}

impl Selector {
    /// 解析选择器列表。语法错误、不支持的语法返回错误，不会 panic。
    pub fn parse(s: &str) -> Result<Selector, SelectorError> {
        let mut p = Parser {
            s,
            b: s.as_bytes(),
            pos: 0,
            nesting: 0,
        };
        let (sel, depth) = p.list(false, false)?;
        if p.pos < p.b.len() {
            return Err(p.unexpected());
        }
        if depth > MAX_DEPTH {
            return Err(SelectorError {
                offset: 0,
                message: format!("selector is too complex (matching depth {depth} > {MAX_DEPTH})"),
            });
        }
        Ok(sel)
    }

    /// 元素是否匹配。`:scope` 匹配根元素。不是元素的节点永远不匹配。
    pub fn matches(&self, node: NodeRef<'_>) -> bool {
        self.matches_with_scope(node, None)
    }

    /// 元素是否匹配，`:scope` 匹配 `scope`（为 `None` 时匹配根元素）。
    pub fn matches_with_scope(&self, node: NodeRef<'_>, scope: Option<NodeRef<'_>>) -> bool {
        let cx = Ctx {
            scope: scope.filter(NodeRef::is_element).map(|s| s.id()),
            anchor: None,
        };
        node.is_element() && self.matches_in(node, &cx)
    }

    #[inline]
    fn matches_in(&self, el: NodeRef<'_>, cx: &Ctx) -> bool {
        self.list.iter().any(|c| {
            c.parts[0].tag_may_match(el) && match_complex(&c.parts, 0, el, cx) == Res::Matched
        })
    }
}

impl FromStr for Selector {
    type Err = SelectorError;
    fn from_str(s: &str) -> Result<Self, Self::Err> {
        Selector::parse(s)
    }
}

impl<'a> Html<'a> {
    /// 文档里匹配选择器的所有元素，按文档顺序。
    pub fn select<'s>(&'a self, sel: &'s Selector) -> Select<'a, 's> {
        Select {
            inner: self.root().descendants(),
            sel,
            cx: Ctx {
                scope: None,
                anchor: None,
            },
        }
    }
}

impl<'a> NodeRef<'a> {
    /// 后代（不含自身）里匹配选择器的元素，按文档顺序；`:scope` 匹配自身。
    pub fn select<'s>(&self, sel: &'s Selector) -> Select<'a, 's> {
        let mut inner = self.descendants();
        inner.next();
        Select {
            inner,
            sel,
            cx: Ctx {
                scope: self.is_element().then(|| self.id()),
                anchor: None,
            },
        }
    }
}

/// [`Html::select`] 和 [`NodeRef::select`] 返回的迭代器。
pub struct Select<'a, 's> {
    inner: Descendants<'a>,
    sel: &'s Selector,
    cx: Ctx,
}

impl<'a> Iterator for Select<'a, '_> {
    type Item = NodeRef<'a>;
    fn next(&mut self) -> Option<NodeRef<'a>> {
        self.inner
            .by_ref()
            .find(|n| n.is_element() && self.sel.matches_in(*n, &self.cx))
    }
}

// ---------------------------------------------------------------------------
// 匹配
// ---------------------------------------------------------------------------

struct Ctx {
    /// `:scope` 指向的元素；`None` 时是根元素。
    scope: Option<NodeId>,
    /// `:has()` 正在判断的元素。
    anchor: Option<NodeId>,
}

/// 复杂选择器的匹配结果。失败时告诉上层从哪里重新开始，避免后代组合器的指数回溯
/// （和 Servo selectors 的 `SelectorMatchingResult` 同一套规则）。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Res {
    Matched,
    RestartFromLaterSibling,
    RestartFromDescendant,
    NotMatchedGlobally,
}

fn match_complex(parts: &[Compound], i: usize, el: NodeRef<'_>, cx: &Ctx) -> Res {
    let part = &parts[i];
    if !part.tag_may_match(el) || !match_compound(&part.simples, el, cx) {
        return Res::RestartFromLaterSibling;
    }
    if i + 1 == parts.len() {
        return Res::Matched;
    }
    let comb = part.comb;
    let not_found = match comb {
        Comb::Next | Comb::Later => Res::RestartFromDescendant,
        Comb::Child | Comb::Descendant => Res::NotMatchedGlobally,
    };
    let mut cur = el;
    loop {
        let next = match comb {
            Comb::Child | Comb::Descendant => cur.parent().filter(NodeRef::is_element),
            Comb::Next | Comb::Later => prev_element(cur),
        };
        let Some(next) = next else {
            return not_found;
        };
        // 标签对不上时不必递归，结果就是复合选择器不匹配
        let r = if parts[i + 1].tag_may_match(next) {
            match_complex(parts, i + 1, next, cx)
        } else {
            Res::RestartFromLaterSibling
        };
        match (r, comb) {
            (Res::Matched | Res::NotMatchedGlobally, _) | (_, Comb::Next) => return r,
            (_, Comb::Child) => return Res::RestartFromDescendant,
            (Res::RestartFromDescendant, Comb::Later) => return r,
            _ => {}
        }
        cur = next;
    }
}

impl Compound {
    /// 按标签快速排除。外部内容里的元素都是 Other，交给 [`type_matches`] 比名字。
    #[inline]
    fn tag_may_match(&self, el: NodeRef<'_>) -> bool {
        match self.tag {
            Some(t) => matches!(el.tag(), Some(x) if x == t || x == Tag::Other),
            None => true,
        }
    }
}

fn match_compound(simples: &[Simple], el: NodeRef<'_>, cx: &Ctx) -> bool {
    let Some(e) = el.as_element() else {
        return false;
    };
    simples.iter().all(|s| match_simple(s, el, &e, cx))
}

fn match_simple(s: &Simple, el: NodeRef<'_>, e: &Element<'_>, cx: &Ctx) -> bool {
    match s {
        Simple::Type(tag, name) => type_matches(e, *tag, name),
        Simple::Id(id) => e.attr("id") == Some(&**id),
        Simple::Class(c) => e
            .attr("class")
            .is_some_and(|v| v.split_ascii_whitespace().any(|x| x == &**c)),
        Simple::Attr(a) => e.attr(&a.name).is_some_and(|v| attr_matches(a, v)),
        Simple::Nth(n) => nth_matches(n, el, e, cx),
        Simple::Empty => !el.children().any(|c| c.is_element() || c.is_text()),
        Simple::Root => el.parent().is_some_and(|p| p.is_document()),
        Simple::Scope => match cx.scope {
            Some(id) => el.id() == id,
            None => el.parent().is_some_and(|p| p.is_document()),
        },
        Simple::Anchor => cx.anchor == Some(el.id()),
        Simple::Not(sel) => !sel.matches_in(el, cx),
        Simple::Is(sel) => sel.matches_in(el, cx),
        Simple::Has(sel) => has_matches(sel, el, cx),
    }
}

fn type_matches(e: &Element<'_>, tag: Tag, name: &str) -> bool {
    let t = e.tag();
    if t == tag {
        tag != Tag::Other || e.name() == name
    } else {
        // SVG/MathML 里的元素都是 Other，名字可能和 HTML 标签相同（例如 svg 的 <a>）
        t == Tag::Other && e.name() == name
    }
}

fn attr_matches(a: &AttrSel, v: &str) -> bool {
    let (v, want) = (v.as_bytes(), a.value.as_bytes());
    let eq = |x: &[u8], y: &[u8]| {
        if a.ci {
            x.eq_ignore_ascii_case(y)
        } else {
            x == y
        }
    };
    match a.op {
        AttrOp::Exists => true,
        AttrOp::Equals => eq(v, want),
        AttrOp::Includes => v
            .split(|c| matches!(c, b' ' | b'\t' | b'\n' | b'\x0c' | b'\r'))
            .any(|w| eq(w, want)),
        AttrOp::Dash => {
            eq(v, want)
                || (v.len() > want.len() && v[want.len()] == b'-' && eq(&v[..want.len()], want))
        }
        AttrOp::Prefix => v.len() >= want.len() && eq(&v[..want.len()], want),
        AttrOp::Suffix => v.len() >= want.len() && eq(&v[v.len() - want.len()..], want),
        AttrOp::Substring => {
            if a.ci {
                v.windows(want.len()).any(|w| w.eq_ignore_ascii_case(want))
            } else {
                memchr::memmem::find(v, want).is_some()
            }
        }
        AttrOp::Never => false,
    }
}

fn nth_matches(n: &Nth, el: NodeRef<'_>, e: &Element<'_>, cx: &Ctx) -> bool {
    if let Some(of) = &n.of
        && !of.matches_in(el, cx)
    {
        return false;
    }
    let tag = e.tag();
    let counts = |s: NodeRef<'_>| {
        let Some(st) = s.tag() else {
            return false;
        };
        if n.of_type {
            st == tag
                && (tag != Tag::Other || s.as_element().is_some_and(|se| se.name() == e.name()))
        } else if let Some(of) = &n.of {
            of.matches_in(s, cx)
        } else {
            true
        }
    };
    let mut pos = 1i64;
    let mut cur = el;
    loop {
        let next = if n.last {
            cur.next_sibling()
        } else {
            cur.prev_sibling()
        };
        let Some(s) = next else { break };
        if counts(s) {
            pos += 1;
        }
        cur = s;
    }
    let (a, b) = (i64::from(n.a), i64::from(n.b));
    if a == 0 {
        pos == b
    } else {
        let d = pos - b;
        d % a == 0 && d / a >= 0
    }
}

/// `:has()`：锚点后面（后代或之后的兄弟及其后代）有元素匹配相对选择器。
fn has_matches(sel: &Selector, el: NodeRef<'_>, cx: &Ctx) -> bool {
    let inner = Ctx {
        scope: cx.scope,
        anchor: Some(el.id()),
    };
    sel.list.iter().any(|c| {
        let leading = c.parts[c.parts.len() - 2].comb;
        let hit = |n: NodeRef<'_>| {
            n.is_element() && match_complex(&c.parts, 0, n, &inner) == Res::Matched
        };
        // 只有一个复合选择器时（`:has(> img)`、`:has(+ p)`），候选范围可以收窄到子节点或兄弟
        let single = c.parts.len() == 2;
        match leading {
            Comb::Child if single => el.children().any(hit),
            Comb::Next if single => next_element(el).is_some_and(hit),
            Comb::Later if single => {
                let mut s = el.next_sibling();
                while let Some(n) = s {
                    if hit(n) {
                        return true;
                    }
                    s = n.next_sibling();
                }
                false
            }
            Comb::Descendant | Comb::Child => el.descendants().skip(1).any(hit),
            Comb::Next | Comb::Later => {
                let mut s = el.next_sibling();
                while let Some(n) = s {
                    if n.descendants().any(hit) {
                        return true;
                    }
                    s = n.next_sibling();
                }
                false
            }
        }
    })
}

fn next_element<'a>(n: NodeRef<'a>) -> Option<NodeRef<'a>> {
    let mut cur = n.next_sibling();
    while let Some(c) = cur {
        if c.is_element() {
            return Some(c);
        }
        cur = c.next_sibling();
    }
    None
}

fn prev_element<'a>(n: NodeRef<'a>) -> Option<NodeRef<'a>> {
    let mut cur = n.prev_sibling();
    while let Some(c) = cur {
        if c.is_element() {
            return Some(c);
        }
        cur = c.prev_sibling();
    }
    None
}

// ---------------------------------------------------------------------------
// 解析
// ---------------------------------------------------------------------------

/// HTML 规范里值不区分大小写的属性（selectors 对 HTML 元素也按这张表处理）。
const CASE_INSENSITIVE_ATTRS: &[&str] = &[
    "accept",
    "accept-charset",
    "align",
    "alink",
    "axis",
    "bgcolor",
    "charset",
    "checked",
    "clear",
    "codetype",
    "color",
    "compact",
    "declare",
    "defer",
    "dir",
    "direction",
    "disabled",
    "enctype",
    "face",
    "frame",
    "hreflang",
    "http-equiv",
    "lang",
    "language",
    "link",
    "media",
    "method",
    "multiple",
    "nohref",
    "noresize",
    "noshade",
    "nowrap",
    "readonly",
    "rel",
    "rev",
    "rules",
    "scope",
    "scrolling",
    "selected",
    "shape",
    "target",
    "text",
    "type",
    "valign",
    "valuetype",
    "vlink",
];

struct Parser<'a> {
    s: &'a str,
    b: &'a [u8],
    pos: usize,
    nesting: u32,
}

fn is_ws(c: u8) -> bool {
    matches!(c, b' ' | b'\t' | b'\n' | b'\r' | b'\x0c')
}

fn is_name_start(c: u8) -> bool {
    c.is_ascii_alphabetic() || c == b'_' || c >= 0x80
}

fn is_name(c: u8) -> bool {
    is_name_start(c) || c.is_ascii_digit() || c == b'-'
}

impl Parser<'_> {
    fn peek(&self) -> Option<u8> {
        self.b.get(self.pos).copied()
    }

    fn peek_at(&self, i: usize) -> Option<u8> {
        self.b.get(self.pos + i).copied()
    }

    fn err(&self, message: impl Into<String>) -> SelectorError {
        SelectorError {
            offset: self.pos,
            message: message.into(),
        }
    }

    fn unexpected(&self) -> SelectorError {
        match self.s[self.pos..].chars().next() {
            Some(c) => self.err(format!("unexpected `{c}`")),
            None => self.err("unexpected end of selector"),
        }
    }

    /// 跳过空白和注释，返回是否有空白（注释本身不算空白）。
    fn skip_ws(&mut self) -> bool {
        let mut ws = false;
        loop {
            match self.peek() {
                Some(c) if is_ws(c) => {
                    ws = true;
                    self.pos += 1;
                }
                Some(b'/') if self.peek_at(1) == Some(b'*') => {
                    self.pos = match memchr::memmem::find(&self.b[self.pos + 2..], b"*/") {
                        Some(i) => self.pos + 2 + i + 2,
                        None => self.b.len(),
                    };
                }
                _ => return ws,
            }
        }
    }

    /// 选择器列表，返回列表和匹配深度。`nested` 时以 `)` 结束（不消耗）。
    fn list(&mut self, relative: bool, nested: bool) -> Result<(Selector, u32), SelectorError> {
        let mut list = Vec::new();
        let mut depth = 0;
        loop {
            self.skip_ws();
            let (c, d) = self.complex(relative)?;
            list.push(c);
            depth = depth.max(d);
            self.skip_ws();
            match self.peek() {
                Some(b',') => self.pos += 1,
                Some(b')') if nested => break,
                None if !nested => break,
                None => return Err(self.err("expected `)`")),
                _ => return Err(self.unexpected()),
            }
        }
        Ok((
            Selector {
                list: list.into_boxed_slice(),
            },
            depth,
        ))
    }

    fn combinator(&mut self) -> Option<Comb> {
        let c = match self.peek()? {
            b'>' => Comb::Child,
            b'+' => Comb::Next,
            b'~' => Comb::Later,
            _ => return None,
        };
        self.pos += 1;
        Some(c)
    }

    /// 复杂选择器。`relative` 时可以以组合器开头（`:has(> a)`），左端补一个锚点。
    fn complex(&mut self, relative: bool) -> Result<(Complex, u32), SelectorError> {
        // 从左往右收集，最后反转成从右往左
        let mut parts: Vec<(Vec<Simple>, u32)> = Vec::new();
        let mut combs: Vec<Comb> = Vec::new();
        if relative {
            let comb = self.combinator().unwrap_or(Comb::Descendant);
            self.skip_ws();
            parts.push((vec![Simple::Anchor], 0));
            combs.push(comb);
        }
        parts.push(self.compound()?);
        loop {
            let ws = self.skip_ws();
            let comb = match self.combinator() {
                Some(c) => {
                    self.skip_ws();
                    c
                }
                None => match self.peek() {
                    None | Some(b',' | b')') => break,
                    Some(_) if ws => Comb::Descendant,
                    Some(_) => return Err(self.unexpected()),
                },
            };
            combs.push(comb);
            parts.push(self.compound()?);
        }
        let depth = parts.iter().map(|(_, d)| 1 + d).sum();
        let n = parts.len();
        let parts = parts
            .into_iter()
            .rev()
            .enumerate()
            .map(|(i, (mut simples, _))| {
                simples.sort_by_key(Simple::cost);
                let tag = simples.iter().find_map(|s| match s {
                    Simple::Type(t, _) if *t != Tag::Other => Some(*t),
                    _ => None,
                });
                Compound {
                    tag,
                    simples: simples.into_boxed_slice(),
                    // 最左边的复合选择器没有左邻，组合器不会被用到
                    comb: if i + 1 < n {
                        combs[n - 2 - i]
                    } else {
                        Comb::Descendant
                    },
                }
            })
            .collect();
        Ok((Complex { parts }, depth))
    }

    /// 复合选择器，返回简单选择器和其中嵌套选择器的最大匹配深度。
    fn compound(&mut self) -> Result<(Vec<Simple>, u32), SelectorError> {
        let mut simples = Vec::new();
        let mut depth = 0;
        let mut any = false;
        match self.peek() {
            Some(b'*') => {
                self.pos += 1;
                any = true;
            }
            _ if self.ident_start() => {
                let name = self.ident()?.to_ascii_lowercase();
                simples.push(Simple::Type(Tag::from_name(&name), name.into()));
                any = true;
            }
            _ => {}
        }
        if self.peek() == Some(b'|') {
            return Err(self.err("namespaces are not supported"));
        }
        loop {
            match self.peek() {
                Some(b'#') => {
                    self.pos += 1;
                    if !self.ident_start() {
                        return Err(self.err("expected an identifier after `#`"));
                    }
                    simples.push(Simple::Id(self.ident()?.into()));
                }
                Some(b'.') => {
                    self.pos += 1;
                    if !self.ident_start() {
                        return Err(self.err("expected an identifier after `.`"));
                    }
                    simples.push(Simple::Class(self.ident()?.into()));
                }
                Some(b'[') => {
                    self.pos += 1;
                    simples.push(self.attr()?);
                }
                Some(b':') => {
                    self.pos += 1;
                    let (s, d) = self.pseudo()?;
                    depth = depth.max(d);
                    simples.extend(s);
                }
                _ => break,
            }
            any = true;
        }
        if !any {
            return Err(match self.peek() {
                None => self.err("expected a selector"),
                Some(_) => self.unexpected(),
            });
        }
        Ok((simples, depth))
    }

    fn attr(&mut self) -> Result<Simple, SelectorError> {
        self.skip_ws();
        if matches!(self.peek(), Some(b'*' | b'|')) {
            return Err(self.err("namespaces are not supported"));
        }
        if !self.ident_start() {
            return Err(self.err("expected an attribute name"));
        }
        let name = self.ident()?.to_ascii_lowercase();
        if self.peek() == Some(b'|') && self.peek_at(1) != Some(b'=') {
            return Err(self.err("namespaces are not supported"));
        }
        self.skip_ws();
        let op = match (self.peek(), self.peek_at(1)) {
            (Some(b']'), _) => {
                self.pos += 1;
                return Ok(Simple::Attr(Box::new(AttrSel {
                    name: name.into(),
                    op: AttrOp::Exists,
                    value: "".into(),
                    ci: false,
                })));
            }
            (Some(b'='), _) => AttrOp::Equals,
            (Some(b'~'), Some(b'=')) => AttrOp::Includes,
            (Some(b'|'), Some(b'=')) => AttrOp::Dash,
            (Some(b'^'), Some(b'=')) => AttrOp::Prefix,
            (Some(b'$'), Some(b'=')) => AttrOp::Suffix,
            (Some(b'*'), Some(b'=')) => AttrOp::Substring,
            (None, _) => return Err(self.err("unclosed attribute selector, expected `]`")),
            _ => return Err(self.unexpected()),
        };
        self.pos += if op == AttrOp::Equals { 1 } else { 2 };
        self.skip_ws();
        let value = match self.peek() {
            Some(q @ (b'"' | b'\'')) => self.string(q)?,
            _ if self.ident_start() => self.ident()?,
            None => return Err(self.err("expected an attribute value")),
            _ => {
                return Err(
                    self.err("expected an identifier or a quoted string as the attribute value")
                );
            }
        };
        self.skip_ws();
        let mut ci = CASE_INSENSITIVE_ATTRS.contains(&name.as_str());
        if self.ident_start() {
            let flag_at = self.pos;
            let flag = self.ident()?;
            if flag.eq_ignore_ascii_case("i") {
                ci = true;
            } else if flag.eq_ignore_ascii_case("s") {
                ci = false;
            } else {
                self.pos = flag_at;
                return Err(self.err(format!("unknown attribute selector flag `{flag}`")));
            }
            self.skip_ws();
        }
        match self.peek() {
            Some(b']') => self.pos += 1,
            None => return Err(self.err("unclosed attribute selector, expected `]`")),
            _ => return Err(self.unexpected()),
        }
        let op = match op {
            AttrOp::Includes if value.is_empty() || value.bytes().any(is_ws) => AttrOp::Never,
            AttrOp::Prefix | AttrOp::Suffix | AttrOp::Substring if value.is_empty() => {
                AttrOp::Never
            }
            op => op,
        };
        Ok(Simple::Attr(Box::new(AttrSel {
            name: name.into(),
            op,
            value: value.into(),
            ci,
        })))
    }

    /// `:` 之后的伪类。`:only-child` 这类展开成两个简单选择器。
    fn pseudo(&mut self) -> Result<(Vec<Simple>, u32), SelectorError> {
        if self.peek() == Some(b':') {
            return Err(self.err("pseudo-elements are not supported"));
        }
        let start = self.pos;
        if !self.ident_start() {
            return Err(self.err("expected a pseudo-class name after `:`"));
        }
        let name = self.ident()?.to_ascii_lowercase();
        if self.peek() == Some(b'(') {
            self.pos += 1;
            return self.pseudo_fn(&name, start);
        }
        let s = match name.as_str() {
            "first-child" => vec![Nth::simple(0, 1, false, false)],
            "last-child" => vec![Nth::simple(0, 1, true, false)],
            "only-child" => vec![
                Nth::simple(0, 1, false, false),
                Nth::simple(0, 1, true, false),
            ],
            "first-of-type" => vec![Nth::simple(0, 1, false, true)],
            "last-of-type" => vec![Nth::simple(0, 1, true, true)],
            "only-of-type" => vec![
                Nth::simple(0, 1, false, true),
                Nth::simple(0, 1, true, true),
            ],
            "empty" => vec![Simple::Empty],
            "root" => vec![Simple::Root],
            "scope" => vec![Simple::Scope],
            _ => {
                self.pos = start;
                return Err(self.err(format!("unsupported pseudo-class `:{name}`")));
            }
        };
        Ok((s, 0))
    }

    fn pseudo_fn(&mut self, name: &str, start: usize) -> Result<(Vec<Simple>, u32), SelectorError> {
        let nested = |p: &mut Self, relative: bool| -> Result<(Selector, u32), SelectorError> {
            if p.nesting >= MAX_NESTING {
                return Err(p.err(format!("selector nested deeper than {MAX_NESTING} levels")));
            }
            p.nesting += 1;
            let r = p.list(relative, true);
            p.nesting -= 1;
            r
        };
        let (simple, depth) = match name {
            "not" => {
                let (s, d) = nested(self, false)?;
                (Simple::Not(s), d)
            }
            "is" | "where" | "matches" | "any" => {
                let (s, d) = nested(self, false)?;
                (Simple::Is(s), d)
            }
            "has" => {
                let (s, d) = nested(self, true)?;
                (Simple::Has(s), d)
            }
            "nth-child" | "nth-last-child" | "nth-of-type" | "nth-last-of-type" => {
                self.skip_ws();
                let (a, b) = self.an_plus_b()?;
                let ws = self.skip_ws();
                let mut nth = Nth {
                    a,
                    b,
                    last: name.starts_with("nth-last"),
                    of_type: name.ends_with("of-type"),
                    of: None,
                };
                let mut depth = 0;
                if !nth.of_type && ws && self.ident_start() {
                    let at = self.pos;
                    if self.ident()?.eq_ignore_ascii_case("of") && self.skip_ws() {
                        let (s, d) = nested(self, false)?;
                        nth.of = Some(s);
                        depth = d;
                    } else {
                        self.pos = at;
                        return Err(self.unexpected());
                    }
                }
                (Simple::Nth(Box::new(nth)), depth)
            }
            _ => {
                self.pos = start;
                return Err(self.err(format!("unsupported pseudo-class `:{name}()`")));
            }
        };
        self.skip_ws();
        match self.peek() {
            Some(b')') => self.pos += 1,
            None => return Err(self.err(format!("unclosed `:{name}(`, expected `)`"))),
            _ => return Err(self.unexpected()),
        }
        Ok((vec![simple], depth))
    }

    /// `An+B` 记法：`odd`、`even`、`3`、`-n+2`、`2n + 1` 等。
    fn an_plus_b(&mut self) -> Result<(i32, i32), SelectorError> {
        let start = self.pos;
        let bad = |p: &mut Self| {
            p.pos = start;
            Err(p.err("invalid An+B expression"))
        };
        if self.ident_start() {
            let at = self.pos;
            let word = self.ident()?;
            if word.eq_ignore_ascii_case("odd") {
                return Ok((2, 1));
            }
            if word.eq_ignore_ascii_case("even") {
                return Ok((2, 0));
            }
            self.pos = at;
        }
        let sign = match self.peek() {
            Some(b'+') => {
                self.pos += 1;
                1
            }
            Some(b'-') => {
                self.pos += 1;
                -1
            }
            _ => 1,
        };
        let digits = self.digits();
        if matches!(self.peek(), Some(b'n' | b'N')) {
            self.pos += 1;
            let a = sign * digits.unwrap_or(1);
            // n 后面紧跟的字符不能再组成标识符（例如 `2nd`）
            if self.peek().is_some_and(|c| is_name(c) && c != b'-') {
                return bad(self);
            }
            let save = self.pos;
            self.skip_ws();
            let bsign = match self.peek() {
                Some(b'+') => 1,
                Some(b'-') => -1,
                _ => {
                    self.pos = save;
                    return Ok((a, 0));
                }
            };
            self.pos += 1;
            self.skip_ws();
            match self.digits() {
                Some(b) => Ok((a, bsign * b)),
                None => bad(self),
            }
        } else {
            match digits {
                Some(b) => Ok((0, sign * b)),
                None => bad(self),
            }
        }
    }

    fn digits(&mut self) -> Option<i32> {
        let start = self.pos;
        let mut v: i32 = 0;
        while let Some(c @ b'0'..=b'9') = self.peek() {
            v = v.saturating_mul(10).saturating_add(i32::from(c - b'0'));
            self.pos += 1;
        }
        (self.pos > start).then_some(v)
    }

    /// 当前位置能否开始一个标识符（CSS 语法的 "would start an identifier"）。
    fn ident_start(&self) -> bool {
        let valid_escape = |i: usize| {
            self.peek_at(i) == Some(b'\\')
                && !matches!(self.peek_at(i + 1), Some(b'\n' | b'\r' | b'\x0c'))
        };
        match self.peek() {
            Some(b'-') => {
                matches!(self.peek_at(1), Some(c) if is_name_start(c) || c == b'-')
                    || valid_escape(1)
            }
            Some(c) if is_name_start(c) => true,
            _ => valid_escape(0),
        }
    }

    fn ident(&mut self) -> Result<String, SelectorError> {
        let mut out = String::new();
        loop {
            match self.peek() {
                Some(b'\\') if !matches!(self.peek_at(1), Some(b'\n' | b'\r' | b'\x0c')) => {
                    self.pos += 1;
                    out.push(self.escape());
                }
                Some(c) if is_name(c) => {
                    let ch = self.s[self.pos..].chars().next().unwrap_or('\u{fffd}');
                    out.push(ch);
                    self.pos += ch.len_utf8();
                }
                _ => break,
            }
        }
        if out.is_empty() {
            return Err(self.err("expected an identifier"));
        }
        Ok(out)
    }

    /// 反斜杠之后的转义：1～6 位十六进制（后面可跟一个空白）或任意一个字符。
    fn escape(&mut self) -> char {
        let start = self.pos;
        let mut v: u32 = 0;
        while self.pos - start < 6
            && let Some(c) = self.peek()
            && c.is_ascii_hexdigit()
        {
            v = v * 16 + (c as char).to_digit(16).unwrap_or(0);
            self.pos += 1;
        }
        if self.pos > start {
            if self.peek() == Some(b'\r') && self.peek_at(1) == Some(b'\n') {
                self.pos += 2;
            } else if self.peek().is_some_and(is_ws) {
                self.pos += 1;
            }
            return match char::from_u32(v) {
                Some(c) if v != 0 => c,
                _ => '\u{fffd}',
            };
        }
        match self.s[self.pos..].chars().next() {
            Some(c) => {
                self.pos += c.len_utf8();
                c
            }
            None => '\u{fffd}',
        }
    }

    /// 引号字符串，`q` 是引号字符。到结尾没闭合也接受（和 CSS 语法一致）。
    fn string(&mut self, q: u8) -> Result<String, SelectorError> {
        self.pos += 1;
        let mut out = String::new();
        loop {
            match self.peek() {
                None => return Ok(out),
                Some(c) if c == q => {
                    self.pos += 1;
                    return Ok(out);
                }
                Some(b'\n' | b'\r' | b'\x0c') => {
                    return Err(self.err("unterminated string"));
                }
                Some(b'\\') => {
                    self.pos += 1;
                    match self.peek() {
                        None => {}
                        Some(b'\r') if self.peek_at(1) == Some(b'\n') => self.pos += 2,
                        Some(b'\n' | b'\r' | b'\x0c') => self.pos += 1,
                        Some(_) => out.push(self.escape()),
                    }
                }
                Some(_) => {
                    let ch = self.s[self.pos..].chars().next().unwrap_or('\u{fffd}');
                    out.push(ch);
                    self.pos += ch.len_utf8();
                }
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const DOC: &str = r##"<!doctype html><html lang="EN"><head><title>T</title></head><body>
        <div id="main" class="wrap  big">
          <h1 class="title">Head</h1>
          <p class="lead intro" data-x="foo-bar">One</p>
          <p>Two <a href="https://a.example/x.pdf" rel="NoFollow">A</a></p>
          <p></p>
          <!-- c -->
          <ul><li>1</li><li class="odd">2</li><li>3</li><li>4</li><li>5</li></ul>
          <span lang="en-US">S</span><em>E</em><span>S2</span>
          <svg><a href="#s">svg-a</a></svg>
          <my-widget data-k="Ü">W</my-widget>
        </div>
        <table><tr><td>c1</td><td>c2</td></tr></table>
    </body></html>"##;

    fn sel(doc: &Html<'_>, s: &str) -> Vec<String> {
        let sel = Selector::parse(s).unwrap_or_else(|e| panic!("{s}: {e}"));
        doc.select(&sel)
            .map(|n| {
                let e = n.as_element().unwrap();
                let t = n.text();
                if t.trim().is_empty() {
                    e.name().to_owned()
                } else {
                    t.trim().to_owned()
                }
            })
            .collect()
    }

    #[test]
    fn simple_selectors() {
        let doc = Html::parse(DOC);
        assert_eq!(sel(&doc, "h1"), ["Head"]);
        assert_eq!(sel(&doc, "H1"), ["Head"]);
        assert_eq!(sel(&doc, "#main > .title"), ["Head"]);
        assert_eq!(sel(&doc, "p.lead.intro"), ["One"]);
        assert_eq!(sel(&doc, ".wrap.big > h1"), ["Head"]);
        assert!(sel(&doc, ".WRAP").is_empty());
        assert_eq!(sel(&doc, "my-widget"), ["W"]);
        assert_eq!(sel(&doc, "svg a"), ["svg-a"]);
        assert_eq!(sel(&doc, "a").len(), 2);
        assert_eq!(sel(&doc, "td:last-child"), ["c2"]);
        assert_eq!(
            sel(&doc, "*").len(),
            doc.root().descendants().filter(|n| n.is_element()).count()
        );
        assert_eq!(sel(&doc, "#m\\61 in > h1"), ["Head"]);
        assert_eq!(sel(&doc, "h1, li.odd, h1"), ["Head", "2"]);
    }

    #[test]
    fn attribute_selectors() {
        let doc = Html::parse(DOC);
        assert_eq!(sel(&doc, "[data-x]"), ["One"]);
        assert_eq!(sel(&doc, "[data-x=foo-bar]"), ["One"]);
        assert_eq!(sel(&doc, "[data-x='foo-bar']"), ["One"]);
        assert_eq!(sel(&doc, "[data-x|=foo]"), ["One"]);
        assert!(sel(&doc, "[data-x|=fo]").is_empty());
        assert_eq!(sel(&doc, "[data-x^=foo]"), ["One"]);
        assert_eq!(sel(&doc, "[data-x$=\"bar\"]"), ["One"]);
        assert_eq!(sel(&doc, "[data-x*='o-b']"), ["One"]);
        assert!(sel(&doc, "[data-x*='O-B']").is_empty());
        assert_eq!(sel(&doc, "[data-x*='O-B' i]"), ["One"]);
        assert_eq!(sel(&doc, "[class~=intro]"), ["One"]);
        assert!(sel(&doc, "[class~='']").is_empty());
        assert!(sel(&doc, "[data-x^='']").is_empty());
        assert_eq!(sel(&doc, "a[href$='.PDF' i]"), ["A"]);
        // rel、lang 属于 HTML 不区分大小写的属性
        assert_eq!(sel(&doc, "a[rel=nofollow]"), ["A"]);
        assert!(sel(&doc, "a[rel=nofollow s]").is_empty());
        assert_eq!(sel(&doc, "span[lang|=EN]"), ["S"]);
        assert_eq!(sel(&doc, "[data-k='Ü']"), ["W"]);
    }

    #[test]
    fn combinators() {
        let doc = Html::parse(DOC);
        assert_eq!(sel(&doc, "body li"), ["1", "2", "3", "4", "5"]);
        assert!(sel(&doc, "body > li").is_empty());
        assert_eq!(sel(&doc, "h1 + p"), ["One"]);
        assert_eq!(sel(&doc, "h1 ~ p"), ["One", "Two A", "p"]);
        assert_eq!(sel(&doc, "li.odd ~ li"), ["3", "4", "5"]);
        assert_eq!(sel(&doc, "li.odd+li"), ["3"]);
        assert_eq!(sel(&doc, "span + em + span"), ["S2"]);
        assert_eq!(sel(&doc, "div p > a"), ["A"]);
        assert_eq!(sel(&doc, "html body div ul > li + li ~ li").len(), 3);
        assert_eq!(sel(&doc, "#main /* c */ h1"), ["Head"]);
    }

    #[test]
    fn pseudo_classes() {
        let doc = Html::parse(DOC);
        assert_eq!(sel(&doc, "li:first-child"), ["1"]);
        assert_eq!(sel(&doc, "li:last-child"), ["5"]);
        assert_eq!(sel(&doc, "li:nth-child(2)"), ["2"]);
        assert_eq!(sel(&doc, "li:nth-child(odd)"), ["1", "3", "5"]);
        assert_eq!(sel(&doc, "li:nth-child(EVEN)"), ["2", "4"]);
        assert_eq!(sel(&doc, "li:nth-child(-n+2)"), ["1", "2"]);
        assert_eq!(sel(&doc, "li:nth-child(2n + 1)"), ["1", "3", "5"]);
        assert_eq!(sel(&doc, "li:nth-child(n+4)"), ["4", "5"]);
        assert_eq!(sel(&doc, "li:nth-last-child(2)"), ["4"]);
        assert_eq!(sel(&doc, "li:nth-child(odd of :not(.odd))"), ["1", "4"]);
        assert_eq!(sel(&doc, "#main > p:nth-of-type(2)"), ["Two A"]);
        assert_eq!(sel(&doc, "#main > span:last-of-type"), ["S2"]);
        assert_eq!(sel(&doc, "#main > :only-of-type").len(), 5);
        assert_eq!(sel(&doc, "title:only-child"), ["T"]);
        assert_eq!(sel(&doc, "#main > p:empty"), ["p"]);
        assert_eq!(sel(&doc, ":root").len(), 1);
        assert_eq!(sel(&doc, "p:not(.lead):not(:empty)"), ["Two A"]);
        assert_eq!(sel(&doc, "p:not(.lead, :empty)"), ["Two A"]);
        assert_eq!(sel(&doc, ":is(h1, li.odd)"), ["Head", "2"]);
        assert_eq!(sel(&doc, ":where(#main) > h1"), ["Head"]);
        assert_eq!(sel(&doc, "p:has(> a)"), ["Two A"]);
        assert_eq!(sel(&doc, "h1:has(+ p.lead)"), ["Head"]);
        assert_eq!(sel(&doc, "li:has(~ li.odd)"), ["1"]);
        assert_eq!(sel(&doc, "div:has(li.odd)").len(), 1);
        assert_eq!(sel(&doc, "body > :not(div, table)").len(), 0);
    }

    #[test]
    fn scope() {
        let doc = Html::parse(DOC);
        let ul = doc.select(&"ul".parse().unwrap()).next().unwrap();
        let s: Selector = ":scope > li:first-child".parse().unwrap();
        assert_eq!(ul.select(&s).map(|n| n.text()).collect::<Vec<_>>(), ["1"]);
        // 后代不含自身
        let s: Selector = "ul".parse().unwrap();
        assert_eq!(ul.select(&s).count(), 0);
        let html = doc.select(&":scope".parse().unwrap()).collect::<Vec<_>>();
        assert_eq!(html.len(), 1);
        assert_eq!(html[0].tag(), Some(Tag::Html));
    }

    #[test]
    fn errors() {
        for (s, msg) in [
            ("", "expected a selector"),
            ("div[", "attribute"),
            ("p:::x", "pseudo-elements"),
            ("p::before", "pseudo-elements"),
            ("a:hover", "unsupported pseudo-class `:hover`"),
            ("a:foo(1)", "unsupported pseudo-class `:foo()`"),
            ("a >", "expected a selector"),
            ("a,", "expected a selector"),
            (",a", "unexpected `,`"),
            ("a > > b", "unexpected `>`"),
            ("svg|a", "namespaces"),
            ("[x=1]", "attribute value"),
            ("[x=y z]", "unknown attribute selector flag"),
            ("li:nth-child(2nd)", "An+B"),
            ("li:nth-child(+ 2)", "An+B"),
            ("li:nth-child(2n+)", "An+B"),
            (":not(a", "expected `)`"),
            (":not()", "unexpected `)`"),
            ("a)", "unexpected `)`"),
            ("#1a", "identifier"),
            ("a.b*", "unexpected `*`"),
            ("[a='x\ny']", "unterminated string"),
        ] {
            let e = Selector::parse(s).expect_err(s);
            assert!(e.to_string().contains(msg), "{s:?}: {e}");
        }
        let deep = format!("{}a{}", ":not(".repeat(40), ")".repeat(40));
        assert!(
            Selector::parse(&deep)
                .unwrap_err()
                .to_string()
                .contains("nested deeper")
        );
        let long = vec!["a"; 300].join(" ");
        assert!(
            Selector::parse(&long)
                .unwrap_err()
                .to_string()
                .contains("too complex")
        );
        assert!(Selector::parse(&vec!["a"; 200].join(" ")).is_ok());
    }

    #[test]
    fn descendant_backtracking_is_not_exponential() {
        let html = format!("{}{}", "<div>".repeat(400), "<span>x</span>");
        let doc = Html::parse(&html);
        let s: Selector = "div div div div div div div div div div p span"
            .parse()
            .unwrap();
        assert_eq!(doc.select(&s).count(), 0);
        let s: Selector = "div div div div div div div div div div span"
            .parse()
            .unwrap();
        assert_eq!(doc.select(&s).count(), 1);
    }
}
