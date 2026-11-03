//! `pagesift.toml` 的 `[[rules]]`：按主机和路径匹配，编译成钩子。
//!
//! ```toml
//! [[rules]]
//! match = { host = "example.com", path_glob = "/blog/**" }
//! headers = { "Accept-Language" = "en" }
//! include_tags = ["article"]
//! exclude_tags = [".ads"]
//! engine = "http"
//! schema = { baseSelector = "li.post", fields = [{ name = "title", selector = "h2", type = "text" }] }
//! drop_fields = ["links", "extract.internal_id"]
//! rename_fields = { title = "headline" }   # extract 结果里的字段改名
//! priority = 0
//! ```
//!
//! 每张表都 `deny_unknown_fields`：写错的键在启动时报错，不悄悄忽略。主机写 `example.com` 时也匹配
//! 子域名（`*.example.com`）。`path_glob`：`*` 匹配一段路径里的任意字符，`**` 跨段，`?` 一个字符。

use std::collections::BTreeMap;
use std::sync::Arc;

use pagesift_api::types::{Document, Format};
use serde::Deserialize;
use serde_json::Value;

use super::{DocumentHook, Flow, Hooks, Request, RequestHook};

#[derive(Debug, Clone, Default, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Match {
    #[serde(default)]
    pub host: Option<String>,
    #[serde(default)]
    pub path_glob: Option<String>,
}

#[derive(Debug, Clone, Default, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Rule {
    #[serde(rename = "match", default)]
    pub matches: Match,
    #[serde(default)]
    pub headers: BTreeMap<String, String>,
    #[serde(default)]
    pub include_tags: Vec<String>,
    #[serde(default)]
    pub exclude_tags: Vec<String>,
    #[serde(default)]
    pub engine: Option<String>,
    /// 抽取 schema（`extract` 格式）。
    #[serde(default)]
    pub schema: Option<Value>,
    #[serde(default)]
    pub drop_fields: Vec<String>,
    #[serde(default)]
    pub rename_fields: BTreeMap<String, String>,
    #[serde(default)]
    pub priority: i32,
}

/// 文档里能删的顶层字段（`extract.<名字>` 删抽取结果里的字段）。
const DROPPABLE: &[&str] = &[
    "markdown", "html", "rawHtml", "links", "images", "extract", "summary", "json", "chunks",
];

impl Rule {
    fn check(&self) -> Result<(), String> {
        if let Some(e) = &self.engine
            && !matches!(e.as_str(), "http" | "browser" | "auto")
        {
            return Err(format!(
                "rules: engine must be http, browser or auto, got {e:?}"
            ));
        }
        for f in &self.drop_fields {
            let ok = DROPPABLE.contains(&f.as_str())
                || f.strip_prefix("extract.").is_some_and(|k| !k.is_empty());
            if !ok {
                return Err(format!(
                    "rules: drop_fields entry {f:?} is not a document field ({}, or extract.<field>)",
                    DROPPABLE.join(", ")
                ));
            }
        }
        if let Some(s) = &self.schema
            && !s.is_object()
        {
            return Err("rules: schema must be a table".into());
        }
        Ok(())
    }

    fn applies(&self, url: &str) -> bool {
        let Ok(u) = url::Url::parse(url) else {
            return false;
        };
        let host_ok = self.matches.host.as_deref().is_none_or(|want| {
            let h = u.host_str().unwrap_or_default().to_ascii_lowercase();
            let want = want.to_ascii_lowercase();
            h == want || h.ends_with(&format!(".{want}"))
        });
        host_ok
            && self
                .matches
                .path_glob
                .as_deref()
                .is_none_or(|g| glob(g.as_bytes(), u.path().as_bytes()))
    }
}

/// `*` 一段内任意、`**` 跨段任意、`?` 一个字符（不跨 `/`）。
fn glob(p: &[u8], s: &[u8]) -> bool {
    match p {
        [] => s.is_empty(),
        [b'*', b'*', rest @ ..] => (0..=s.len()).any(|i| glob(rest, &s[i..])),
        [b'*', rest @ ..] => (0..=s.len())
            .take_while(|&i| i == 0 || s[i - 1] != b'/')
            .any(|i| glob(rest, &s[i..])),
        [b'?', rest @ ..] => s.first().is_some_and(|c| *c != b'/') && glob(rest, &s[1..]),
        [c, rest @ ..] => s.first() == Some(c) && glob(rest, &s[1..]),
    }
}

struct RuleHook(Rule);

impl RequestHook for RuleHook {
    fn before(&self, req: &Request) -> Flow<Request> {
        let r = &self.0;
        if !r.applies(&req.url) {
            return Flow::Pass;
        }
        let mut out = req.clone();
        let o = &mut out.options;
        for (k, v) in &r.headers {
            o.headers.insert(k.clone(), v.clone());
        }
        o.include_tags.extend(r.include_tags.iter().cloned());
        o.exclude_tags.extend(r.exclude_tags.iter().cloned());
        if let Some(e) = &r.engine {
            o.engine = Some(e.clone());
        }
        if let Some(s) = &r.schema {
            o.formats.retain(|f| f.kind != "extract");
            let mut f = Format::new("extract");
            f.options.insert("schema".into(), s.clone());
            o.formats.push(f);
        }
        Flow::Replace(out)
    }
}

impl DocumentHook for RuleHook {
    fn document(&self, url: &str, doc: &Document) -> Flow<Document> {
        let r = &self.0;
        if !r.applies(url) || (r.drop_fields.is_empty() && r.rename_fields.is_empty()) {
            return Flow::Pass;
        }
        let mut d = doc.clone();
        for f in &r.drop_fields {
            match f.as_str() {
                "markdown" => d.markdown = None,
                "html" => d.html = None,
                "rawHtml" => d.raw_html = None,
                "links" => d.links = None,
                "images" => d.images = None,
                "extract" => d.extract = None,
                "summary" => d.summary = None,
                "json" => d.json = None,
                "chunks" => d.chunks = None,
                other => {
                    if let (Some(k), Some(Value::Array(items))) =
                        (other.strip_prefix("extract."), d.extract.as_mut())
                    {
                        for it in items.iter_mut().filter_map(Value::as_object_mut) {
                            it.remove(k);
                        }
                    }
                }
            }
        }
        if let Some(Value::Array(items)) = d.extract.as_mut() {
            for it in items.iter_mut().filter_map(Value::as_object_mut) {
                for (from, to) in &r.rename_fields {
                    if let Some(v) = it.remove(from) {
                        it.insert(to.clone(), v);
                    }
                }
            }
        }
        Flow::Replace(d)
    }
}

/// 把规则编译成钩子；写错的规则报错（启动时）。
pub fn compile(rules: &[Rule]) -> Result<Hooks, String> {
    let mut h = Hooks::default();
    for r in rules {
        r.check()?;
        let hook = Arc::new(RuleHook(r.clone()));
        h.request(r.priority, hook.clone());
        h.document(r.priority, hook);
    }
    Ok(h)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[derive(Deserialize)]
    #[serde(deny_unknown_fields)]
    struct File {
        rules: Vec<Rule>,
    }

    #[test]
    fn glob_matching() {
        assert!(glob(b"/blog/**", b"/blog/2026/11/post"));
        assert!(glob(b"/blog/*", b"/blog/post"));
        assert!(!glob(b"/blog/*", b"/blog/2026/post"));
        assert!(glob(b"/p?ge", b"/page"));
    }

    #[test]
    fn toml_rules_compile_and_apply() {
        let f: File = toml::from_str(r#"
            [[rules]]
            match = { host = "example.com", path_glob = "/shop/**" }
            headers = { "Accept-Language" = "de" }
            exclude_tags = [".ads"]
            engine = "http"
            schema = { baseSelector = "li", fields = [{ name = "sku", selector = "b", type = "text" }] }
            drop_fields = ["links", "extract.sku"]
            rename_fields = { title = "name" }
        "#).unwrap();
        let hooks = compile(&f.rules).unwrap();
        let req = hooks
            .run_request(Request {
                url: "https://www.example.com/shop/a/b".into(),
                options: Default::default(),
            })
            .unwrap();
        assert_eq!(req.options.headers["Accept-Language"], "de");
        assert_eq!(req.options.engine.as_deref(), Some("http"));
        assert!(req.options.wants("extract"));
        let other = hooks
            .run_request(Request {
                url: "https://example.com/about".into(),
                options: Default::default(),
            })
            .unwrap();
        assert!(other.options.headers.is_empty());
        let doc = Document {
            links: Some(vec!["x".into()]),
            extract: Some(serde_json::json!([{"title": "T", "sku": "1"}])),
            ..Default::default()
        };
        let out = hooks.run_document("https://example.com/shop/x", doc);
        assert!(out.links.is_none());
        assert_eq!(out.extract.unwrap(), serde_json::json!([{"name": "T"}]));
        assert!(toml::from_str::<File>("[[rules]]\nheader = {}").is_err());
        assert!(
            compile(&[Rule {
                drop_fields: vec!["metadata".into()],
                ..Default::default()
            }])
            .is_err()
        );
    }
}
