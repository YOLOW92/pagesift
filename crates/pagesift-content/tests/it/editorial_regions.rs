//! Article identity must bound header expansion even in mixed recommendation layouts.
use pagesift_content::{Options, process};

#[test]
fn unrelated_h1_cannot_expand_a_story_into_a_recommendation_grid() {
    let prose =
        "Independent reporting follows the evidence and explains the consequences for residents. "
            .repeat(12);
    for order in [false, true] {
        let recommendation = "<section><article><h1>Unrelated recommended story</h1><p>Recommendation teaser.</p></article></section>";
        let story = format!(
            "<article><header><h2 itemprop='headline'>Actual story title</h2></header><div class='article-body'><p>{prose}</p><blockquote><p>Important witness quotation remains.</p></blockquote><p>{prose}</p></div></article>"
        );
        let content = if order {
            format!("{recommendation}{story}")
        } else {
            format!("{story}{recommendation}")
        };
        let html = format!(
            "<head><meta property='og:type' content='article'><meta property='article:published_time' content='2026-10-22'></head><main>{content}</main>"
        );
        let md = process(&html, &Options::default()).markdown;
        assert!(md.contains("Actual story title"), "{md}");
        assert!(md.contains("Important witness quotation"), "{md}");
        assert!(!md.contains("Unrelated recommended story"), "{md}");
        assert!(!md.contains("Recommendation teaser"), "{md}");
    }
}

#[test]
fn headline_identity_beats_a_larger_nested_recommendation_article() {
    let main = "Original editorial account with sufficient detail for readers to understand what happened. ".repeat(9);
    let teaser = "A different article preview contains unrelated statements and should remain outside this story. ".repeat(6);
    let cards = (0..8)
        .map(|i| format!("<article><h2>Other item {i}</h2><p>{teaser}</p></article>"))
        .collect::<String>();
    let html = format!(
        "<head><title>Actual reported event | Publisher</title><meta property='og:type' content='article'><meta property='article:published_time' content='2026-10-22'></head><main><article><h1>Actual reported event</h1><p>{main}</p></article><article><h3>More reading</h3>{cards}</article></main>"
    );
    let md = process(&html, &Options::default()).markdown;
    assert!(md.contains("Original editorial account"), "{md}");
    assert!(!md.contains("different article preview"), "{md}");
}

#[test]
fn identified_article_keeps_short_trailing_sections() {
    let long =
        "Substantial reporting with independent context and careful explanation. ".repeat(100);
    let html = format!(
        "<head><title>Careful reporting from the field</title><meta property='og:type' content='article'><meta property='article:published_time' content='2026-10-22'></head><article><section><h1>Careful reporting from the field</h1><p>{long}</p></section><section><h2>Next steps</h2><p>A short but essential conclusion remains part of the same article.</p></section></article>"
    );
    let md = process(&html, &Options::default()).markdown;
    assert!(md.contains("essential conclusion"), "{md}");
}

#[test]
fn headline_binding_handles_unsegmented_cjk_titles() {
    let main = "这是经过核实的文章正文，包含背景、事实与分析。".repeat(35);
    let other = "这是另外一篇报道的摘要，不属于当前文章。".repeat(50);
    let html = format!(
        "<head><title>关于城市交通改善方案的现场报道 | 新闻</title><meta property='og:type' content='article'><meta property='article:published_time' content='2026-10-22'></head><main><article><h1>关于城市交通改善方案的现场报道</h1><p>{main}</p></article><article><h2>其他文章</h2><p>{other}</p></article></main>"
    );
    let md = process(&html, &Options::default()).markdown;
    assert!(md.contains("经过核实的文章正文"), "{md}");
    assert!(!md.contains("另外一篇报道的摘要"), "{md}");
}

#[test]
fn document_level_title_is_retained_without_importing_other_articles() {
    let text = "The article provides substantial evidence, context, and detailed reporting from multiple sources. ".repeat(15);
    let html = format!(
        "<head><title>Evidence from the city council | News</title><meta property='og:type' content='article'><meta property='article:published_time' content='2026-10-22'></head><main><h1>Evidence from the city council</h1><article><p>{text}</p><p>{text}</p></article><aside><h2>Other links</h2></aside></main>"
    );
    let md = process(&html, &Options::default()).markdown;
    assert!(md.contains("Evidence from the city council"), "{md}");
    assert!(!md.contains("Other links"), "{md}");
}

#[test]
fn unmarked_short_prose_is_bounded_in_all_body_outputs() {
    let prose = "Residents reported damage to the vehicles this morning. Police inspected the site and recorded the evidence. Scheduled appointments will resume after repairs are complete. The council will provide another update to affected residents later today. ".repeat(2);
    let html = format!(
        "<title>Vehicles damaged at the local care centre</title><body><div><a href='/'>Archive link</a></div><div><p>Vehicles damaged at the local care centre</p><small>Published today</small>{prose}<div class='share'>SHARE THIS</div></div><div>Municipal office opening hours and telephone directory.</div></body>"
    );
    let opts = Options {
        plain_text: true,
        clean_html: true,
        ..Options::default()
    };
    let doc = process(&html, &opts);
    for output in [
        &doc.markdown,
        doc.plain_text.as_ref().unwrap(),
        doc.html.as_ref().unwrap(),
    ] {
        assert!(output.contains("Scheduled appointments"), "{output}");
        assert!(!output.contains("telephone directory"), "{output}");
    }
    let full = process(
        &html,
        &Options {
            only_main_content: false,
            ..opts
        },
    );
    assert!(full.markdown.contains("telephone directory"));
}

#[test]
fn article_attachments_keep_product_details_and_video_caption_but_not_recommendations() {
    let prose = "Our independent review explains how these devices perform in daily use, including measurements and limitations. ".repeat(10);
    let card = "<div class='product-callout'><div class='product-inner'><a href='/buy/widget'>Reviewed widget</a><div class='product-price'>$29</div><div class='product-description'><a href='/buy/widget'>Available from the manufacturer</a></div></div></div>";
    let video = "<figure><div data-video-id='42'><video src='/clip.mp4'></video><h3>Field demonstration</h3></div><figcaption><div>The demonstration shows the measured behaviour in the field.</div></figcaption></figure>";
    let html = format!(
        "<title>Independent device review</title><meta property='og:type' content='article'><meta property='article:published_time' content='2026-10-22'><article><h1>Independent device review</h1><p>{prose}</p>{card}{video}<div class='recommended'>{card}<p>Unrelated recommendation</p></div><p>{prose}</p></article>"
    );
    let md = process(&html, &Options::default()).markdown;
    assert!(md.contains("Available from the manufacturer"), "{md}");
    assert!(md.contains("$29"), "{md}");
    assert!(md.contains("measured behaviour"), "{md}");
    assert!(!md.contains("Unrelated recommendation"), "{md}");
    assert_eq!(md.matches("Available from the manufacturer").count(), 1);
}

#[test]
fn ambiguous_unmarked_prose_does_not_select_one_of_two_stories() {
    let prose = "An independent report describes an event in detail. Residents provided further information to investigators. The investigation continues and more information will be published. ".repeat(3);
    let html = format!(
        "<title>Reports from the community</title><div><p>Reports from the community</p>{prose}FIRST</div><div><p>Reports from the community</p>{prose}SECOND</div>"
    );
    let md = process(&html, &Options::default()).markdown;
    assert!(md.contains("FIRST") && md.contains("SECOND"), "{md}");
}

#[test]
fn attachment_lookalikes_outside_article_do_not_trigger_article_exceptions() {
    let html = "<title>Store catalogue</title><main><h1>Store catalogue</h1><div class='product-callout'><a href='/one'>First product</a><div class='product-price'>$20</div></div><div class='product-callout'><a href='/two'>Second product</a><div class='product-price'>$40</div></div></main>";
    let md = process(html, &Options::default()).markdown;
    assert!(
        md.contains("First product") && md.contains("Second product"),
        "{md}"
    );
}

#[test]
fn hidden_video_and_playlist_never_inherit_attachment_protection() {
    let prose = "This is a complete editorial report providing the context and detail necessary for readers. ".repeat(12);
    let html = format!(
        "<title>Report from the field</title><meta property='og:type' content='article'><meta property='article:published_time' content='2026-10-22'><article><h1>Report from the field</h1><p>{prose}</p><figure><video src='/clip'></video><figcaption>Visible editorial caption</figcaption><div hidden>HIDDEN CONTROLS</div><div class='playlist'><a href='/unrelated'>UNRELATED PLAYLIST</a></div></figure></article>"
    );
    let md = process(&html, &Options::default()).markdown;
    assert!(md.contains("Visible editorial caption"), "{md}");
    assert!(!md.contains("HIDDEN CONTROLS"), "{md}");
    assert!(!md.contains("UNRELATED PLAYLIST"), "{md}");
}

#[test]
fn embedded_source_quote_and_procedural_product_links_remain_editorial() {
    let prose = "Reporting includes evidence gathered from independent sources and practical instructions for readers. ".repeat(12);
    let html = format!(
        "<title>Evidence and instructions</title><meta property='og:type' content='article'><meta property='article:published_time' content='2026-10-22'><article><h1>Evidence and instructions</h1><div class='article-body'><p>{prose}</p><inline-embed><blockquote class='blockquote-embed'><div class='blockquote-embed__content'><p>Readers can contact the confidential source desk using the secure channel described here. The source desk accepts documents and protects the confidentiality of each submission.</p></div></blockquote></inline-embed><h3>Preparation materials</h3><p>Prepare these materials before continuing.</p><p><a href='https://store.example/shop/item'>Required preparation kit</a></p><ul><li><a href='/other?campaign=RelatedStories'>OTHER ONE</a></li><li><a href='/other-two?campaign=RelatedStories'>OTHER TWO</a></li><li><a href='/other-three?campaign=RelatedStories'>OTHER THREE</a></li></ul></div></article>"
    );
    let md = process(&html, &Options::default()).markdown;
    assert!(md.contains("confidential source desk"), "{md}");
    assert!(md.contains("Required preparation kit"), "{md}");
    assert!(!md.contains("OTHER ONE"), "{md}");
}

#[test]
fn quotation_protection_does_not_restore_embed_loaders_or_link_catalogues() {
    let prose = "This article contains a carefully reported account and explains the evidence for readers in detail. ".repeat(12);
    let html = format!(
        "<title>Carefully reported account</title><meta property='og:type' content='article'><meta property='article:published_time' content='2026-10-22'><article><h1>Carefully reported account</h1><p>{prose}</p><blockquote class='social-embed'><a href='/social'>LOADER PLACEHOLDER</a></blockquote><blockquote class='quote-embed'><a href='/a'>UNRELATED CATALOGUE ENTRY WITH A LONG TITLE</a><a href='/b'>ANOTHER UNRELATED CATALOGUE ENTRY WITH A LONG TITLE</a><a href='/c'>A THIRD UNRELATED CATALOGUE ENTRY WITH A LONG TITLE</a></blockquote></article>"
    );
    let md = process(&html, &Options::default()).markdown;
    assert!(!md.contains("LOADER PLACEHOLDER"), "{md}");
    assert!(!md.contains("UNRELATED CATALOGUE"), "{md}");
}
