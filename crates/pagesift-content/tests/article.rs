//! 文章模式：页面声明是文章时只输出文章主体，去掉评论、推荐、分享和署名这类块。

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

fn news_page(head: &str, extra: &str) -> String {
    format!(
        "<html lang=\"en\"><head><title>Council approves budget</title>{head}</head><body>\
         <header><nav><a href=\"/\">Home</a> <a href=\"/news\">News</a></nav></header>\
         <main><article><h1>Council approves budget</h1>\
         <div class=\"byline\">By Jane Doe, City Reporter</div>\
         <div class=\"share-buttons\"><a href=\"/s/fb\">Share on Facebook</a> <a href=\"/s/x\">Share on X</a></div>\
         <div class=\"article-body\">{BODY}</div>\
         <figure><img src=\"/a.jpg\" alt=\"\"><figcaption>The council chamber on Tuesday.</figcaption></figure>\
         </article>{extra}</main></body></html>"
    )
}

const ARTICLE_LD: &str = r#"<script type="application/ld+json">{"@type":"NewsArticle","headline":"Council approves budget","datePublished":"2026-10-20T08:00:00Z"}</script>"#;

const COMMENTS: &str = r#"<section id="comments"><h2>12 comments</h2>
    <div class="comment" id="comment-1"><p>I think this is a terrible decision and the library should never have been allowed to cost that much money in the first place.</p></div>
    <div class="comment" id="comment-2"><p>Finally some investment in buses, I have been waiting for this for years and my commute is awful every single morning.</p></div>
    </section>"#;

const RELATED: &str = r#"<div class="related-stories"><h3>More from News</h3><ul>
    <li><a href="/a">Mayor announces new cycling lanes across the whole city centre</a></li>
    <li><a href="/b">School district reports record enrollment figures this autumn</a></li>
    <li><a href="/c">Library renovation timeline slips again after contractor dispute</a></li>
    <li><a href="/d">Transport authority publishes its annual passenger survey</a></li>
    <li><a href="/e">Council elections set for next spring, candidates announced</a></li></ul></div>"#;

#[test]
fn article_mode_keeps_the_story_and_drops_comments_and_related() {
    let out = md(&news_page(ARTICLE_LD, &format!("{COMMENTS}{RELATED}")));
    assert!(out.contains("# Council approves budget"), "{out}");
    assert!(out.contains("The budget takes effect in January"), "{out}");
    for gone in [
        "terrible decision",
        "Finally some investment",
        "cycling lanes",
        "Share on Facebook",
        "By Jane Doe",
        "council chamber",
    ] {
        assert!(!out.contains(gone), "{gone:?} should be removed:\n{out}");
    }
}

#[test]
fn pages_without_an_article_declaration_also_try_article_mode() {
    // 标准测试（WebMainBench / WCEB / WCXB）显示：只在 og:type=article 时才收窄会留下大量
    // 导航与推荐块，整体 F1 明显偏低。现在所有页面都先尝试正文定位；三道兜底仍会在正文过窄、
    // 或与整页文字比例过低时回退到默认规则。因此这里评论被排除、正文保留。
    let out = md(&news_page("", COMMENTS));
    assert!(
        !out.contains("terrible decision"),
        "comments are now outside the located article body:\n{out}"
    );
    assert!(out.contains("The budget takes effect in January"), "{out}");
}

#[test]
fn og_article_with_a_publish_date_counts() {
    let head = r#"<meta property="og:type" content="article"><meta property="article:published_time" content="2026-10-20T08:00:00Z">"#;
    let out = md(&news_page(head, COMMENTS));
    assert!(!out.contains("terrible decision"), "{out}");
    assert!(out.contains("The budget takes effect in January"), "{out}");
}

#[test]
fn a_short_article_under_a_long_comment_thread_is_not_mistaken_for_a_bad_root() {
    let many: String = (0..30)
        .map(|i| format!(r#"<div class="comment" id="comment-{i}"><p>Comment number {i}: I have a lot of thoughts about this open thread and want to share every single one of them here with everybody.</p></div>"#))
        .collect();
    let page = format!(
        "<html><head>{ARTICLE_LD}</head><body><article><h1>Open thread</h1>\
         <div class=\"entry-content\"><p>Our goal with these open threads is to give readers a chance to raise \
         questions about our work, our research and related topics in the comments below.</p></div></article>\
         <section id=\"comments\">{many}</section></body></html>"
    );
    let out = md(&page);
    assert!(out.contains("Our goal with these open threads"), "{out}");
    assert!(!out.contains("Comment number 7"), "{out}");
}

#[test]
fn whole_page_conversion_ignores_article_mode() {
    let out = process(
        &news_page(ARTICLE_LD, COMMENTS),
        &Options {
            only_main_content: false,
            ..Options::default()
        },
    )
    .markdown;
    assert!(out.contains("terrible decision"), "{out}");
}

#[test]
fn list_shaped_article_body_is_not_filtered_away() {
    // 正文主体大半是链接列表（维基的“列表”条目、文档的索引页）：严格规则的链接密度把纯链接的
    // 列表整个删掉，保留下来的也只数非链接文字。严格规则删掉主体一半以上时改用默认规则渲染主体
    let list = |name: &str, n: usize, suffix: &str| {
        let items: String = (0..n)
            .map(|i| {
                format!("<li><a href=\"/wiki/{name}{i}\">{name} language {i}</a>{suffix}</li>")
            })
            .collect();
        format!("<h2>{name}</h2><div class=\"div-col\"><ul>{items}</ul></div>")
    };
    let html = format!(
        "<html lang=\"en\"><head><title>List of languages</title>{ARTICLE_LD}</head><body>\
         <header><nav><a href=\"/\">Home</a></nav></header>\
         <main><article><h1>List of languages</h1>{BODY}{}{}</article>{RELATED}</main></body></html>",
        list("Alpha", 30, ""),
        list("Beta", 60, " (1990)")
    );
    let out = md(&html);
    assert!(out.contains("The budget takes effect in January"), "{out}");
    assert!(
        out.contains("Alpha language 29") && out.contains("Beta language 59"),
        "{out}"
    );
    assert!(!out.contains("cycling lanes"), "{out}");
}

/// 手风琴、FAQ 的标题按钮（`aria-expanded`、`aria-controls`）是下面那段内容的小标题：
/// 文章模式只输出白名单元素、删按钮时也保留它；"展开全部"这类短按钮照删。
#[test]
fn disclosure_headers_of_accordions_are_kept() {
    let item = |n: usize, q: &str| {
        format!(
            "<div class=\"accordion__item\">\
             <button type=\"button\" class=\"accordion__header\" aria-expanded=\"false\" \
             aria-controls=\"panel-{n}\">{q}</button>\
             <div class=\"accordion__content\" id=\"panel-{n}\"><p>{BODY}</p></div></div>"
        )
    };
    let html = format!(
        "<html><body><main><article><h1>Questions</h1>{BODY}\
         <div class=\"accordion\"><button type=\"button\">Expand all</button>{}{}</div>\
         </article></main></body></html>",
        item(1, "How long does the budget review take?"),
        item(2, "Who pays for the library renovation?")
    );
    let out = md(&html);
    for q in [
        "How long does the budget review take?",
        "Who pays for the library renovation?",
    ] {
        assert!(out.contains(q), "{q}: {out}");
    }
    assert!(!out.contains("Expand all"), "{out}");
}

const OG_ARTICLE: &str = r#"<meta property="og:type" content="article">"#;

#[test]
fn a_headline_article_that_only_holds_the_teaser_yields_the_body_outside_it() {
    // 新闻稿模板：标题和截断的摘要单独包成一个 <article>，正文在它外面
    let html = format!(
        "<html><head><title>Council approves budget</title>{OG_ARTICLE}</head><body>\
         <article class=\"article-topper\"><header><h1>Council approves budget</h1></header>\
         <p class=\"article-topper__excerpt\">The city council approved the new budget on Tuesday after a long debate about \
         funding for public transport, schools and the renovation of the old central library. Supporters \
         said the plan would cut commuting times, while critics warned that the cost of the library project \
         had doubled since it was first proposed three years ago. The budget takes effect in January and \
         will be reviewed again in the middle of next year, when the council expects updated figures from \
         the transport authority. Council members will meet residents in every district...</p></article>\
         <div class=\"columns\"><div class=\"body\">{BODY}<p>Council members will meet residents in every \
         district before the review, the mayor's office said in a statement on Wednesday morning.</p>\
         <p>The transport authority has promised to publish monthly ridership numbers so that the council \
         can check whether the new bus lanes are reducing journey times as the plan assumes.</p>\
         <p>Opposition members said they would table amendments at the next session, arguing that the \
         library money should be released in stages tied to the contractor meeting its deadlines.</p>\
         <p>Business groups welcomed the decision, saying that reliable transport would help shops in the \
         old town recover from two difficult years of road works and falling numbers of visitors.</p>\
         <p>The full budget documents, including the breakdown for each district, will be published on the \
         council website next week together with the minutes of Tuesday's meeting.</p></div>\
         <aside><a href=\"/x\">Most read</a></aside></div></body></html>"
    );
    let out = md(&html);
    assert!(out.contains("# Council approves budget"), "{out}");
    assert!(out.contains("will be reviewed again"), "{out}");
    assert!(out.contains("meet residents in every district"), "{out}");
    assert!(!out.contains("every district..."), "{out}");
}

#[test]
fn story_cards_in_a_side_column_are_not_part_of_the_article() {
    // 没有 <article>、也没有正文类名的两栏版式：右栏是别的新闻的卡片（标题链接 + 截断的摘要）
    let card = |i: usize| {
        format!(
            "<div class=\"card\"><h5><a href=\"/news/{i}\">Another story number {i} from the city desk</a></h5>\
             <p>CITY: Officials said on Monday that the plans for the new district were still being \
             discussed by several committees and residents of the area...</p></div>"
        )
    };
    let cards: String = (0..8).map(card).collect();
    let html = format!(
        "<html><head><title>Council approves budget | City News</title>{OG_ARTICLE}</head><body>\
         <div class=\"wrapper\"><div class=\"left\"><div class=\"banner\"><h2>Council approves budget</h2>\
         <p>City News<br>28 Oct 2026</p><div class=\"text\">{BODY}</div></div></div>\
         <div class=\"right\"><h3>International</h3>{cards}</div></div></body></html>"
    );
    let out = md(&html);
    assert!(out.contains("Council approves budget"), "{out}");
    assert!(out.contains("will be reviewed again"), "{out}");
    assert!(!out.contains("Another story number"), "{out}");
    assert!(!out.contains("several committees"), "{out}");
}

#[test]
fn a_section_name_in_h1_is_not_taken_for_the_headline() {
    // 页面唯一的 <h1> 是栏目名，文章标题是 <h2>；正文标了 articleBody，旁边是一长串新闻链接
    let feed: String = (0..30)
        .map(|i| {
            format!(
                "<li><a href=\"/n/{i}\">Headline of another news item number {i} today</a></li>"
            )
        })
        .collect();
    let html = format!(
        "<html><head><title>Council approves budget</title>{OG_ARTICLE}</head><body>\
         <main><div class=\"container\"><h1>Asia</h1><div class=\"page\"><div class=\"left\">\
         <h2>Council approves budget</h2><div itemprop=\"articleBody\">{BODY}</div></div>\
         <div class=\"feed\"><h3>News feed</h3><ul>{feed}</ul></div></div></div></main></body></html>"
    );
    let out = md(&html);
    assert!(out.contains("Council approves budget"), "{out}");
    assert!(out.contains("will be reviewed again"), "{out}");
    assert!(!out.contains("another news item"), "{out}");
    assert!(!out.contains("Asia"), "{out}");
}
