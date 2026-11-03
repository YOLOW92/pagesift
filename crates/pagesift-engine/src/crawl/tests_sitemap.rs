//! sitemap 解析（`crawl/sitemap.rs` 的 `parse_sitemap`）的表驱动测试：一次跑完全部用例，最后统一报告失败的条目。
//!
//! 标 `// BUG?` 的是"当前行为看起来不对"的用例，按当前行为写，留待以后决定要不要改生产代码。

use super::sitemap::parse_sitemap;

type Want = (&'static [&'static str], &'static [&'static str]);

#[test]
fn table_parse_sitemap() {
    // (输入, (页面, 子 sitemap))
    const CASES: &[(&[u8], Want)] = &[
        // ---- 原 parses_urlsets_and_indexes ----
        (
            br#"<?xml version="1.0"?><urlset xmlns="http://www.sitemaps.org/schemas/sitemap/0.9">
            <url><loc>https://example.com/a</loc><lastmod>2026-01-01</lastmod></url>
            <url><loc><![CDATA[https://example.com/b?x=1&y=2]]></loc></url>
            <url><loc>https://example.com/c?p=1&amp;q=&#50;</loc></url></urlset>"#,
            (
                &[
                    "https://example.com/a",
                    "https://example.com/b?x=1&y=2",
                    "https://example.com/c?p=1&q=2",
                ],
                &[],
            ),
        ),
        (
            br#"<sitemapindex><sitemap><loc>https://example.com/s1.xml.gz</loc></sitemap></sitemapindex>"#,
            (&[], &["https://example.com/s1.xml.gz"]),
        ),
        (b"not xml <<<", (&[], &[])),
        // ---- 原 parses_feeds_and_text_sitemaps ----
        // RSS 2.0：<item> 的 <link> 文本（channel 的 link 不算）
        (
            br#"<?xml version="1.0"?><rss version="2.0"><channel><link>https://example.com/</link>
            <item><title>A</title><link>https://example.com/a?x=1&amp;y=2</link></item>
            <item><link><![CDATA[https://example.com/b]]></link></item></channel></rss>"#,
            (&["https://example.com/a?x=1&y=2", "https://example.com/b"], &[]),
        ),
        // Atom：<entry> 的 <link href>，只认 rel="alternate" 或没有 rel
        (
            br#"<feed xmlns="http://www.w3.org/2005/Atom"><link href="https://example.com/"/>
            <entry><link rel="alternate" href="https://example.com/c"/><link rel="edit" href="https://example.com/edit/c"/></entry>
            <entry><link href="https://example.com/d"></link></entry></feed>"#,
            (&["https://example.com/c", "https://example.com/d"], &[]),
        ),
        // 纯文本：每行一个 URL，BOM、CRLF、首尾空白都能处理，别的行忽略
        (
            b"\xef\xbb\xbfhttps://example.com/e\r\n\n  http://example.com/f  \nnot a url\n",
            (&["https://example.com/e", "http://example.com/f"], &[]),
        ),
        // ---- 补充：XML sitemap ----
        // 命名空间前缀
        (
            br#"<sm:urlset xmlns:sm="http://www.sitemaps.org/schemas/sitemap/0.9"><sm:url><sm:loc>https://example.com/p</sm:loc></sm:url></sm:urlset>"#,
            (&["https://example.com/p"], &[]),
        ),
        // 多个子 sitemap、lastmod、<loc> 里的首尾空白
        (
            br#"<sitemapindex><sitemap><loc>
                https://example.com/s1.xml
            </loc><lastmod>2024-01-01</lastmod></sitemap><sitemap><loc>https://example.com/s2.xml</loc></sitemap></sitemapindex>"#,
            (&[], &["https://example.com/s1.xml", "https://example.com/s2.xml"]),
        ),
        // 空的 <loc> 跳过
        (
            br#"<urlset><url><loc>  </loc></url><url><loc></loc></url><url><loc>https://example.com/x</loc></url></urlset>"#,
            (&["https://example.com/x"], &[]),
        ),
        // 十六进制字符引用还原，未知实体丢掉
        (
            br#"<urlset><url><loc>https://example.com/?a=1&#x26;b=2&nbsp;</loc></url></urlset>"#,
            (&["https://example.com/?a=1&b=2"], &[]),
        ),
        // 扩展：hreflang 备用链接、视频、新闻都不当页面
        (
            br#"<urlset xmlns:xhtml="http://www.w3.org/1999/xhtml" xmlns:video="http://www.google.com/schemas/sitemap-video/1.1" xmlns:news="http://www.google.com/schemas/sitemap-news/0.9">
            <url><loc>https://example.com/en</loc><xhtml:link rel="alternate" hreflang="de" href="https://example.com/de"/></url>
            <url><loc>https://example.com/v</loc><video:video><video:content_loc>https://example.com/v.mp4</video:content_loc><video:player_loc>https://example.com/player</video:player_loc></video:video></url>
            <url><loc>https://example.com/n</loc><news:news><news:title>T</news:title></news:news></url></urlset>"#,
            (
                &[
                    "https://example.com/en",
                    "https://example.com/v",
                    "https://example.com/n",
                ],
                &[],
            ),
        ),
        // 图片扩展的 <image:loc> 本地名也是 loc，但不是页面（0.80 前被当成页面）
        (
            br#"<urlset xmlns:image="http://www.google.com/schemas/sitemap-image/1.1"><url><loc>https://example.com/page</loc><image:image><image:loc>https://example.com/img.jpg</image:loc></image:image></url></urlset>"#,
            (&["https://example.com/page"], &[]),
        ),
        // 图片扩展写在 <loc> 前面、sitemap 命名空间带前缀
        (
            br#"<sm:urlset xmlns:sm="http://www.sitemaps.org/schemas/sitemap/0.9" xmlns:image="http://www.google.com/schemas/sitemap-image/1.1"><sm:url><image:image><image:loc>https://example.com/a.png</image:loc></image:image><sm:loc>https://example.com/p</sm:loc></sm:url></sm:urlset>"#,
            (&["https://example.com/p"], &[]),
        ),
        // 不在 <url> 里的 <loc> 也收（宽松）
        (
            br#"<urlset><loc>https://example.com/bare</loc></urlset>"#,
            (&["https://example.com/bare"], &[]),
        ),
        // 带 BOM 的 XML、XML 前面有空白
        (
            b"\xef\xbb\xbf<urlset><url><loc>https://example.com/bom</loc></url></urlset>",
            (&["https://example.com/bom"], &[]),
        ),
        (
            b"\n  <?xml version=\"1.0\"?>\n<urlset><url><loc>https://example.com/ws</loc></url></urlset>",
            (&["https://example.com/ws"], &[]),
        ),
        // 结束标签写错：照样读出（容错）
        (
            br#"<urlset><url><loc>https://example.com/1</loc></url><url><loc>https://example.com/2</loc></bad></urlset>"#,
            (&["https://example.com/1", "https://example.com/2"], &[]),
        ),
        // 文件被截断：保留截断前完整的条目
        (
            br#"<urlset><url><loc>https://example.com/1</loc></url><url><loc>https://exam"#,
            (&["https://example.com/1"], &[]),
        ),
        // ---- 补充：feed ----
        // RDF / RSS 1.0
        (
            br#"<rdf:RDF xmlns:rdf="http://www.w3.org/1999/02/22-rdf-syntax-ns#" xmlns="http://purl.org/rss/1.0/"><channel><link>https://example.com/</link></channel><item rdf:about="https://example.com/r1"><title>R</title><link>https://example.com/r1</link></item></rdf:RDF>"#,
            (&["https://example.com/r1"], &[]),
        ),
        // Atom 只有 rel="self" 的链接：没有页面
        (
            br#"<feed><entry><link rel="self" href="https://example.com/self"/></entry></feed>"#,
            (&[], &[]),
        ),
        // Atom href 里的 &amp;
        (
            br#"<feed><entry><link href="https://example.com/q?a=1&amp;b=2"/></entry></feed>"#,
            (&["https://example.com/q?a=1&b=2"], &[]),
        ),
        // RSS 只有 channel 的 link，没有 item
        (
            br#"<rss><channel><link>https://example.com/</link></channel></rss>"#,
            (&[], &[]),
        ),
        // ---- 补充：纯文本 ----
        (
            b"ftp://example.com/x\n# comment\nhttps://example.com/1\n\twww.example.com/2\n",
            (&["https://example.com/1"], &[]),
        ),
        (b"", (&[], &[])),
    ];
    let mut failures = Vec::new();
    for (i, (input, (pages, children))) in CASES.iter().enumerate() {
        let got = parse_sitemap(input);
        if got.0 != *pages || got.1 != *children {
            failures.push(format!(
                "  #{i} 输入 = {:?}\n      期望 = ({pages:?}, {children:?})\n      实际 = {got:?}",
                String::from_utf8_lossy(input)
            ));
        }
    }
    assert!(
        failures.is_empty(),
        "parse_sitemap：{} / {} 条用例失败\n{}",
        failures.len(),
        CASES.len(),
        failures.join("\n")
    );
}
