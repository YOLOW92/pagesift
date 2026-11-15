//! 正文一级标题之前的站点框架块（logo、站点名链接、搜索提示）在只取正文时去掉。

use pagesift_content::{Options, process};

fn md(html: &str) -> String {
    process(html, &Options::default()).markdown
}

#[test]
fn drops_link_and_logo_blocks_before_the_title() {
    let html = r#"<html><body>
        <div class="top"><a href="/"><img src="/logo.png" alt="company logo"></a></div>
        <div class="brand"><a href="/">Acme Help Center</a></div>
        <div class="search">Search for articles</div>
        <div class="intro"><p>Some sites put a real sentence up here, which we keep.</p></div>
        <h2>Popular topics</h2>
        <h1>How to reset your password</h1>
        <p>Open the settings page and choose a new password, then confirm it by email.</p>
    </body></html>"#;
    let out = md(html);
    assert!(!out.contains("company logo"), "{out}");
    assert!(!out.contains("Acme Help Center"), "{out}");
    assert!(!out.contains("Search for articles"), "{out}");
    assert!(out.contains("real sentence"), "{out}");
    assert!(out.contains("## Popular topics"), "{out}");
    assert!(out.starts_with("Some sites"), "{out}");
}

#[test]
fn leaves_pages_without_a_leading_title_alone() {
    let html = r#"<html><body>
        <div><a href="/">Acme Help Center</a></div>
        <p>A page with no top-level heading keeps everything the other rules keep.</p>
    </body></html>"#;
    let out = md(html);
    assert!(out.contains("Acme Help Center"), "{out}");
}
