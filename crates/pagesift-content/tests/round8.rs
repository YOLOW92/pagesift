//! 0.72.8 field-audit fixes: extraction and signal regressions from rounds 5–8.

use pagesift_content::{BarrierKind, Options, process};
use url::Url;

fn page(body: &str) -> String {
    format!("<html><head><title>Fixture</title></head><body>{body}</body></html>")
}

#[test]
fn image_title_is_kept_and_escaped() {
    let html = page(
        r#"<article><p>Some text before the image, long enough to keep.</p><img src="/a.png" alt="A [b]" title="hover &quot;quote&quot;"></article>"#,
    );
    let md = process(&html, &Options::default()).markdown;
    assert!(md.contains(r#"![A b](/a.png "hover \"quote\"")"#), "{md}");
}

#[test]
fn github_code_view_drops_line_numbers_and_merges_code() {
    let html = page(
        r#"<main><div class="react-line-numbers">
             <div data-line-number="1" class="react-line-number">1</div>
             <div data-line-number="2" class="react-line-number">2</div>
           </div>
           <div class="react-code-lines">
             <div class="react-code-text"><div><div class="react-file-line html-div" data-testid="code-cell">fn main() {</div></div></div>
             <div class="react-code-text"><div><div class="react-file-line html-div" data-testid="code-cell">    println!("hi");</div></div></div>
             <div class="react-code-text"><div><div class="react-file-line html-div" data-testid="code-cell">}</div></div></div>
           </div></main>"#,
    );
    let md = process(&html, &Options::default()).markdown;
    assert!(
        md.contains("```\nfn main() {\n    println!(\"hi\");\n}\n```"),
        "{md}"
    );
    assert!(
        !md.lines().any(|l| l.trim() == "1"),
        "line numbers leaked: {md}"
    );
}

#[test]
fn content_aside_is_kept_while_sidebar_is_dropped() {
    let body = "The body text is long enough to be kept by the extraction rules. ".repeat(12);
    let html = page(&format!(
        r#"<main><article><h1>Recipe</h1><p>{body}</p>
           <aside class="recipe__ingredients"><h2>Ingredients</h2><ul><li>2 cups flour</li><li>1 teaspoon salt</li></ul></aside>
           <aside class="sidebar"><h2>Related</h2><a href="/x">Other posts</a></aside></article></main>"#
    ));
    let opts = Options {
        base_url: Some(Url::parse("http://example.com/").unwrap()),
        ..Options::default()
    };
    let md = process(&html, &opts).markdown;
    assert!(md.contains("2 cups flour"), "content aside dropped: {md}");
    assert!(!md.contains("Other posts"), "sidebar kept: {md}");
}

#[test]
fn author_list_survives_link_density_rules() {
    let body = "The dominant sequence transduction models are based on complex recurrent or convolutional neural networks. ".repeat(6);
    let html = page(&format!(
        r#"<main><div id="content"><h1>Attention Is All You Need</h1>
           <div class="authors">Authors:
             <a href="/a">Ashish Vaswani</a>, <a href="/b">Noam Shazeer</a>,
             <a href="/c">Niki Parmar</a>, <a href="/d">Jakob Uszkoreit</a>
           </div>
           <blockquote><p>{body}</p></blockquote></div></main>"#
    ));
    let md = process(&html, &Options::default()).markdown;
    assert!(md.contains("Ashish Vaswani"), "authors dropped: {md}");
}

#[test]
fn citation_author_fills_metadata() {
    let html = r#"<html><head>
        <meta name="citation_title" content="Attention Is All You Need">
        <meta name="citation_author" content="Vaswani, Ashish">
        <meta name="citation_author" content="Shazeer, Noam">
        <meta name="citation_doi" content="10.48550/arXiv.1706.03762">
        </head><body><article><h1>Attention Is All You Need</h1><p>Body text.</p></article></body></html>"#;
    let doc = process(html, &Options::default());
    assert_eq!(
        doc.metadata.author.as_deref(),
        Some("Vaswani, Ashish, Shazeer, Noam")
    );
    assert!(
        doc.metadata
            .extra
            .iter()
            .any(|(k, v)| k == "citation_doi" && v == "10.48550/arXiv.1706.03762")
    );
}

#[test]
fn json_ld_is_passed_through() {
    let html = r#"<html><head><title>JSON-LD</title>
        <script type="application/ld+json">{"@type":"Article","headline":"JSONLD_MARKER","author":"A"}</script>
        <script type="application/ld+json">{ broken </script>
        </head><body><article><p>Body text for extraction.</p></article></body></html>"#;
    let doc = process(html, &Options::default());
    assert_eq!(doc.metadata.json_ld.len(), 1);
    assert_eq!(doc.metadata.json_ld[0]["headline"], "JSONLD_MARKER");
}

#[test]
fn video_object_fills_an_empty_body() {
    let html = r#"<html><head><title>Video - Site</title>
        <meta name="description" content="Fallback description">
        <script type="application/ld+json">{"@type":"VideoObject","name":"Never Gonna Give You Up","description":"The official video.","uploadDate":"2009-10-24T23:57:33-07:00","author":{"@type":"Person","name":"Rick Astley"}}</script>
        </head><body><div id="player"></div></body></html>"#;
    let doc = process(html, &Options::default());
    assert!(doc.metadata.video_fallback, "{}", doc.markdown);
    assert!(
        doc.markdown.contains("# Never Gonna Give You Up"),
        "{}",
        doc.markdown
    );
    assert!(
        doc.markdown.contains("The official video."),
        "{}",
        doc.markdown
    );
    assert!(doc.markdown.contains("Rick Astley"), "{}", doc.markdown);
}

#[test]
fn german_block_page_is_classified() {
    let filler =
        "Wir schützen unsere Inhalte vor unbefugten und automatisierten Zugriffen. ".repeat(40);
    let html = page(&format!(
        "<h1>Ihre Anfrage wurde blockiert.</h1><p>{filler}</p>"
    ));
    let doc = process(
        &html,
        &Options {
            status: Some(403),
            ..Options::default()
        },
    );
    let barrier = doc.barrier.expect("barrier");
    assert_eq!(barrier.kind, BarrierKind::Block);
    assert_eq!(barrier.rule_id, "generic.denied_de");
}

#[test]
fn age_gate_is_classified_as_an_interstitial() {
    let html = page(
        "<h1>Age verification</h1><p>Please enter your birth date to continue.</p>\
         <form><input name=\"age\"></form>",
    );
    let doc = process(&html, &Options::default());
    let barrier = doc.barrier.expect("barrier");
    assert_eq!(barrier.kind, BarrierKind::Interstitial);
}

/// 导航和页脚很长（整页文字超过整页识别的上限），正文只有年龄确认表单：仍是年龄门（Steam 的
/// `/agecheck/` 页，0.80.3 返回 547 字的表单正文、退出码 0）。
fn store_page(main: &str) -> String {
    let nav: String = (0..60)
        .map(|i| format!("<li><a href=\"/c/{i}\">Category number {i} with games</a></li>"))
        .collect();
    let footer = "<p>All trademarks are property of their respective owners in the US and other countries. \
         VAT included in all prices where applicable. Privacy Policy, Legal, Subscriber Agreement, Refunds.</p>"
        .repeat(4);
    format!(
        "<html><head><title>GAME</title></head><body><header><nav><ul>{nav}</ul></nav></header>\
         <main>{main}</main><footer>{footer}</footer></body></html>"
    )
}

#[test]
fn an_age_gate_inside_a_large_site_frame_is_classified() {
    let html = store_page(
        "<h1>GAME</h1><h2>This game may contain content not appropriate for all ages.</h2>\
         <p>Please enter your birth date to continue:</p>\
         <select name=\"ageDay\"><option>1</option></select>\
         <select name=\"ageMonth\"><option>January</option></select>\
         <select name=\"ageYear\"><option>2026</option></select>\
         <p>This data is for verification purposes only and will not be stored.</p>\
         <a href=\"#\">View Page</a>",
    );
    let doc = process(&html, &Options::default());
    let barrier = doc.barrier.expect("barrier");
    assert_eq!(barrier.kind, BarrierKind::Interstitial);
    assert_eq!(barrier.rule_id, "generic.age_gate");
}

#[test]
fn a_short_article_about_age_checks_is_not_a_gate() {
    // 正文短，也含"verify your age"，但它是成段的文字
    let html = store_page(
        "<h1>New rules for online shops</h1><p>From next year, sites that sell games rated for adults \
         will have to ask buyers to verify your age before checkout, the regulator said on Tuesday, \
         adding that the rules apply to stores of every size.</p>",
    );
    assert!(process(&html, &Options::default()).barrier.is_none());
    // 正文长的页面完全不走正文识别
    let long = store_page(&format!(
        "<h1>Game</h1><p>Please enter your birth date to continue.</p>{}",
        "<p>A long description of the game that goes on for a while.</p>".repeat(40)
    ));
    assert!(process(&long, &Options::default()).barrier.is_none());
}
