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

use crate::{Descendants, Html, NodeRef, Tag};

/// 函数式伪类（`:not(` 等）的最大嵌套层数，限制解析器的递归深度。
pub(super) const MAX_NESTING: u32 = 32;
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
pub(super) struct Complex {
    pub(super) parts: Box<[Compound]>,
}

#[derive(Debug, Clone)]
pub(super) struct Compound {
    /// 含已知标签的类型选择器时就是那个标签：匹配前先比节点的标签，多数节点不用构造 [`Element`]。
    pub(super) tag: Option<Tag>,
    pub(super) simples: Box<[Simple]>,
    pub(super) comb: Comb,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) enum Comb {
    Descendant,
    Child,
    Next,
    Later,
}

#[derive(Debug, Clone)]
pub(super) enum Simple {
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
    pub(super) fn cost(&self) -> u8 {
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
pub(super) struct AttrSel {
    pub(super) name: Box<str>,
    pub(super) op: AttrOp,
    pub(super) value: Box<str>,
    /// 值按 ASCII 不区分大小写比较。
    pub(super) ci: bool,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) enum AttrOp {
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
pub(super) struct Nth {
    pub(super) a: i32,
    pub(super) b: i32,
    /// 从后往前数。
    pub(super) last: bool,
    /// 只数同类型的兄弟。
    pub(super) of_type: bool,
    /// `of S`：只数匹配 S 的兄弟，自身也要匹配 S。
    pub(super) of: Option<Selector>,
}

impl Nth {
    pub(super) fn simple(a: i32, b: i32, last: bool, of_type: bool) -> Simple {
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
    pub(super) fn matches_in(&self, el: NodeRef<'_>, cx: &Ctx) -> bool {
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

mod matcher;
mod parse;

use matcher::{Ctx, Res, match_complex};
use parse::Parser;

/// HTML 规范里值不区分大小写的属性（selectors 对 HTML 元素也按这张表处理）。
pub(super) const CASE_INSENSITIVE_ATTRS: &[&str] = &[
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
