//! 链接在正文 Markdown 里的锚文本和所在行，给链接打分用。

use std::collections::HashMap;
use std::sync::LazyLock;

use regex::Regex;

/// 链接上下文最多取多少字节（一行很长时只看链接附近）。
const CONTEXT_BYTES: usize = 400;

/// 一个链接在正文 Markdown 里的锚文本和所在行（链接语法换成锚文本）。
pub(crate) struct LinkContext {
    pub anchor: String,
    pub line: String,
}

static MD_LINK: LazyLock<Regex> = LazyLock::new(|| {
    Regex::new(r"\[([^\[\]]*)\]\(([^()\s]+)(?:\s+\x22[^\x22]*\x22)?\)").expect("valid regex")
});

/// 从正文 Markdown 里找出每个链接（去掉 fragment 的绝对地址）的锚文本和上下文。
/// 导航栏等不在正文里的链接没有上下文，调用方用 DOM 里的锚文本代替。同一链接取第一次出现。
pub(crate) fn link_contexts(markdown: &str) -> HashMap<String, LinkContext> {
    let mut out: HashMap<String, LinkContext> = HashMap::new();
    for line in markdown.lines() {
        if !line.contains("](") {
            continue;
        }
        let plain = MD_LINK.replace_all(line, "$1");
        for cap in MD_LINK.captures_iter(line) {
            let url = cap[2].split('#').next().unwrap_or_default();
            if url.is_empty() || out.contains_key(url) {
                continue;
            }
            let anchor = cap[1].trim().to_string();
            out.insert(
                url.to_string(),
                LinkContext {
                    line: around(&plain, &anchor),
                    anchor,
                },
            );
        }
    }
    out
}

/// 正文里不属于链接标签的词数：一页的字大多是链接标签（目录页、文章列表）时它就是枢纽页，
/// 标题里凑齐了查询词也不算内容。
pub(crate) fn prose_words(markdown: &str) -> usize {
    MD_LINK
        .replace_all(markdown, "")
        .split_whitespace()
        .filter(|w| w.chars().any(char::is_alphanumeric))
        .count()
}

/// 一行里锚文本附近的 [`CONTEXT_BYTES`] 字节；行不长时就是整行。
fn around(line: &str, anchor: &str) -> String {
    if line.len() <= CONTEXT_BYTES {
        return line.to_string();
    }
    let at = line.find(anchor).unwrap_or(0);
    let mut lo = at.saturating_sub(CONTEXT_BYTES / 2);
    let mut hi = (lo + CONTEXT_BYTES).min(line.len());
    while !line.is_char_boundary(lo) {
        lo -= 1;
    }
    while !line.is_char_boundary(hi) {
        hi += 1;
    }
    line[lo..hi].to_string()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn link_contexts_come_from_markdown_lines() {
        let md = "# T\n\nRead the [refund rules](https://e.com/r#top) before buying.\n\n- [About](https://e.com/a \"title\")\n- [Refund again](https://e.com/r)\n\n![img](https://e.com/i.png)\n";
        let c = link_contexts(md);
        let r = &c["https://e.com/r"];
        assert_eq!(r.anchor, "refund rules");
        assert_eq!(r.line, "Read the refund rules before buying.");
        assert_eq!(c["https://e.com/a"].anchor, "About");
        let long = format!(
            "x{} [k](https://e.com/k) {}",
            "a".repeat(600),
            "b".repeat(600)
        );
        let c = link_contexts(&long);
        assert!(c["https://e.com/k"].line.len() <= CONTEXT_BYTES + 1);
        assert!(c["https://e.com/k"].line.contains('k'));
    }

    #[test]
    fn prose_words_exclude_link_labels() {
        let md = "Intro line here.\n\n- [Refund policy for everyone](https://e.com/a)\n- [Second long label](https://e.com/b)\n";
        assert_eq!(prose_words(md), 3);
        assert_eq!(prose_words("plain words only"), 3);
    }
}
