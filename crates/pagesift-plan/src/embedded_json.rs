//! `embedded-json` 执行器：前端框架把正文放在页面里的 JSON 中（Next.js 的 `__NEXT_DATA__`、
//! Nuxt 的 `__NUXT_DATA__`、schema.org 的 `application/ld+json`），HTTP 拿到的 HTML 骨架里没有正文时，
//! 不用浏览器也能取出来。它只产出候选，用不用由质量后验决定。
//!
//! 哪些字符串是"正文"：同一页所有字符串叶子的长度分布里，长度超过 `q(1−δ)` 的离群值，
//! 并且像自然语言（含空白分隔的词）。不设固定的字数门槛。字符串本身是 HTML 时按 HTML 提取。
//!
//! 叶子少于 `⌈1/δ⌉` 个时经验分位点 `q(1−δ)` 就是最大值本身（`⌈(1−δ)n⌉ = n`），按"大于它"
//! 取不到任何叶子：0.90 对只有 `@type`、`headline`、`articleBody` 几个字段的 JSON-LD 因此总是空的。
//! 这时改用长度分布的最大断点：排好序的长度里相邻两项比值（取对数后的差）最大的位置，
//! 断点之上的叶子是离群的长文本。所有长度相同（没有断点）时不做长度筛选，只看像不像自然语言。
//! 用不用候选仍由质量分决定。

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
    let cut = cutoff(&lens, delta);
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

/// 正文叶子的长度下限（不含）：`q(1−δ)`；样本少到分位点等于最大值时取最大断点（见模块说明）。
/// `lens` 已排序、非空。
fn cutoff(lens: &[usize], delta: Delta) -> usize {
    let n = lens.len();
    let k = ((1.0 - delta.get()) * n as f64).ceil() as usize;
    if k < n {
        return lens[k.max(1) - 1];
    }
    // 相邻长度的对数比最大处；加一避免长度 0。只有一个叶子时没有间隔，下限为 0
    let gap = |i: usize| ((lens[i + 1] + 1) as f64).ln() - ((lens[i] + 1) as f64).ln();
    match (0..n - 1).max_by(|&a, &b| gap(a).total_cmp(&gap(b))) {
        Some(i) if gap(i) > 0.0 => lens[i],
        _ => 0,
    }
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

    const BODY: &str = "The council approved the new transit plan on Tuesday after a long debate. \
                        Supporters said the plan would cut commute times across the city.";

    fn ld(fields: &str) -> String {
        format!(
            r#"<html><head><script type="application/ld+json">{{{fields}}}</script></head><body></body></html>"#
        )
    }

    fn texts(html: &str) -> Vec<String> {
        extract(html, Delta::default())
            .into_iter()
            .map(|b| b.text)
            .collect()
    }

    /// 0.90 在字符串叶子少于 20 个时总是返回空（分位点等于最大值）。n = 1…20 都要取出正文、
    /// 不带进短字段。
    #[test]
    fn small_islands_yield_the_article_body() {
        for n in [1usize, 2, 3, 5, 10, 19, 20] {
            let mut fields = vec![format!(r#""articleBody":"{BODY}""#)];
            if n >= 2 {
                fields.push(r#""headline":"Transit plan""#.into());
            }
            for i in 2..n {
                fields.push(format!(r#""k{i}":"id-{i}""#));
            }
            let t = texts(&ld(&fields.join(",")));
            assert!(t.iter().any(|x| x == BODY.trim()), "n={n}: {t:?}");
            assert!(!t.iter().any(|x| x.starts_with("id-")), "n={n}: {t:?}");
            if n >= 2 {
                assert_eq!(t[0], "# Transit plan", "n={n}");
            }
        }
    }

    #[test]
    fn typical_news_article_json_ld() {
        let html = ld(&format!(
            r#""@context":"https://schema.org","@type":"NewsArticle","headline":"Transit plan",
               "datePublished":"2026-11-03","author":{{"@type":"Person","name":"A. Writer"}},
               "articleBody":"{BODY}""#
        ));
        let t = texts(&html);
        assert_eq!(t, ["# Transit plan", BODY.trim()]);
    }

    #[test]
    fn edge_cases_of_the_small_sample_rule() {
        // 没有像自然语言的字段：什么也不出
        assert!(texts(&ld(r#""@type":"Thing","sku":"A-1","gtin":"0123""#)).is_empty());
        // 长度全相同（没有断点）：只按是否像自然语言取
        let t = texts(&ld(r#""a":"one two","b":"thr fou""#));
        assert_eq!(t, ["one two", "thr fou"]);
        // 很长的正文、多个数据岛：短字段不进来
        let long = BODY.repeat(40);
        let html = format!(
            r#"<script type="application/ld+json">{{"@type":"Article","articleBody":"{long}"}}</script>
               <script type="application/ld+json">{{"@type":"WebPage","name":"Home"}}</script>"#
        );
        let t = texts(&html);
        assert!(t.iter().any(|x| x.len() > BODY.len() * 30), "{t:?}");
        assert!(
            !t.iter().any(|x| x == "Home" || x.contains("Article")),
            "{t:?}"
        );
        // 字符串本身是 HTML：按 HTML 提取
        let html = format!(
            r#"<script id="__NEXT_DATA__">{{"props":{{"html":"<p>{BODY}</p>","id":"x"}}}}</script>"#
        );
        let t = texts(&html);
        assert_eq!(t.len(), 1, "{t:?}");
        assert!(
            t[0].contains("council approved") && !t[0].contains("<p>"),
            "{t:?}"
        );
    }

    #[test]
    fn cutoff_rule() {
        let d = Delta::default();
        assert_eq!(cutoff(&[7], d), 0);
        assert_eq!(cutoff(&[11, 12, 300], d), 12);
        assert_eq!(cutoff(&[5, 5, 5], d), 0);
        // 20 个及以上照旧取 q(1−δ)
        let lens: Vec<usize> = (1..=20).collect();
        assert_eq!(cutoff(&lens, d), 19);
    }
}
