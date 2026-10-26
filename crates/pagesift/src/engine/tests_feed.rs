//! feed 解析（`engine/feed.rs`）的表驱动测试：每个函数一张用例表，一次跑完全部用例，最后统一报告失败的条目。
//!
//! 标 `// BUG?` 的是"当前行为看起来不对"的用例，按当前行为写，留待以后决定要不要改生产代码。

use std::fmt::Debug;

use pagesift_content::Document;

use super::{parse, safe_link, strip_html};

/// 跑一张表：逐条比较，收集所有失败再统一 panic（打印序号、输入、期望、实际）。
fn check<I: Debug, E: Debug, A: Debug>(
    name: &str,
    cases: &[(I, E)],
    f: impl Fn(&I) -> A,
    eq: impl Fn(&A, &E) -> bool,
) {
    let mut failures = Vec::new();
    for (i, (input, want)) in cases.iter().enumerate() {
        let got = f(input);
        if !eq(&got, want) {
            failures.push(format!(
                "  #{i} 输入 = {input:?}\n      期望 = {want:?}\n      实际 = {got:?}"
            ));
        }
    }
    assert!(
        failures.is_empty(),
        "{name}：{} / {} 条用例失败\n{}",
        failures.len(),
        cases.len(),
        failures.join("\n")
    );
}

/// Markdown 原文，后面接 `--`、标题和语言，要了链接时每个链接一行。
fn render(doc: Option<Document>) -> Option<String> {
    let d = doc?;
    let mut s = d.markdown;
    s.push_str(&format!(
        "--\ntitle={:?} lang={:?}\n",
        d.metadata.title, d.metadata.language
    ));
    for l in d.links.into_iter().flatten() {
        s.push_str(&format!(
            "link {} {:?} internal={}\n",
            l.url, l.text, l.internal
        ));
    }
    Some(s)
}

#[test]
fn table_parse_feed() {
    // 原 cjk_title_across_the_sniff_window_does_not_panic：标题跨过开头 4 KB 的嗅探窗口
    let head = r#"<?xml version="1.0" encoding="utf-8"?><rss><channel><title>"#;
    let pad = "a".repeat(4095 - head.len());
    let cjk_title = format!("{pad}中文标题");
    let cjk_feed = format!(
        "{head}{cjk_title}</title><item><title>条目</title><link>https://x.test/1</link></item></channel></rss>"
    );
    let cjk_want = format!(
        "# {cjk_title}\n\n- [条目](https://x.test/1)\n--\ntitle={:?} lang=None\n",
        Some(&cjk_title)
    );
    let cjk_html = format!(
        "<html><body><p>{}{}</p></body></html>",
        "a".repeat(4083),
        "中文".repeat(50)
    );

    // ((XML, 是否要链接), 期望)
    let cases: Vec<((String, bool), Option<String>)> = vec![
        // ---- 原 parses_rss_items：RSS 2.0 ----
        (
            (
                r#"<?xml version="1.0"?><rss version="2.0"><channel>
            <title>xkcd.com</title><link>https://xkcd.com/</link>
            <description>A webcomic of romance and math humor.</description>
            <language>en</language>
            <item><title>Forms of Water</title><link>https://xkcd.com/3315/</link>
            <description>&lt;p&gt;A comic.&lt;/p&gt;</description>
            <pubDate>Fri, 23 Oct 2026 04:00:00 -0000</pubDate></item>
            <item><title>Second</title><link>https://xkcd.com/3314/</link></item>
        </channel></rss>"#
                    .into(),
                true,
            ),
            Some(
                "# xkcd.com\n\nA webcomic of romance and math humor.\n\n\
                 - [Forms of Water](https://xkcd.com/3315/) — Fri, 23 Oct 2026 04:00:00 -0000\n  A comic.\n\
                 - [Second](https://xkcd.com/3314/)\n\
                 --\ntitle=Some(\"xkcd.com\") lang=Some(\"en\")\n\
                 link https://xkcd.com/3315/ \"Forms of Water\" internal=true\n\
                 link https://xkcd.com/3314/ \"Second\" internal=true\n"
                    .into(),
            ),
        ),
        // ---- 原 parses_atom_entries：Atom ----
        (
            (
                r#"<feed xmlns="http://www.w3.org/2005/Atom">
            <title>Example Feed</title>
            <entry><title>One</title><link href="https://example.com/1"/>
            <summary>First post</summary><updated>2026-10-20T10:00:00Z</updated></entry>
        </feed>"#
                    .into(),
                false,
            ),
            Some(
                "# Example Feed\n\n- [One](https://example.com/1) — 2026-10-20T10:00:00Z\n  First post\n\
                 --\ntitle=Some(\"Example Feed\") lang=None\n"
                    .into(),
            ),
        ),
        // ---- 原 cjk_title_across_the_sniff_window_does_not_panic ----
        ((cjk_feed, false), Some(cjk_want)),
        ((cjk_html, false), None),
        // ---- 原 unsafe_links_and_scripts_are_dropped ----
        (
            (
                r#"<rss><channel><title>t</title><link>https://site.test/blog/</link>
            <item><title>js</title><link>javascript:alert(1)</link>
            <description><![CDATA[<script>alert(1)</script><p>kept [x](javascript:alert(2))</p><style>p{}</style>]]></description></item>
            <item><title>data</title><link>data:text/html,hi</link></item>
            <item><title>rel</title><link>post/2</link></item>
        </channel></rss>"#
                    .into(),
                true,
            ),
            Some(
                "# t\n\n- js\n  kept \\[x\\](javascript:alert(2))\n- data\n- [rel](https://site.test/blog/post/2)\n\
                 --\ntitle=Some(\"t\") lang=None\n\
                 link https://site.test/blog/post/2 \"rel\" internal=true\n"
                    .into(),
            ),
        ),
        // ---- 原 ignores_html_and_sitemaps ----
        (("<html><body><p>hi</p></body></html>".into(), false), None),
        (
            (
                "<urlset><url><loc>https://example.com/</loc></url></urlset>".into(),
                false,
            ),
            None,
        ),
        // ---- 补充：RDF / RSS 1.0（dc:date、dc:language） ----
        (
            (
                r#"<?xml version="1.0"?><rdf:RDF xmlns:rdf="http://www.w3.org/1999/02/22-rdf-syntax-ns#" xmlns="http://purl.org/rss/1.0/" xmlns:dc="http://purl.org/dc/elements/1.1/"><channel rdf:about="https://r.test/"><title>R</title><link>https://r.test/</link><description>D</description><dc:language>ja</dc:language></channel><item rdf:about="https://r.test/1"><title>One</title><link>https://r.test/1</link><description>S</description><dc:date>2024-03-05T08:00:00+09:00</dc:date></item></rdf:RDF>"#
                    .into(),
                false,
            ),
            Some(
                "# R\n\nD\n\n- [One](https://r.test/1) — 2024-03-05T08:00:00+09:00\n  S\n\
                 --\ntitle=Some(\"R\") lang=Some(\"ja\")\n"
                    .into(),
            ),
        ),
        // ---- 补充：Atom 的多个链接只取 alternate；HTML 摘要；published 与 updated 取后出现的 ----
        (
            (
                r#"<feed xmlns="http://www.w3.org/2005/Atom"><title>F</title><entry><title>M</title><link rel="self" href="https://a.test/self"/><link rel="alternate" type="text/html" href="https://a.test/m"/><link rel="enclosure" href="https://a.test/m.mp3"/><summary type="html">&lt;b&gt;Bold&lt;/b&gt; &amp;amp; more</summary><published>2024-01-01T00:00:00Z</published><updated>
                2024-02-01T00:00:00Z
            </updated></entry></feed>"#
                    .into(),
                false,
            ),
            Some(
                "# F\n\n- [M](https://a.test/m) — 2024-02-01T00:00:00Z\n  Bold & more\n\
                 --\ntitle=Some(\"F\") lang=None\n"
                    .into(),
            ),
        ),
        // ---- 补充：Atom 相对链接按 feed 级 <link> 补全 ----
        (
            (
                r#"<feed xmlns="http://www.w3.org/2005/Atom"><title>A</title><link href="https://a.test/blog/"></link><entry><title>Post</title><link href="posts/1"/></entry></feed>"#
                    .into(),
                false,
            ),
            Some(
                "# A\n\n- [Post](https://a.test/blog/posts/1)\n--\ntitle=Some(\"A\") lang=None\n".into(),
            ),
        ),
        // feed 级 <link .../> 是自闭合时（Atom 的常见写法）没读到，相对链接被丢掉
        (
            (
                r#"<feed xmlns="http://www.w3.org/2005/Atom"><title>A</title><link href="https://a.test/blog/"/><entry><title>Post</title><link href="posts/1"/></entry></feed>"#
                    .into(),
                false,
            ),
            Some("# A\n\n- Post\n--\ntitle=Some(\"A\") lang=None\n".into()), // BUG? 应为 [Post](https://a.test/blog/posts/1)
        ),
        // ---- 补充：BOM 和开头空白、CDATA 标题、实体、方括号、无标题、只有标题、空条目、站外链接 ----
        (
            (
                "\u{feff}  \n<rss version=\"2.0\"><channel><title><![CDATA[News & Views]]></title><link>https://n.test/</link>\
                 <description>&lt;p&gt;Hello &lt;b&gt;world&lt;/b&gt;&lt;/p&gt;</description><language>zh-CN</language>\
                 <item><title>[Breaking] A &amp; B</title><link>https://n.test/a?x=1&amp;y=2</link></item>\
                 <item><link>https://other.test/b</link></item>\
                 <item><title>Only title</title></item>\
                 <item><description>no title no link</description></item>\
                 </channel></rss>"
                    .into(),
                true,
            ),
            Some(
                "# News & Views\n\nHello world\n\n\
                 - [\\[Breaking\\] A & B](https://n.test/a?x=1&y=2)\n\
                 - [Untitled](https://other.test/b)\n\
                 - Only title\n\
                 --\ntitle=Some(\"News & Views\") lang=Some(\"zh-CN\")\n\
                 link https://n.test/a?x=1&y=2 \"[Breaking] A & B\" internal=true\n\
                 link https://other.test/b \"\" internal=false\n"
                    .into(),
            ),
        ),
        // ---- 补充：不是 feed ----
        (
            (
                r#"<rss version="2.0"><channel><title>Empty</title></channel></rss>"#.into(),
                false,
            ),
            None, // 没有条目
        ),
        ((r#"<feed><entry></entry></feed>"#.into(), false), None), // 条目既无标题也无链接
        (
            (
                r#"<html><body><rss><item>x</item></rss></body></html>"#.into(),
                false,
            ),
            None,
        ),
        (("plain text, not xml".into(), false), None),
        (("".into(), false), None),
    ];
    check(
        "feed::parse",
        &cases,
        |(xml, links)| render(parse(xml, *links)),
        |a, e| a == e,
    );
}

#[test]
fn table_safe_link() {
    const BASE: &str = "https://a.test/blog/";
    // ((原始链接, 是否有 base), 期望)
    const CASES: &[((&str, bool), Option<&str>)] = &[
        (("https://a.test/x", false), Some("https://a.test/x")),
        (("  https://a.test/x  ", false), Some("https://a.test/x")),
        (("HTTP://A.TEST/X", false), Some("http://a.test/X")),
        (("https://a.test", false), Some("https://a.test/")),
        // 相对地址按 base 补全
        (("/p", true), Some("https://a.test/p")),
        (("p", true), Some("https://a.test/blog/p")),
        (("?q=1", true), Some("https://a.test/blog/?q=1")),
        (("//cdn.test/x", true), Some("https://cdn.test/x")),
        (("p", false), None),
        // 非 http(s) 一律丢弃
        (("javascript:alert(1)", true), None),
        (("JavaScript:alert(1)", true), None),
        (("data:text/html,hi", true), None),
        (("mailto:x@y.test", true), None),
        (("ftp://a.test/f", true), None),
        (("", true), None),
        (("   ", true), None),
    ];
    let base = pagesift_net::Url::parse(BASE).unwrap();
    check(
        "safe_link",
        CASES,
        |&(raw, with_base)| safe_link(raw, with_base.then_some(&base)),
        |a, e| a.as_deref() == *e,
    );
}

#[test]
fn table_strip_html() {
    const CASES: &[(&str, &str)] = &[
        ("<p>a</p><p>b</p>", "a b"),
        ("plain", "plain"),
        ("<a href=\"x\">link</a> text", "link text"),
        ("a<br/>b", "a b"),
        ("<!-- c -->x", "x"),
        ("  multi \n line  ", "multi line"),
        // 实体只还原一层
        ("&lt;tag&gt;", "<tag>"),
        ("a &amp;lt; b", "a &lt; b"),
        ("&nbsp;a&quot;b&#39;c&apos;d", "a\"b'c'd"),
        // 脚本、样式的内容整个丢掉（大小写不敏感）
        ("<script>x()</script>y", "y"),
        ("<SCRIPT>x()</SCRIPT>y", "y"),
        ("<style>p{}</style>z", "z"),
        ("<script>never closed", ""),
        // 方括号转义，免得拼出 Markdown 链接
        ("[x](javascript:y)", "\\[x\\](javascript:y)"),
        // 没闭合的标签丢掉后面的内容
        ("unclosed <b", "unclosed"),
        // 文本里的 "<" 被当成标签开头
        ("a < b", "a"),             // BUG? 应保留 "a < b"
        ("1 < 2 and 3 > 2", "1 2"), // BUG? "< 2 and 3 >" 被当成标签吃掉
    ];
    check("strip_html", CASES, |s| strip_html(s), |a, e| a == e);
}
