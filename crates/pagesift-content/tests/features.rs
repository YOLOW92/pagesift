//! 按需输出：纯文本、清洗后的 HTML、链接、图片、指纹，以及元数据里的作者和时间。

use pagesift_content::{Options, estimate_tokens, process};
use url::Url;

const PAGE: &str = r#"<!doctype html><html><head><title>Post</title>
<meta name="author" content="Meta Author">
<meta property="article:published_time" content="2024-06-24T10:00:00+02:00">
<script type="application/ld+json">{"@context":"https://schema.org","@graph":[
 {"@type":"Person","@id":"/#p","name":"Ld Author"},
 {"@type":"Article","author":{"@id":"/#p"},"datePublished":"2024-06-23T22:30:00-04:00"}]}</script>
<script type="application/ld+json">{ broken json </script>
</head><body>
<nav><a href="/">Home</a> <a href="/about#team">About</a></nav>
<article>
<h1>Hello   world</h1>
<p onclick="evil()" style="color:red">First <b>bold</b> paragraph with a <a href="/docs#intro">doc link</a>
and an <a href="https://other.org/x">outside link</a>.</p>
<ul><li>one</li><li>two</li></ul>
<table><tr><th>Name</th><th>City</th></tr><tr><td>Ada</td><td>New York</td></tr></table>
<pre><code>fn main() {
    let x = 1;
}</code></pre>
<img src="/a.jpg" srcset="/a-400.jpg 400w, /a-1600.jpg 1600w" alt=" A cat ">
<picture><source srcset="/b-2x.webp 2x, /b-3x.webp 3x"><img src="/b.jpg"></picture>
<img src="data:image/gif;base64,R0lGOD" alt="inline">
<img src="https://t.example.net/pixel.gif" width="1" height="1">
<img src="/a.jpg" alt="duplicate">
<a href="javascript:void(0)">js</a><a href="mailto:x@example.com">mail</a>
<a href="https://blog.example.com/p">sub</a><a href="/docs">docs again</a>
<script>alert(1)</script>
</article>
<footer>Footer text</footer>
</body></html>"#;

fn all() -> Options {
    Options {
        base_url: Some(Url::parse("https://www.example.com/post").unwrap()),
        plain_text: true,
        clean_html: true,
        links: true,
        images: true,
        fingerprints: true,
        ..Default::default()
    }
}

#[test]
fn optional_outputs_are_off_by_default() {
    let doc = process(PAGE, &Options::default());
    assert!(doc.plain_text.is_none());
    assert!(doc.html.is_none());
    assert!(doc.links.is_none());
    assert!(doc.images.is_none());
    assert!(doc.fingerprints.is_none());
    assert!(doc.barrier.is_none());
}

#[test]
fn plain_text_follows_skip_rules() {
    let doc = process(PAGE, &all());
    let text = doc.plain_text.unwrap();
    assert!(
        text.starts_with("Hello world\n\nFirst bold paragraph"),
        "{text}"
    );
    assert!(text.contains("one\ntwo"), "{text}");
    assert!(text.contains("Name\tCity\nAda\tNew York"), "{text}");
    assert!(text.contains("fn main() {\n    let x = 1;\n}"), "{text}");
    for noise in ["Home", "Footer", "alert", "](", "**"] {
        assert!(!text.contains(noise), "{noise:?} in {text}");
    }
}

#[test]
fn clean_html_drops_scripts_and_handlers() {
    let doc = process(PAGE, &all());
    let html = doc.html.unwrap();
    assert!(html.contains("<h1>Hello   world</h1>"), "{html}");
    assert!(
        html.contains(r#"<a href="https://www.example.com/docs#intro">doc link</a>"#),
        "{html}"
    );
    assert!(html.contains("<p>First <b>bold</b>"), "{html}");
    for bad in [
        "onclick",
        "style=",
        "<script",
        "alert(",
        "<nav",
        "<footer",
        "javascript:",
    ] {
        assert!(!html.contains(bad), "{bad:?} in {html}");
    }
    assert!(
        html.contains("src=\"https://www.example.com/a.jpg\""),
        "{html}"
    );
    assert!(
        html.contains("srcset=\"https://www.example.com/a-400.jpg 400w, https://www.example.com/a-1600.jpg 1600w\""),
        "{html}"
    );
}

#[test]
fn links_are_absolute_deduped_and_classified() {
    let doc = process(PAGE, &all());
    let links = doc.links.unwrap();
    let urls: Vec<&str> = links.iter().map(|l| l.url.as_str()).collect();
    assert_eq!(
        urls,
        [
            "https://www.example.com/",
            "https://www.example.com/about",
            "https://www.example.com/docs",
            "https://other.org/x",
            "https://blog.example.com/p",
        ]
    );
    let internal: Vec<bool> = links.iter().map(|l| l.internal).collect();
    assert_eq!(internal, [true, true, true, false, true]);
    assert_eq!(links[2].text, "doc link");
}

#[test]
fn images_pick_largest_and_skip_trackers() {
    let doc = process(PAGE, &all());
    let images = doc.images.unwrap();
    assert_eq!(images.len(), 2, "{images:?}");
    assert_eq!(images[0].src, "https://www.example.com/a.jpg");
    assert_eq!(
        images[0].srcset_largest.as_deref(),
        Some("https://www.example.com/a-1600.jpg")
    );
    assert_eq!(images[0].alt.as_deref(), Some("A cat"));
    assert_eq!(images[1].src, "https://www.example.com/b.jpg");
    assert_eq!(
        images[1].srcset_largest.as_deref(),
        Some("https://www.example.com/b-3x.webp")
    );
    assert_eq!(images[1].alt, None);
}

#[test]
fn fingerprints_ignore_whitespace_differences() {
    let a = process(PAGE, &all()).fingerprints.unwrap();
    let spaced = PAGE.replace("First <b>", "First   \n <b>");
    let b = process(&spaced, &all()).fingerprints.unwrap();
    assert_ne!(a.raw_html, b.raw_html);
    assert_eq!(a.markdown, b.markdown);
    assert_eq!(a.text, b.text);
    assert_eq!(a.raw_html.len(), 64);
    assert!(
        a.text
            .chars()
            .all(|c| c.is_ascii_hexdigit() && !c.is_ascii_uppercase())
    );
    let changed = process(&PAGE.replace("bold", "brave"), &all())
        .fingerprints
        .unwrap();
    assert_ne!(a.text, changed.text);
}

#[test]
fn author_and_dates_prefer_json_ld_and_use_utc() {
    let m = process(PAGE, &all()).metadata;
    assert_eq!(m.author.as_deref(), Some("Ld Author"));
    assert_eq!(m.published_time.as_deref(), Some("2024-06-24T02:30:00Z"));
    assert_eq!(m.modified_time, None);
}

#[test]
fn dates_fall_back_to_meta_then_time() {
    let meta = r#"<html><head><meta name="author" content="M"><meta property="article:published_time" content="Tue, 01 Oct 2024 08:00:00 +0100"><meta property="article:modified_time" content="not a date"></head><body><p>x</p><time datetime="2024-10-05T00:00:00Z" itemprop="dateModified">Oct 5</time></body></html>"#;
    let m = process(meta, &Options::default()).metadata;
    assert_eq!(m.author.as_deref(), Some("M"));
    assert_eq!(m.published_time.as_deref(), Some("2024-10-01T07:00:00Z"));
    assert_eq!(m.modified_time.as_deref(), Some("2024-10-05T00:00:00Z"));

    let time = r#"<html><body><p>Posted <time datetime="2023-03-04T05:06:07+09:00">March 4</time></p></body></html>"#;
    let m = process(time, &Options::default()).metadata;
    assert_eq!(m.published_time.as_deref(), Some("2023-03-03T20:06:07Z"));
}

#[test]
fn token_estimate_is_in_range() {
    let doc = process(PAGE, &all());
    let n = estimate_tokens(&doc.markdown);
    assert!((40..200).contains(&n), "{n}");
}
