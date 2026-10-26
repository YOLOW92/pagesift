//! 0.73.0：审查与奇葩测试发现的启发式误伤。每条修复都配“普通页面不受影响”的反例。

use pagesift_content::{Options, process};
use url::Url;

fn page(body: &str) -> String {
    format!("<html><head><title>Fixture</title></head><body>{body}</body></html>")
}

fn opts() -> Options {
    Options {
        base_url: Some(Url::parse("https://example.com/post/1").unwrap()),
        ..Options::default()
    }
}

fn prose(n: usize) -> String {
    "This is a perfectly normal paragraph of article text that should survive extraction intact. "
        .repeat(n)
}

#[test]
fn age_gate_strings_in_raw_html_are_not_a_barrier() {
    // Substack 每页都有这个功能开关；`language-gate` 也含 `age-gate`
    let html = format!(
        r#"<html><head><title>Choosing a web framework</title>
           <script>window._preloads = {{"test_age_gate_user":false,"theme":"x"}}</script></head>
           <body><div class="language-gate"></div><article><h1>Choosing a web framework</h1><p>{}</p></article></body></html>"#,
        prose(3)
    );
    let doc = process(&html, &opts());
    assert!(doc.barrier.is_none(), "{:?}", doc.barrier);
    assert!(doc.markdown.contains("Choosing a web framework"));
}

#[test]
fn visible_age_confirmation_is_still_a_barrier() {
    let html = page(
        r#"<div class="agegate"><h1>Welcome</h1><p>Please enter your birth date to continue.</p>
           <select name="year"></select><button>Enter</button></div>"#,
    );
    let doc = process(&html, &opts());
    assert!(doc.barrier.is_some());
}

#[test]
fn author_list_is_kept_but_author_widgets_are_not() {
    let html = page(&format!(
        r#"<main><h1>Attention Is All You Need</h1>
           <div class="authors"><a href="/a/1">Ashish Vaswani</a>, <a href="/a/2">Noam Shazeer</a></div>
           <p>{}</p></main>
           <aside class="authors-widget"><h3>Our writers</h3><ul><li><a href="/w/1">Columnist One</a></li></ul></aside>
           <footer><div class="authors"><a href="/w/2">Footer Writer</a></div></footer>"#,
        prose(4)
    ));
    let md = process(&html, &opts()).markdown;
    assert!(md.contains("Ashish Vaswani"), "{md}");
    assert!(!md.contains("Columnist One"), "{md}");
    assert!(!md.contains("Footer Writer"), "{md}");
}

#[test]
fn author_bio_box_is_pruned_in_article_mode() {
    let html = format!(
        r#"<html><head><title>Story</title><meta property="og:type" content="article">
           <meta property="article:published_time" content="2026-10-01T00:00:00Z"></head>
           <body><article><h1>Story</h1><p>{}</p>
           <div class="author-box"><p>About the writer: she has covered cities for twenty years and likes long walks.</p></div>
           </article></body></html>"#,
        prose(6)
    );
    let md = process(&html, &opts()).markdown;
    assert!(md.contains("perfectly normal paragraph"), "{md}");
    assert!(!md.contains("About the writer"), "{md}");
}

#[test]
fn cloaked_app_root_with_server_rendered_page_is_kept() {
    let html = page(&format!(
        r#"<div id="app" v-cloak><div class="page"><h1>Laravel page</h1><p>{}</p></div></div>"#,
        prose(14)
    ));
    let md = process(&html, &opts()).markdown;
    assert!(md.contains("Laravel page"), "{md}");
    assert!(md.contains("perfectly normal paragraph"), "{md}");
}

#[test]
fn cloaked_templates_and_modals_stay_hidden() {
    let html = page(&format!(
        r#"<main><h1>Real</h1><p>{}</p></main>
           <div x-cloak class="modal"><p>Subscribe to our newsletter for weekly updates!</p></div>
           <div x-data><span x-cloak>{{{{ error }}}}</span></div>"#,
        prose(4)
    ));
    let md = process(&html, &opts()).markdown;
    assert!(md.contains("Real"));
    assert!(!md.contains("Subscribe to our newsletter"), "{md}");
    assert!(!md.contains("{{ error }}"), "{md}");
}

#[test]
fn excerpts_are_not_negative_classes() {
    let html = page(&format!(
        r#"<main><h1>Blog</h1>
           <div class="entry-excerpt"><p>Excerpt: a short standfirst that introduces the story.</p></div>
           <p>{}</p></main>"#,
        prose(4)
    ));
    let md = process(&html, &opts()).markdown;
    assert!(md.contains("Excerpt: a short standfirst"), "{md}");
}

#[test]
fn template_content_never_becomes_the_body() {
    let html = page(&format!(
        r#"<template><article><h1>Template ghost</h1><p>{}</p></article></template>
           <noscript><p>Enable JS</p></noscript><div id="app"></div>"#,
        prose(20)
    ));
    let md = process(&html, &opts()).markdown;
    assert!(!md.contains("Template ghost"), "{md}");
    let with_body = page(&format!(
        r#"<template><article><h1>Template ghost</h1><p>{}</p></article></template>
           <main><h1>Real</h1><p>{}</p></main>"#,
        prose(20),
        prose(3)
    ));
    let md = process(&with_body, &opts()).markdown;
    assert!(md.contains("Real"), "{md}");
    assert!(!md.contains("Template ghost"), "{md}");
}

#[test]
fn short_body_is_not_replaced_by_video_metadata() {
    let html = format!(
        r#"<html><head><title>Note</title><script type="application/ld+json">
           {{"@type":"VideoObject","name":"Sidebar video","description":"Unrelated clip"}}</script></head>
           <body><main><p>A short but real note of about one hundred characters, written by a human.</p></main>
           <aside><iframe src="https://www.youtube.com/embed/x"></iframe></aside></body></html>"#
    );
    let doc = process(&html, &opts());
    assert!(
        doc.markdown.contains("short but real note"),
        "{}",
        doc.markdown
    );
    assert!(!doc.markdown.contains("Sidebar video"), "{}", doc.markdown);
    assert!(!doc.metadata.video_fallback);
}

#[test]
fn empty_player_shell_still_uses_video_metadata() {
    let html = r#"<html><head><title>Video</title><script type="application/ld+json">
        {"@type":"VideoObject","name":"Real video","description":"What the video is about."}</script></head>
        <body><div id="player"></div></body></html>"#;
    let doc = process(html, &opts());
    assert!(doc.markdown.contains("Real video"), "{}", doc.markdown);
    assert!(doc.metadata.video_fallback);
}

#[test]
fn nested_emphasis_is_not_repeated() {
    let html = page(&format!(
        "<main><p>{}<b><strong><b>bold</b></strong></b> and <i><em>it</em></i> and {}deep{}</p></main>",
        prose(2),
        "<b><i>".repeat(1000),
        "</i></b>".repeat(1000)
    ));
    let md = process(&html, &opts()).markdown;
    assert!(md.contains("**bold**"), "{md}");
    assert!(!md.contains("***bold"), "{md}");
    assert!(md.contains("*it*"), "{md}");
    assert!(md.matches('*').count() < 200, "{}", md.matches('*').count());
}

#[test]
fn json_ld_wrapped_in_cdata_is_passed_through() {
    let html = format!(
        r#"<html><head><title>T</title><script type="application/ld+json">//<![CDATA[
           {{"@type":"NewsArticle","author":{{"@type":"Person","name":"Ada"}},"datePublished":"2026-10-01"}}
           //]]></script></head><body><article><p>{}</p></article></body></html>"#,
        prose(3)
    );
    let doc = process(&html, &opts());
    assert_eq!(doc.metadata.json_ld().len(), 1);
    assert_eq!(doc.metadata.author.as_deref(), Some("Ada"));
}
