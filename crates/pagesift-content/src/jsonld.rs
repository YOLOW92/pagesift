//! 从 JSON-LD 里取作者和发布、修改时间。支持顶层对象、数组和 `@graph`；解析失败的块直接跳过。

use std::collections::HashMap;

use serde_json::{Map, Value};

use crate::datetime::to_utc_rfc3339;

#[derive(Default)]
pub(crate) struct Info {
    pub author: Option<String>,
    pub published: Option<String>,
    pub modified: Option<String>,
    /// 有文章类的节点（`Article`、`NewsArticle`、`BlogPosting`……）。
    pub article: bool,
}

type Obj = Map<String, Value>;

pub(crate) fn extract(blocks: &[String]) -> Info {
    let mut values = Vec::new();
    for b in blocks {
        // 不含这几个键的块（面包屑、站点搜索框）不用解析
        if ![
            "author",
            "datePublished",
            "dateModified",
            "dateCreated",
            "Article",
            "Posting",
        ]
        .iter()
        .any(|k| b.contains(k))
        {
            continue;
        }
        if let Some(v) = parse(b) {
            values.push(v);
        }
    }
    let mut nodes = Vec::new();
    for v in &values {
        collect(v, &mut nodes, 0);
    }
    let ids: HashMap<&str, &Obj> = nodes
        .iter()
        .filter_map(|n| Some((n.get("@id")?.as_str()?, *n)))
        .collect();
    // 文章类节点优先，其余按出现顺序
    nodes.sort_by_key(|n| !is_article(n));

    let mut info = Info {
        article: nodes.iter().any(|n| is_story(n)) && !nodes.iter().any(|n| is_discussion(n)),
        ..Info::default()
    };
    for n in &nodes {
        if info.published.is_none() {
            info.published = date(n, "datePublished").or_else(|| date(n, "dateCreated"));
        }
        if info.modified.is_none() {
            info.modified = date(n, "dateModified");
        }
        if info.author.is_none() {
            info.author = n.get("author").and_then(|a| author(a, &ids));
        }
    }
    info
}

fn parse(block: &str) -> Option<Value> {
    let s = block.trim();
    if let Ok(v) = serde_json::from_str(s) {
        return Some(v);
    }
    // 常见的包装：HTML 注释、CDATA、末尾分号
    let s = s
        .trim_start_matches("<!--")
        .trim_end_matches("-->")
        .trim()
        .trim_start_matches("//<![CDATA[")
        .trim_end_matches("//]]>")
        .trim()
        .trim_end_matches(';');
    serde_json::from_str(s).ok()
}

fn collect<'a>(v: &'a Value, out: &mut Vec<&'a Obj>, depth: usize) {
    if depth > 4 {
        return;
    }
    match v {
        Value::Array(items) => items.iter().for_each(|i| collect(i, out, depth + 1)),
        Value::Object(o) => {
            out.push(o);
            if let Some(g) = o.get("@graph") {
                collect(g, out, depth + 1);
            }
        }
        _ => {}
    }
}

fn types(n: &Obj) -> impl Iterator<Item = &str> {
    let t = n.get("@type");
    let one = t.and_then(Value::as_str);
    let many = t
        .and_then(Value::as_array)
        .into_iter()
        .flatten()
        .filter_map(Value::as_str);
    one.into_iter().chain(many)
}

/// 一篇文章（新闻、博客、报道）：文章模式用。论坛帖、社交帖、问答页里"评论"就是内容，不算。
fn is_story(n: &Obj) -> bool {
    types(n).any(|t| {
        (t.contains("Article") || t == "BlogPosting" || t == "LiveBlogPosting" || t == "Report")
            && t != "SocialMediaPosting"
    })
}

/// 论坛、问答、社交帖页面（trafilatura 也靠 DiscussionForumPosting 认论坛页）。
fn is_discussion(n: &Obj) -> bool {
    types(n).any(|t| {
        matches!(
            t,
            "DiscussionForumPosting" | "SocialMediaPosting" | "QAPage" | "Question"
        )
    })
}

fn is_article(n: &Obj) -> bool {
    types(n).any(|t| t.contains("Article") || t.contains("Posting") || t == "Report")
}

fn date(n: &Obj, key: &str) -> Option<String> {
    to_utc_rfc3339(n.get(key)?.as_str()?)
}

/// 作者可以是字符串、`{name}`、`{@id}` 引用，或者它们的数组。
fn author(v: &Value, ids: &HashMap<&str, &Obj>) -> Option<String> {
    let mut names: Vec<String> = Vec::new();
    let mut push = |s: &str| {
        let s = s.split_whitespace().collect::<Vec<_>>().join(" ");
        if !s.is_empty() && !s.starts_with("http") && !names.contains(&s) {
            names.push(s);
        }
    };
    let items: Vec<&Value> = match v {
        Value::Array(a) => a.iter().collect(),
        other => vec![other],
    };
    for item in items {
        match item {
            Value::String(s) => push(s),
            Value::Object(o) => {
                let target = match o.get("name") {
                    Some(_) => Some(o),
                    None => o
                        .get("@id")
                        .and_then(Value::as_str)
                        .and_then(|id| ids.get(id).copied()),
                };
                if let Some(name) = target.and_then(|t| t.get("name")).and_then(Value::as_str) {
                    push(name);
                }
            }
            _ => {}
        }
    }
    (!names.is_empty()).then(|| names.join(", "))
}

#[cfg(test)]
mod tests {
    use super::extract;

    fn one(s: &str) -> super::Info {
        extract(&[s.to_string()])
    }

    #[test]
    fn reads_graph_with_id_reference() {
        let info = one(r##"{"@context":"https://schema.org","@graph":[
                {"@type":"WebSite","@id":"#site","name":"Site"},
                {"@type":"Person","@id":"#me","name":"Ada  Lovelace"},
                {"@type":"BlogPosting","author":{"@id":"#me"},
                 "datePublished":"2024-06-24T10:00:00+02:00","dateModified":"2024-06-25"}]}"##);
        assert_eq!(info.author.as_deref(), Some("Ada Lovelace"));
        assert_eq!(info.published.as_deref(), Some("2024-06-24T08:00:00Z"));
        assert_eq!(info.modified.as_deref(), Some("2024-06-25T00:00:00Z"));
    }

    #[test]
    fn reads_array_and_author_list() {
        let info = one(r#"[{"@type":"Organization","name":"Org"},
                {"@type":["NewsArticle"],"author":[{"@type":"Person","name":"A"},"B"],
                 "datePublished":"2024-01-02"}]"#);
        assert_eq!(info.author.as_deref(), Some("A, B"));
        assert_eq!(info.published.as_deref(), Some("2024-01-02T00:00:00Z"));
    }

    #[test]
    fn broken_json_does_not_panic() {
        for s in [
            "{\"author\": ",
            "<!-- {\"author\": {\"name\": 1}} -->",
            "[[[[[[[[[[[[\"author\"]]]]]]]]]]]]",
            "{\"author\": null, \"datePublished\": 5}",
        ] {
            let info = one(s);
            assert!(info.author.is_none() && info.published.is_none(), "{s}");
        }
        let wrapped = one("<!--{\"@type\":\"Article\",\"author\":\"C\"};-->");
        assert_eq!(wrapped.author.as_deref(), Some("C"));
    }
}
