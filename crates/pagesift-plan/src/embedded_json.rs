//! `embedded-json` 执行器：前端框架把正文放在页面里的 JSON 中（Next.js 的 `__NEXT_DATA__`、
//! Nuxt 的 `__NUXT_DATA__`、schema.org 的 `application/ld+json`），HTTP 拿到的 HTML 骨架里没有正文时，
//! 不用浏览器也能取出来。它只产出候选，用不用由质量后验决定。
//!
//! 哪些字符串是"正文"：同一页所有字符串叶子的长度分布里，长度超过 `q(1−δ)` 的离群值，
//! 并且像自然语言（含空白分隔的词）。不设固定的字数门槛。字符串本身是 HTML 时按 HTML 提取。

use pagesift_stat::Delta;
use serde_json::Value;

use crate::block::{Block, Kind, split};

/// 页面里能找到的 JSON 数据岛（原文）。
pub fn islands(html: &str) -> Vec<&str> {
    let mut out = vec![];
    let mut rest = html;
    while let Some(at) = find_ci(rest, "<script") {
        let tag_end = match rest[at..].find('>') {
            Some(e) => at + e + 1,
            None => break,
        };
        let tag = &rest[at..tag_end];
        let body_end = match find_ci(&rest[tag_end..], "</script") {
            Some(e) => tag_end + e,
            None => break,
        };
        let lower = tag.to_ascii_lowercase();
        if lower.contains("application/ld+json")
            || lower.contains("__next_data__")
            || lower.contains("__nuxt_data__")
        {
            out.push(rest[tag_end..body_end].trim());
        }
        rest = &rest[body_end..];
    }
    out
}

fn find_ci(hay: &str, needle: &str) -> Option<usize> {
    hay.as_bytes()
        .windows(needle.len())
        .position(|w| w.eq_ignore_ascii_case(needle.as_bytes()))
}

/// 从数据岛里取出正文块；没有可用的长文本时返回空。
pub fn extract(html: &str, delta: Delta) -> Vec<Block> {
    let values: Vec<Value> = islands(html)
        .into_iter()
        .filter_map(|s| serde_json::from_str(s).ok())
        .collect();
    let mut leaves = vec![];
    for v in &values {
        collect(v, None, &mut leaves);
    }
    if leaves.is_empty() {
        return vec![];
    }
    let mut lens: Vec<usize> = leaves.iter().map(|l| l.text.chars().count()).collect();
    lens.sort_unstable();
    let k = ((1.0 - delta.get()) * lens.len() as f64).ceil() as usize;
    let cut = lens[k.clamp(1, lens.len()) - 1];
    let mut out = vec![];
    let mut seen = std::collections::HashSet::new();
    for leaf in leaves {
        let n = leaf.text.chars().count();
        if n <= cut || !leaf.text.contains(char::is_whitespace) || !seen.insert(leaf.text) {
            continue;
        }
        if let Some(h) = leaf.heading
            && seen.insert(h)
        {
            out.push(Block::new(Kind::Heading, format!("# {h}")));
        }
        out.extend(text_blocks(leaf.text));
    }
    out
}

struct Leaf<'a> {
    text: &'a str,
    /// 同一对象里的标题字段（`headline`、`title`、`name`），作为这段正文的标题。
    heading: Option<&'a str>,
}

fn collect<'a>(v: &'a Value, heading: Option<&'a str>, out: &mut Vec<Leaf<'a>>) {
    match v {
        Value::String(s) => out.push(Leaf { text: s, heading }),
        Value::Array(a) => a.iter().for_each(|x| collect(x, heading, out)),
        Value::Object(o) => {
            let h = ["headline", "title", "name"]
                .iter()
                .find_map(|k| o.get(*k).and_then(Value::as_str))
                .or(heading);
            o.values().for_each(|x| collect(x, h, out));
        }
        _ => {}
    }
}

fn text_blocks(s: &str) -> Vec<Block> {
    if s.contains('<') && s.contains('>') {
        let doc = pagesift_content::process(
            s,
            &pagesift_content::Options {
                only_main_content: false,
                ..Default::default()
            },
        );
        return split(&doc.markdown);
    }
    s.split("\n\n")
        .flat_map(|p| p.split('\n'))
        .map(str::trim)
        .filter(|p| !p.is_empty())
        .map(|p| Block::new(Kind::Paragraph, p.to_string()))
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn finds_long_prose_in_next_data_and_ld_json() {
        let body =
            "Rust is a language that empowers everyone to build reliable and efficient software. "
                .repeat(4);
        let short: String = (0..30).map(|i| format!("\"k{i}\": \"id-{i}\",")).collect();
        let html = format!(
            r#"<html><head><script type="application/ld+json">{{"@type":"Article","headline":"Why Rust","articleBody":"{body}"}}</script></head>
            <body><div id="root"></div><script id="__NEXT_DATA__" type="application/json">{{{short} "props":{{"title":"Why Rust","html":"<p>{body}</p>"}}}}</script></body></html>"#
        );
        assert_eq!(islands(&html).len(), 2);
        let blocks = extract(&html, Delta::default());
        assert_eq!(blocks[0].text, "# Why Rust");
        assert!(
            blocks
                .iter()
                .filter(|b| b.text.contains("empowers everyone"))
                .count()
                >= 1
        );
        assert!(!blocks.iter().any(|b| b.text.starts_with("id-")));
        assert!(extract("<html><body><p>plain</p></body></html>", Delta::default()).is_empty());
    }
}
