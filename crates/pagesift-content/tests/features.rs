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
fn lazy_images_fall_through_unusable_sources() {
    let html = r#"<article><h1>Photos</h1>
        <img src="data:image/gif;base64,AAAA" data-src="/one.jpg">
        <img src="javascript:void(0)" data-lazy-src="/two.jpg">
        <img src="  " data-original="/three.jpg">
        <img data-srcset="/small.jpg 400w, /four.jpg 1200w">
        <img src="data:image/gif;base64,AAAA" data-lazy-srcset="/five.jpg 2x">
        </article>"#;
    let doc = process(html, &all());
    let images = doc.images.unwrap();
    let urls: Vec<_> = images.iter().map(|i| i.src.as_str()).collect();
    assert_eq!(
        urls,
        [
            "https://www.example.com/one.jpg",
            "https://www.example.com/two.jpg",
            "https://www.example.com/three.jpg",
            "https://www.example.com/four.jpg",
            "https://www.example.com/five.jpg",
        ]
    );
    assert!(doc.markdown.contains("/one.jpg"));
    assert!(doc.markdown.contains("/two.jpg"));
    assert!(doc.markdown.contains("/three.jpg"));
    assert!(!doc.markdown.contains("javascript:"));
}

#[test]
fn unsupported_image_schemes_do_not_become_markdown_destinations() {
    let html = r##"<article><h1>Photos</h1>
        <img src="JaVaScRiPt:alert(1)"><img src="blob:https://example.com/id">
        <img src="data:image/png;base64,AAAA"><img src="#section">
        <img src="HTTPS://cdn.example.com/valid.jpg">
        </article>"##;
    let doc = process(html, &all());
    let images = doc.images.unwrap();
    assert_eq!(images.len(), 1);
    assert_eq!(images[0].src, "https://cdn.example.com/valid.jpg");
    assert!(!doc.markdown.contains("JaVaScRiPt:"));
    assert!(!doc.markdown.contains("blob:"));
    assert!(!doc.markdown.contains("data:"));
    assert!(doc.markdown.contains("valid.jpg"));
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
fn author_prefers_json_ld_and_dates_keep_the_page_offset() {
    let m = process(PAGE, &all()).metadata;
    assert_eq!(m.author.as_deref(), Some("Ld Author"));
    // 发布日期按 htmldate 的顺序先看 meta，保留页面上写的时区
    assert_eq!(
        m.published_time.as_deref(),
        Some("2024-06-24T10:00:00+02:00")
    );
    assert_eq!(m.modified_time, None);
}

#[test]
fn dates_fall_back_to_meta_then_time() {
    let meta = r#"<html><head><meta name="author" content="M"><meta property="article:published_time" content="Tue, 01 Oct 2024 08:00:00 +0100"><meta property="article:modified_time" content="not a date"></head><body><p>x</p><time datetime="2024-10-05T00:00:00Z" itemprop="dateModified">Oct 5</time></body></html>"#;
    let m = process(meta, &Options::default()).metadata;
    assert_eq!(m.author.as_deref(), Some("M"));
    assert_eq!(
        m.published_time.as_deref(),
        Some("2024-10-01T08:00:00+01:00")
    );
    assert_eq!(m.modified_time.as_deref(), Some("2024-10-05T00:00:00Z"));

    let time = r#"<html><body><p>Posted <time datetime="2023-03-04T05:06:07+09:00">March 4</time></p></body></html>"#;
    let m = process(time, &Options::default()).metadata;
    assert_eq!(
        m.published_time.as_deref(),
        Some("2023-03-04T05:06:07+09:00")
    );
}

#[test]
fn token_estimate_is_in_range() {
    let doc = process(PAGE, &all());
    let n = estimate_tokens(&doc.markdown);
    assert!((40..200).contains(&n), "{n}");
}

#[test]
fn discovery_links_include_get_documents_but_not_resources_or_writes() {
    let html = r##"<link rel="NEXT" href="/page/2"><link rel="preload" href="/asset">
      <iframe src="/frame#part"></iframe><iframe src="/ignored" srcdoc="inline"></iframe>
      <div hx-get="/fragment">Read</div>
      <div data-hx-get="/fragment#dup"></div><button hx-post="/delete">Delete</button>
      <div hx-get="javascript:bad()"></div><div hx-get="#here"></div>
      <div hx-get=""></div><script>const route='/not-a-link';</script>"##;
    let urls: Vec<String> = process(html, &all())
        .links
        .unwrap()
        .into_iter()
        .map(|l| l.url)
        .collect();
    assert_eq!(
        urls,
        [
            "https://www.example.com/page/2",
            "https://www.example.com/frame",
            "https://www.example.com/fragment"
        ]
    );
}

#[test]
fn editorial_embeds_survive_but_social_controls_do_not() {
    for class in ["social-media-embed", "article-embed", "video-embed"] {
        let prose = "This is an editorial paragraph with sufficient context for a full article. "
            .repeat(20);
        let html = format!(
            "<article><div class='article-body'><p>{prose}</p><div class='{class}'><blockquote><p>Quoted witness account with a factual statement.</p></blockquote><script>load()</script></div><div class='social-share'><a>Share this page</a></div><div class='related'><blockquote>Unrelated recommendation</blockquote><a href='/other'>Other article</a></div></div></article>"
        );
        let out = process(&html, &Options::default()).markdown;
        assert!(out.contains("Quoted witness account"), "{class}: {out}");
        assert!(!out.contains("Share this page"));
        assert!(!out.contains("Unrelated recommendation"));
    }
}

#[test]
fn undated_article_claim_requires_one_substantial_editorial_region() {
    let prose = "A substantial independently written paragraph explains the evidence and gives readers useful context. ".repeat(6);
    for body in ["<article>", "<div class='article_body'>"] {
        let end = if body.starts_with("<article") {
            "</article>"
        } else {
            "</div>"
        };
        let html = format!(
            "<html><head><meta property='og:type' content='article'></head><body>{body}<p>{prose}</p><div class='cms_subheadline'>The second question</div><p>{prose}</p><p>{prose}</p>{end}<div class='related'><a href='/other'>A recommendation</a></div></body></html>"
        );
        let out = process(&html, &Options::default());
        assert!(out.metadata.is_article);
        assert!(out.markdown.contains("The second question"));
        assert!(!out.markdown.contains("A recommendation"));
    }
    let cards = format!(
        "<meta property='og:type' content='article'><article><p>{prose}</p><p>{prose}</p><p>{prose}</p></article><article><p>{prose}</p><p>{prose}</p><p>{prose}</p></article>"
    );
    assert!(!process(&cards, &Options::default()).metadata.is_article);
    assert!(
        !process(
            "<meta property='og:type' content='article'><article><p>Short card</p></article>",
            &Options::default()
        )
        .metadata
        .is_article
    );
}

#[test]
fn executable_inline_script_navigation_is_separate_from_arbitrary_strings() {
    let html = "<script type='application/ld+json'>{\"ignored\":\"fetch('/data-only')\"}</script><script src='/bundle.js'></script><script>location.href='/destination';fetch('/read');fetch('/write',{method:'POST'});const route='/unproven';</script>";
    let doc = pagesift_content::process(
        html,
        &pagesift_content::Options {
            links: true,
            base_url: Some("https://example.test/source".parse().unwrap()),
            ..Default::default()
        },
    );
    let urls: Vec<_> = doc.links.unwrap().into_iter().map(|l| l.url).collect();
    assert_eq!(
        urls,
        [
            "https://example.test/destination",
            "https://example.test/read"
        ]
    );
}
