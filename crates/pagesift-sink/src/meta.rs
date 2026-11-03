//! 元数据键名：内容层的 `Metadata` 映射成对外的键，只在这里写一份。
//! scrape（Firecrawl 信封）、py/node 的 `convert` 都用它，所以同一页经哪个入口拿到的键名都一样。

use pagesift_api::types::Metadata;

/// 标准字段：(Firecrawl 键名, 值)，顺序就是 node `convert` 输出对象里的顺序。
/// [推导] 数组长度 = Firecrawl `metadata` 里取自页面的标准字段数
pub fn standard(m: &pagesift_content::Metadata) -> [(&'static str, Option<&String>); 9] {
    [
        ("title", m.title.as_ref()),
        ("description", m.description.as_ref()),
        ("language", m.language.as_ref()),
        ("keywords", m.keywords.as_ref()),
        ("robots", m.robots.as_ref()),
        ("favicon", m.favicon.as_ref()),
        ("publishedTime", m.published_time.as_ref()),
        ("author", m.author.as_ref()),
        ("modifiedTime", m.modified_time.as_ref()),
    ]
}

/// 页面里其余的 meta（`og:image` 之类），键名已映射；同一个键出现多次时取第一个。
pub fn extra(m: &pagesift_content::Metadata) -> impl Iterator<Item = (String, &String)> {
    m.extra.iter().map(|(k, v)| (key(k), v))
}

/// 把内容层元数据填进 Firecrawl 的 `metadata`（来源、状态码这些由调用方填）。
pub fn fill(meta: &mut Metadata, m: &pagesift_content::Metadata) {
    meta.title = m.title.clone();
    meta.description = m.description.clone();
    meta.language = m.language.clone();
    meta.keywords = m.keywords.clone();
    meta.robots = m.robots.clone();
    meta.favicon = m.favicon.clone();
    meta.published_time = m.published_time.clone();
    meta.author = m.author.clone();
    meta.modified_time = m.modified_time.clone();
    for (k, v) in extra(m) {
        meta.extra.entry(k).or_insert_with(|| v.clone().into());
    }
}

/// `og:image` → `ogImage`、`article:published_time` → `articlePublishedTime`，和 Firecrawl 的键名风格一致。
pub fn key(raw: &str) -> String {
    let mut out = String::with_capacity(raw.len());
    let mut upper = false;
    for c in raw.chars() {
        if matches!(c, ':' | '_' | '-' | '.') {
            upper = true;
        } else if upper {
            out.extend(c.to_uppercase());
            upper = false;
        } else {
            out.push(c);
        }
    }
    out
}

/// Python 的键名：`ogImage` → `og_image`、`numPages` → `num_pages`。
pub fn snake_case(k: &str) -> String {
    let mut out = String::with_capacity(k.len() + 4);
    for c in k.chars() {
        if c.is_ascii_uppercase() {
            if !out.is_empty() {
                out.push('_');
            }
            out.push(c.to_ascii_lowercase());
        } else {
            out.push(c);
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn keys_follow_firecrawl_then_python_style() {
        assert_eq!(key("og:image"), "ogImage");
        assert_eq!(key("article:published_time"), "articlePublishedTime");
        assert_eq!(snake_case(&key("og:image")), "og_image");
        let m = pagesift_content::Metadata {
            author: Some("A".into()),
            extra: vec![
                ("og:image".into(), "1".into()),
                ("og_image".into(), "2".into()),
            ],
            ..Default::default()
        };
        let mut meta = Metadata::default();
        fill(&mut meta, &m);
        assert_eq!(meta.author.as_deref(), Some("A"));
        assert_eq!(meta.extra["ogImage"], "1");
        assert!(
            standard(&m)
                .iter()
                .any(|(k, v)| *k == "author" && v.is_some())
        );
    }
}
