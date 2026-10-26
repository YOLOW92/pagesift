//! 正文完整度警告：结果可能只有正文一部分时要说出来，完整的结果不报。
use pagesift_content::{Options, process};

const HEAD: &str = "<head><title>Field notes from the river survey</title><meta property='og:type' content='article'><meta property='article:published_time' content='2026-10-22'></head>";

fn warnings(html: &str) -> Vec<String> {
    process(html, &Options::default()).coverage.warnings()
}

#[test]
fn complete_article_has_no_coverage_warning() {
    let prose =
        "The survey team measured flow, sediment and temperature at each of the twelve stations. "
            .repeat(10);
    let html = format!(
        "{HEAD}<body><nav><a href='/'>Home</a> <a href='/a'>About</a></nav><article><h1>Field notes from the river survey</h1><p>{prose}</p><p>{prose}</p></article><footer>Contact us</footer></body>"
    );
    assert!(warnings(&html).is_empty(), "{:?}", warnings(&html));
}

#[test]
fn selecting_only_a_lead_reports_partial_content() {
    // 标题和导语在一个小 <article> 里，正文在它后面的兄弟节点：文章模式只选中导语
    let lead =
        "A short lead paragraph that introduces the survey and its purpose for readers. ".repeat(6);
    let body = "<p>Downstream stations recorded much higher sediment loads after the spring floods, and the team traced the source to two eroding banks upstream of the old mill.</p>".repeat(12);
    let html = format!(
        "{HEAD}<body><article><h1>Field notes from the river survey</h1><p>{lead}</p></article><div class='entry'>{body}</div></body>"
    );
    let md = process(&html, &Options::default()).markdown;
    // 夹具复现的是"选中区域只有导语"；提取规则改进到不再丢正文时，换一个仍会丢的夹具
    assert!(!md.contains("eroding banks"), "{md}");
    let w = warnings(&html);
    assert!(
        w.iter().any(|w| w.starts_with("partial_content:")),
        "{w:?}\n{md}"
    );
}

#[test]
fn taking_one_post_of_a_thread_reports_replies_omitted() {
    let post =
        "I have been trying to calibrate the flow meter for weeks and the readings keep drifting. "
            .repeat(4);
    let reply = "Check the reference voltage first; a drifting supply explains exactly this kind of slow error. ".repeat(3);
    let replies = (0..10)
        .map(|i| format!("<div class='comment-body'><p>Reply {i}: {reply}</p></div>"))
        .collect::<String>();
    let html = format!(
        "{HEAD}<body><div id='comments' class='comments-thread'><article><h1>Field notes from the river survey</h1><p>{post}</p></article>{replies}</div></body>"
    );
    let md = process(&html, &Options::default()).markdown;
    assert!(!md.contains("Reply 9"), "{md}");
    let w = warnings(&html);
    assert!(
        w.iter().any(|w| w.starts_with("replies_omitted:")),
        "{w:?}\n{md}"
    );
}

#[test]
fn blog_comments_outside_the_article_are_not_reported() {
    let prose =
        "The survey team measured flow, sediment and temperature at each of the twelve stations. "
            .repeat(6);
    let reply =
        "Great write-up, thanks for sharing the station data with everyone here. ".repeat(3);
    let comments = (0..20)
        .map(|i| format!("<li class='comment'><p>Comment {i}: {reply}</p></li>"))
        .collect::<String>();
    let html = format!(
        "{HEAD}<body><article><h1>Field notes from the river survey</h1><p>{prose}</p></article><section id='comments'><ol>{comments}</ol></section></body>"
    );
    assert!(warnings(&html).is_empty(), "{:?}", warnings(&html));
}

#[test]
fn full_page_mode_never_reports() {
    let lead =
        "A short lead paragraph that introduces the survey and its purpose for readers. ".repeat(2);
    let body =
        "<p>Downstream stations recorded much higher sediment loads after the spring floods.</p>"
            .repeat(20);
    let html =
        format!("{HEAD}<body><article><h1>T</h1><p>{lead}</p></article><div>{body}</div></body>");
    let opts = Options {
        only_main_content: false,
        ..Options::default()
    };
    assert!(process(&html, &opts).coverage.warnings().is_empty());
}
