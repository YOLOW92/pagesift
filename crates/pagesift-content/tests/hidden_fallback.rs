//! 内容藏在 display:none 里或只在 <noscript> 里的单页应用页面。
use pagesift_content::{Options, process};

fn words(md: &str) -> usize {
    md.split_whitespace().count()
}

const PARA: &str = "The scholarship supports students from economically weaker backgrounds with yearly awards, \
    and applications are free and fully online with direct bank transfer of the funds to every selected student.";

#[test]
fn content_hidden_until_scripts_run_is_kept() {
    let html = format!(
        "<html><body><div id=app><div style=\"display:none\"><header><a href=/>Home</a></header>\
         <main><h1>Federal history portal</h1><p>{PARA}</p><p>{PARA}</p></main></div></div></body></html>"
    );
    for only_main_content in [true, false] {
        let doc = process(
            &html,
            &Options {
                only_main_content,
                ..Options::default()
            },
        );
        assert!(
            doc.markdown.contains("Federal history portal"),
            "{only_main_content}: {}",
            doc.markdown
        );
        assert!(words(&doc.markdown) > 40);
    }
}

#[test]
fn noscript_fallback_is_parsed_as_html() {
    let html = format!(
        "<html><body><app-root></app-root><noscript><img height=1 width=1 style=\"display:none\" \
         src=\"https://www.facebook.com/tr?id=1&ev=PageView&noscript=1\"><main><h1>Asha Scholarship</h1>\
         <p>{PARA}</p><ul><li>School students</li><li>Undergraduates</li></ul></main></noscript></body></html>"
    );
    let doc = process(&html, &Options::default());
    assert!(
        doc.markdown.contains("# Asha Scholarship"),
        "{}",
        doc.markdown
    );
    assert!(doc.markdown.contains("- School students"));
    // 不是转义后的原始 HTML，也不带追踪像素
    assert!(
        !doc.markdown.contains("\\<") && !doc.markdown.contains("facebook.com/tr"),
        "{}",
        doc.markdown
    );
}

#[test]
fn normal_pages_ignore_hidden_parts() {
    let html = format!(
        "<html><body><main><h1>Visible article</h1><p>{PARA}</p><p>{PARA}</p></main>\
         <div style=\"display:none\"><p>Hidden cookie dialog text that should never show up.</p></div>\
         <noscript><p>Please enable JavaScript to use this site.</p></noscript></body></html>"
    );
    let doc = process(&html, &Options::default());
    assert!(doc.markdown.contains("Visible article"));
    assert!(
        !doc.markdown.contains("cookie dialog") && !doc.markdown.contains("enable JavaScript"),
        "{}",
        doc.markdown
    );
}
