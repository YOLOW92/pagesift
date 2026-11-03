//! 正文里的小标题和文章标题不能被框架规则误删。

use pagesift_content::{Options, process};

fn md(html: &str) -> String {
    process(html, &Options::default()).markdown
}

const BODY: &str = "<p>The city council approved the new budget on Tuesday after a long debate about \
    funding for public transport, schools and the renovation of the old central library.</p>\
    <p>Supporters said the plan would cut commuting times, while critics warned that the cost of the \
    library project had doubled since it was first proposed three years ago.</p>\
    <p>The budget takes effect in January and will be reviewed again in the middle of next year, \
    when the council expects updated figures from the transport authority.</p>";

/// 文档站按标题文字生成的锚点 id 里有噪声词（recommended、sharing）：小标题照常保留。
#[test]
fn heading_slug_id_with_noise_word_is_kept() {
    let html = format!(
        "<html><head><title>Filter node output</title></head><body><main><article>\
         <h1>Filter node output</h1>{BODY}\
         <h2 id=\"not-recommended-filtering-in-publishing-node\">Not Recommended: Filtering in Publishing Node</h2>\
         {BODY}<h2 id=\"sharing-data-between-nodes\">Sharing data between nodes</h2>{BODY}\
         </article></main></body></html>"
    );
    let out = md(&html);
    assert!(
        out.contains("## Not Recommended: Filtering in Publishing Node"),
        "{out}"
    );
    assert!(out.contains("## Sharing data between nodes"), "{out}");
}

/// 不是按文字生成的 id 照旧当类名看：`id="related-posts"` 的块还是删掉。
#[test]
fn non_slug_noise_id_is_still_dropped() {
    let html = format!(
        "<html><head><title>Budget</title></head><body><main><article><h1>Budget</h1>{BODY}\
         <div id=\"related-posts\"><p>Other stories you may enjoy from our newsroom this week.</p></div>\
         </article></main></body></html>"
    );
    let out = md(&html);
    assert!(!out.contains("Other stories"), "{out}");
}

/// 标题文字本身就是指向本节锚点的链接（"复制链接"自链接）：是小标题，不是链接块。
#[test]
fn self_anchored_heading_is_kept_in_article_mode() {
    let ld =
        r#"<script type="application/ld+json">{"@type":"Article","headline":"AI SDK"}</script>"#;
    let html = format!(
        "<html><head><title>AI SDK</title>{ld}</head><body><main><article><h1>AI SDK</h1>{BODY}\
         <div class=\"container\"><h2><span id=\"generating-structured-data\"></span>\
         <a href=\"#generating-structured-data\">Generating structured data</a>\
         <a href=\"#generating-structured-data\" aria-label=\"Copy link\"></a></h2></div>{BODY}\
         <div class=\"more\"><h3><a href=\"/other-story\">Another story elsewhere</a></h3></div>\
         </article></main></body></html>"
    );
    let out = md(&html);
    assert!(out.contains("Generating structured data"), "{out}");
    // 指向别的页面的标题链接照旧按链接密度删
    assert!(!out.contains("Another story elsewhere"), "{out}");
}

/// 页面上的 `<h1>` 和页头一起被删时，补标题用和文档标题一致的 `<h1>` 文字，不用拼着站名的 `<title>`。
#[test]
fn hidden_h1_agreeing_with_title_replaces_document_title() {
    let ld = r#"<script type="application/ld+json">{"@type":"Article","headline":"Moms Don't Have Time"}</script>"#;
    let html = format!(
        "<html><head><title>Moms Don't Have Time - Best Book for Moms | NAPPA Awards</title>{ld}</head><body>\
         <header class=\"site-header\"><h1>Moms Don't Have Time</h1><nav><a href=\"/\">Home</a> \
         <a href=\"/a\">Awards</a> <a href=\"/b\">Winners</a> <a href=\"/c\">Contact</a></nav></header>\
         <main><article>{BODY}</article></main></body></html>"
    );
    let out = md(&html);
    assert!(out.starts_with("# Moms Don't Have Time\n"), "{out}");
    assert!(!out.contains("NAPPA Awards"), "{out}");
}
