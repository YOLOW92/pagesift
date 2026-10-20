//! XPath 后端：把 pagesift-html 的 DOM 接到 `xpath-eval`（完整的 XPath 1.0）上。

use std::cmp::Ordering;

use pagesift_html::{Html, NodeRef};
use serde_json::Value;
use xpath_eval::{EvaluationContext, ExpandedName, Expr, NodeKind, evaluate, parse};

use crate::engine::{self, Backend, Compiled, Role};
use crate::text::node_text;
use crate::{Error, Schema};

/// 文档嵌套超过这个深度就拒绝求值：`xpath-eval` 的后代轴是递归实现。
/// pagesift-html 建树时已经把深度限制在这以内，这里是兜底。
const MAX_DEPTH: usize = 1024;
const MAX_SELECTOR_LEN: usize = 4096;
/// 括号嵌套上限，防止解析器递归过深。
const MAX_NESTING: usize = 64;

/// 按 XPath schema 提取，对应 Crawl4AI 的 `JsonXPathExtractionStrategy`。
pub struct JsonXPathExtractor {
    compiled: Compiled<Expr>,
}

impl JsonXPathExtractor {
    pub fn new(schema: &Schema) -> Result<Self, Error> {
        let compiled = Compiled::compile(schema, &mut |sel, path, role| {
            let err = |message: String| Error::Selector {
                path: path.to_owned(),
                selector: sel.to_owned(),
                message,
            };
            check_limits(sel).map_err(err)?;
            let text = match role {
                Role::Base => sel.trim().to_owned(),
                Role::Field => field_xpath(sel.trim()),
                Role::Source => source_xpath(sel),
            };
            parse(&text).map_err(|e| err(format!("{} at offset {}", e.message, e.position)))
        })?;
        Ok(JsonXPathExtractor { compiled })
    }

    pub fn extract(&self, html: &str) -> Result<Vec<Value>, Error> {
        self.extract_document(&Html::parse(html))
    }

    pub fn extract_document(&self, doc: &Html<'_>) -> Result<Vec<Value>, Error> {
        let root = doc.root();
        let (order, depth) = document_order(doc);
        if depth > MAX_DEPTH {
            return Err(Error::TooDeep {
                depth,
                limit: MAX_DEPTH,
            });
        }
        let backend = XPath {
            root: XNode::new(root, &order),
        };
        engine::run(&backend, &self.compiled)
    }
}

fn check_limits(sel: &str) -> Result<(), String> {
    if sel.len() > MAX_SELECTOR_LEN {
        return Err(format!("longer than {MAX_SELECTOR_LEN} bytes"));
    }
    let (mut depth, mut max) = (0usize, 0usize);
    for b in sel.bytes() {
        match b {
            b'(' | b'[' => {
                depth += 1;
                max = max.max(depth);
            }
            b')' | b']' => depth = depth.saturating_sub(1),
            _ => {}
        }
    }
    if max > MAX_NESTING {
        return Err(format!("nested deeper than {MAX_NESTING} levels"));
    }
    Ok(())
}

/// 字段选择器的写法和 Crawl4AI 一致：不含 `/` 的当作简单 CSS 转换，`/` 开头的加 `.` 变成相对路径。
fn field_xpath(sel: &str) -> String {
    let xpath = if sel.contains('/') {
        sel.to_owned()
    } else if sel.contains(" > ") {
        format!("//{}", sel.split(" > ").collect::<Vec<_>>().join("/"))
    } else if sel.contains(' ') {
        format!("//{}", sel.split(' ').collect::<Vec<_>>().join("//"))
    } else {
        format!("//{sel}")
    };
    if xpath.starts_with('/') {
        format!(".{xpath}")
    } else {
        xpath
    }
}

/// `"+ tr.subtext"` 转成后面第一个匹配的兄弟元素。
fn source_xpath(sel: &str) -> String {
    let sel = sel.trim_start().trim_start_matches('+').trim();
    let mut parts = sel.split('.');
    let tag = match parts.next().map(str::trim) {
        Some(t) if !t.is_empty() => t,
        _ => "*",
    };
    let mut xpath = format!("following-sibling::{tag}");
    for cls in parts.map(str::trim).filter(|c| !c.is_empty()) {
        xpath.push_str(&format!(
            "[contains(concat(' ',normalize-space(@class),' '),' {cls} ')]"
        ));
    }
    xpath.push_str("[1]");
    xpath
}

/// 按节点下标索引的先序编号。建树时节点可能被挪动（foster parenting），下标不一定是文档顺序。
type Order = [u32];

/// 先序遍历编号即文档顺序；同时算出最大深度（根节点算第 1 层）。
fn document_order(doc: &Html<'_>) -> (Vec<u32>, usize) {
    let mut order = vec![u32::MAX; doc.len()];
    // depth[i] 是节点 i 的深度，子节点的深度由父节点推出
    let mut depth = vec![0u32; doc.len()];
    let mut max = 0;
    for (i, n) in doc.root().descendants().enumerate() {
        order[n.id().index()] = i as u32;
        let d = n.parent().map_or(1, |p| depth[p.id().index()] + 1);
        depth[n.id().index()] = d;
        max = max.max(d);
    }
    (order, max as usize)
}

const NO_ATTR: u32 = u32::MAX;

#[derive(Clone, Copy)]
struct XNode<'a> {
    node: NodeRef<'a>,
    attr: u32,
    order: &'a Order,
}

impl<'a> XNode<'a> {
    fn new(node: NodeRef<'a>, order: &'a Order) -> Self {
        XNode {
            node,
            attr: NO_ATTR,
            order,
        }
    }

    fn attr_pair(self) -> Option<(&'a str, &'a str)> {
        if self.attr == NO_ATTR {
            return None;
        }
        self.node.as_element()?.attrs().nth(self.attr as usize)
    }

    fn key(self) -> (u32, u32) {
        let pos = self.order[self.node.id().index()];
        (pos, self.attr.wrapping_add(1))
    }
}

impl PartialEq for XNode<'_> {
    fn eq(&self, other: &Self) -> bool {
        self.node.id() == other.node.id() && self.attr == other.attr
    }
}

impl Eq for XNode<'_> {}

impl<'a> xpath_eval::Node<'a> for XNode<'a> {
    fn kind(self) -> NodeKind {
        if self.attr != NO_ATTR {
            return NodeKind::Attribute;
        }
        let n = self.node;
        if n.is_element() {
            NodeKind::Element
        } else if n.is_text() {
            NodeKind::Text
        } else if n.is_comment() {
            NodeKind::Comment
        } else {
            // 文档和 doctype
            NodeKind::Root
        }
    }

    fn parent(self) -> Option<Self> {
        if self.attr != NO_ATTR {
            return Some(XNode::new(self.node, self.order));
        }
        self.node.parent().map(|p| XNode::new(p, self.order))
    }

    fn children(self) -> impl Iterator<Item = Self> + 'a {
        let order = self.order;
        let it = (self.attr == NO_ATTR).then(|| self.node.children());
        it.into_iter()
            .flatten()
            .filter(|c| c.is_element() || c.is_text() || c.is_comment())
            .map(move |c| XNode::new(c, order))
    }

    fn attributes(self) -> impl Iterator<Item = Self> + 'a {
        let n = match (self.attr, self.node.as_element()) {
            (NO_ATTR, Some(e)) => e.attrs().count() as u32,
            _ => 0,
        };
        let (node, order) = (self.node, self.order);
        (0..n).map(move |attr| XNode { node, attr, order })
    }

    fn namespaces(self) -> impl Iterator<Item = Self> + 'a {
        std::iter::empty()
    }

    fn expanded_name(self) -> Option<ExpandedName> {
        // 和 lxml.html 一样不区分命名空间，名字测试只看本地名
        let local = if self.attr != NO_ATTR {
            self.attr_pair()?.0
        } else {
            self.node.as_element()?.name()
        };
        Some(ExpandedName {
            namespace_uri: None,
            local_name: local.to_owned(),
        })
    }

    fn string_value(self) -> String {
        if self.attr != NO_ATTR {
            return self
                .attr_pair()
                .map(|(_, v)| v.to_owned())
                .unwrap_or_default();
        }
        match self.node.as_text().or_else(|| self.node.as_comment()) {
            Some(t) => t.to_owned(),
            None => self.node.text(),
        }
    }

    fn document_order(self, other: Self) -> Ordering {
        self.key().cmp(&other.key())
    }
}

#[derive(Clone)]
enum Item<'a> {
    Node(XNode<'a>),
    /// XPath 返回的是字符串、数字或布尔值，例如 `string(@href)`、`count(li)`。
    Scalar(String),
}

struct XPath<'a> {
    root: XNode<'a>,
}

impl<'a> XPath<'a> {
    fn eval(&self, ctx: XNode<'a>, expr: &Expr, path: &str) -> Result<Vec<Item<'a>>, Error> {
        let v = evaluate(expr, &EvaluationContext::new(ctx)).map_err(|e| Error::XPath {
            path: path.to_owned(),
            message: e.to_string(),
        })?;
        Ok(match v {
            xpath_eval::Value::NodeSet(nodes) => nodes.into_iter().map(Item::Node).collect(),
            other => vec![Item::Scalar(other.to_xpath_string())],
        })
    }
}

impl<'a> Backend for XPath<'a> {
    type Item = Item<'a>;
    type Sel = Expr;

    fn select_base(&self, sel: &Expr) -> Result<Vec<Item<'a>>, Error> {
        self.eval(self.root, sel, "baseSelector")
    }

    fn select(
        &self,
        ctx: &Item<'a>,
        sel: &Expr,
        path: &str,
        first_only: bool,
    ) -> Result<Vec<Item<'a>>, Error> {
        let Item::Node(n) = ctx else {
            return Ok(Vec::new());
        };
        let mut v = self.eval(*n, sel, path)?;
        if first_only {
            v.truncate(1);
        }
        Ok(v)
    }

    fn sibling(&self, ctx: &Item<'a>, sel: &Expr, path: &str) -> Result<Option<Item<'a>>, Error> {
        Ok(self.select(ctx, sel, path, true)?.into_iter().next())
    }

    fn text(&self, item: &Item<'a>) -> String {
        match item {
            Item::Node(n) if n.attr != NO_ATTR => n
                .attr_pair()
                .map(|(_, v)| v.trim().to_owned())
                .unwrap_or_default(),
            Item::Node(n) => node_text(n.node),
            Item::Scalar(s) => s.trim().to_owned(),
        }
    }

    fn html(&self, item: &Item<'a>) -> String {
        match item {
            Item::Node(n) if n.attr == NO_ATTR && n.node.is_element() => n.node.html(),
            Item::Node(n) if n.attr == NO_ATTR => node_text(n.node),
            other => self.text(other),
        }
    }

    fn attr(&self, item: &Item<'a>, name: &str) -> Option<String> {
        match item {
            Item::Node(n) if n.attr == NO_ATTR => {
                n.node.as_element()?.attr(name).map(str::to_owned)
            }
            _ => None,
        }
    }
}

#[cfg(test)]
mod tests {
    use serde_json::json;

    use super::*;

    fn run(schema: Value, html: &str) -> Result<Vec<Value>, Error> {
        JsonXPathExtractor::new(&Schema::from_json(&schema).unwrap())?.extract(html)
    }

    #[test]
    fn common_subset() {
        let html = r#"<ul id="l"><li class="a x" data-n="1">Apple <b>pie</b></li><li class="b">Banana</li>
            <li class="a">Avocado<a href="/av">link</a></li><!-- c --></ul>"#;
        let names = |xp: &str| {
            run(
                json!({"baseSelector": xp, "fields": [{"name": "t", "type": "text"}]}),
                html,
            )
            .unwrap()
            .into_iter()
            .map(|v| v["t"].as_str().unwrap().to_owned())
            .collect::<Vec<_>>()
        };
        assert_eq!(names("//li[@class='b']"), ["Banana"]);
        assert_eq!(names("//ul/li[2]"), ["Banana"]);
        assert_eq!(names("//li[last()]"), ["Avocadolink"]);
        assert_eq!(names("//li[contains(@class, 'a')]").len(), 2);
        assert_eq!(names("//li[starts-with(text(), 'Ban')]"), ["Banana"]);
        assert_eq!(names("//li[text()='Banana']"), ["Banana"]);
        assert_eq!(names("//b/.."), ["Apple pie"]);
        assert_eq!(names("//*[@id='l']/li[1]/@data-n"), ["1"]);
        assert_eq!(names("//li/text()"), ["Apple", "Banana", "Avocado"]);
        assert_eq!(names("//a/ancestor::li"), ["Avocadolink"]);
        assert_eq!(names("//li[position() > 1 and @class]").len(), 2);
    }

    #[test]
    fn fields_scalars_and_sources() {
        let html = r#"<table><tr class="row"><td><a href="/1">One</a></td></tr><tr class="sub"><td>5 pts</td></tr>
            <tr class="row"><td><a href="/2">Two</a></td></tr></table>"#;
        let out = run(
            json!({"baseSelector": "//tr[@class='row']", "fields": [
                {"name": "title", "selector": ".//a", "type": "text"},
                {"name": "href", "selector": ".//a", "type": "attribute", "attribute": "href"},
                {"name": "href2", "selector": "string(.//a/@href)", "type": "text"},
                {"name": "td", "selector": "td", "type": "text", "transform": "uppercase"},
                {"name": "pts", "source": "+ tr.sub", "selector": "td", "type": "regex", "pattern": "(\\d+)", "default": "0"}
            ]}),
            html,
        )
        .unwrap();
        assert_eq!(
            out[0],
            json!({"title": "One", "href": "/1", "href2": "/1", "td": "ONE", "pts": "5"})
        );
        assert_eq!(
            out[1],
            json!({"title": "Two", "href": "/2", "href2": "/2", "td": "TWO", "pts": "0"})
        );
    }

    #[test]
    fn errors_instead_of_panics() {
        let e = |s: Value| run(s, "<p>x</p>").unwrap_err().to_string();
        assert!(e(json!({"baseSelector": "//p[", "fields": []})).contains("`baseSelector`"));
        assert!(
            e(json!({"baseSelector": "//p[foo()]", "fields": []})).contains("unknown function")
        );
        let deep = format!("//p{}", "[(".repeat(100));
        assert!(e(json!({"baseSelector": deep, "fields": []})).contains("nested deeper"));
        // pagesift-html 建树时限制了深度，深层嵌套的页面不会再触发 TooDeep
        let html = "<div>".repeat(2000);
        let schema = json!({"baseSelector": "//div", "fields": [{"name": "t", "type": "text"}]});
        assert_eq!(run(schema, &html).unwrap().len(), 2000);
    }
}
