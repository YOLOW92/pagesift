use pagesift_html::{Html, NodeRef, Selector};
use serde_json::Value;

use crate::engine::{self, Backend, Compiled, Role};
use crate::text::node_text;
use crate::{Error, Schema};

/// 按 CSS schema 提取，对应 Crawl4AI 的 `JsonCssExtractionStrategy`。
/// 选择器在构造时编译一次，之后可以在多个页面、多个线程上复用。
pub struct JsonCssExtractor {
    compiled: Compiled<Selector>,
}

impl JsonCssExtractor {
    pub fn new(schema: &Schema) -> Result<Self, Error> {
        let compiled = Compiled::compile(schema, &mut |sel, path, role| {
            let text = match role {
                Role::Base => sel.trim().to_owned(),
                // BeautifulSoup 允许 "> li" 这种以组合符开头的相对选择器
                Role::Field if sel.trim_start().starts_with('>') => {
                    format!(":scope {}", sel.trim())
                }
                Role::Field => sel.trim().to_owned(),
                Role::Source => sel.trim_start().trim_start_matches('+').trim().to_owned(),
            };
            Selector::parse(&text).map_err(|e| Error::Selector {
                path: path.to_owned(),
                selector: sel.to_owned(),
                message: e.to_string(),
            })
        })?;
        Ok(JsonCssExtractor { compiled })
    }

    /// 解析 HTML 并提取，每个 base 元素一个 JSON 对象；没有取到任何字段的元素会被跳过。
    pub fn extract(&self, html: &str) -> Vec<Value> {
        self.extract_document(&Html::parse(html))
    }

    /// 在已经解析好的文档上提取，避免重复解析。
    pub fn extract_document(&self, doc: &Html<'_>) -> Vec<Value> {
        // CSS 后端的所有操作都不会失败
        engine::run(&Css { doc }, &self.compiled).unwrap_or_default()
    }
}

struct Css<'d> {
    doc: &'d Html<'d>,
}

impl<'d> Backend for Css<'d> {
    type Item = NodeRef<'d>;
    type Sel = Selector;

    fn select_base(&self, sel: &Selector) -> Result<Vec<NodeRef<'d>>, Error> {
        Ok(self.doc.select(sel).collect())
    }

    fn select(
        &self,
        ctx: &NodeRef<'d>,
        sel: &Selector,
        _path: &str,
        first_only: bool,
    ) -> Result<Vec<NodeRef<'d>>, Error> {
        let mut it = ctx.select(sel);
        Ok(if first_only {
            it.next().into_iter().collect()
        } else {
            it.collect()
        })
    }

    fn sibling(
        &self,
        ctx: &NodeRef<'d>,
        sel: &Selector,
        _path: &str,
    ) -> Result<Option<NodeRef<'d>>, Error> {
        let mut cur = ctx.next_sibling();
        while let Some(n) = cur {
            if sel.matches(n) {
                return Ok(Some(n));
            }
            cur = n.next_sibling();
        }
        Ok(None)
    }

    fn text(&self, item: &NodeRef<'d>) -> String {
        node_text(*item)
    }

    fn html(&self, item: &NodeRef<'d>) -> String {
        item.html()
    }

    fn attr(&self, item: &NodeRef<'d>, name: &str) -> Option<String> {
        item.as_element()?.attr(name).map(str::to_owned)
    }
}

#[cfg(test)]
mod tests {
    use serde_json::json;

    use super::*;

    fn run(schema: Value, html: &str) -> Vec<Value> {
        JsonCssExtractor::new(&Schema::from_json(&schema).unwrap())
            .unwrap()
            .extract(html)
    }

    #[test]
    fn field_types_defaults_and_transforms() {
        let html = r#"<div class="p" data-id="7"><h2> Widget  <b>Pro</b> </h2><span class="price">$1,299.00</span>
            <a href="/w?id=42">x</a><ul><li>red</li><li>blue</li></ul><div class="m"><i>ACME</i></div></div>
            <div class="p" data-id="8"><h2>Other</h2></div>"#;
        let out = run(
            json!({
                "baseSelector": "div.p",
                "baseFields": [{"name": "id", "type": "attribute", "attribute": "data-id"}],
                "fields": [
                    {"name": "title", "selector": "h2", "type": "text", "transform": "lowercase"},
                    {"name": "price", "selector": ".price", "type": "regex", "pattern": "([\\d,.]+)", "default": "n/a"},
                    {"name": "pid", "selector": "a", "type": ["attribute", "regex"], "attribute": "href", "pattern": "id=(\\d+)"},
                    {"name": "missing", "selector": ".nope", "type": "text"},
                    {"name": "dflt", "selector": ".nope", "type": "text", "default": 0},
                    {"name": "colors", "selector": "li", "type": "list", "fields": [{"name": "c", "type": "text"}]},
                    {"name": "maker", "selector": ".m", "type": "nested", "fields": [{"name": "brand", "selector": "i", "type": "text"}]},
                    {"name": "raw", "selector": "i", "type": "html"},
                    {"name": "calc", "type": "computed", "expression": "x", "default": "off"}
                ]
            }),
            html,
        );
        assert_eq!(
            out[0],
            json!({"id": "7", "title": "widget pro", "price": "1,299.00", "pid": "42", "dflt": 0,
                   "colors": [{"c": "red"}, {"c": "blue"}], "maker": {"brand": "ACME"}, "raw": "<i>ACME</i>", "calc": "off"})
        );
        assert_eq!(
            out[1],
            json!({"id": "8", "title": "other", "price": "n/a", "dflt": 0, "colors": [], "maker": {}, "calc": "off"})
        );
    }

    #[test]
    fn sibling_source_and_relative_child_selector() {
        let html = r#"<table><tr class="athing"><td><a href="/1">One</a></td></tr><tr><td class="sub"><span>10 points</span></td></tr>
            <tr class="athing"><td><a href="/2">Two</a></td></tr></table><ul class="t"><li>a<ul><li>nested</li></ul></li></ul>"#;
        let out = run(
            json!({"baseSelector": "tr.athing", "fields": [
                {"name": "t", "selector": "a", "type": "text"},
                {"name": "pts", "source": "+ tr", "selector": "span", "type": "regex", "pattern": "(\\d+)", "default": "0"}
            ]}),
            html,
        );
        assert_eq!(
            out,
            vec![
                json!({"t": "One", "pts": "10"}),
                json!({"t": "Two", "pts": "0"})
            ]
        );
        let out = run(
            json!({"baseSelector": "ul.t", "fields": [{"name": "n", "selector": "> li", "type": "list", "fields": [{"name": "x", "type": "text"}]}]}),
            html,
        );
        assert_eq!(out, vec![json!({"n": [{"x": "anested"}]})]);
    }

    #[test]
    fn invalid_selectors_and_regex_are_errors() {
        let e = |s: Value| {
            JsonCssExtractor::new(&Schema::from_json(&s).unwrap())
                .err()
                .unwrap()
                .to_string()
        };
        assert!(e(json!({"baseSelector": "div[", "fields": []})).contains("`baseSelector`"));
        assert!(e(json!({"baseSelector": "div", "fields": [{"name": "a", "type": "text", "selector": "p:::x"}]})).contains("`fields[0].selector`"));
        assert!(e(json!({"baseSelector": "div", "fields": [{"name": "a", "type": "regex", "pattern": "(?<=a)b"}]})).contains("look-around"));
        assert!(e(json!({"baseSelector": "div", "fields": [{"name": "a", "type": "regex", "pattern": "ab"}]})).contains("group 1 does not exist"));
    }

    #[test]
    fn empty_items_are_dropped() {
        let out = run(
            json!({"baseSelector": "p", "fields": [{"name": "a", "selector": "a", "type": "text"}]}),
            "<p>x</p><p><a>y</a></p>",
        );
        assert_eq!(out, vec![json!({"a": "y"})]);
    }
}
