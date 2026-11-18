use pagesift_content::{
    DomTagFilter, Options, ProcessWorkspace, TagFilter, process, process_with_workspace,
    with_parsed_dom, yolowhtml_dom::ParseScratch,
};

#[test]
fn workspace_matches_fresh_processing_across_reuse() {
    let pages = [
        "<html><head><title>One</title></head><body><main><h1>A &amp; B</h1><p>First page.</p></main></body></html>",
        "<html>\r\n<head><title>Two</title></head>\r\n<body><article><h1>Second</h1><p>Another page.</p></article></body></html>",
        "<body><nav>Menu</nav><main><h1>Third</h1><p>Final page with <a href='/x'>a link</a>.</p></main></body>",
    ];
    let options = Options::default();
    let mut workspace = ProcessWorkspace::default();

    for _ in 0..4 {
        for html in pages {
            let fresh = process(html, &options);
            let reused = process_with_workspace(html, &options, &mut workspace);
            assert_eq!(reused.markdown, fresh.markdown);
            assert_eq!(reused.metadata.title, fresh.metadata.title);
            assert_eq!(reused.metadata.is_article, fresh.metadata.is_article);
            assert_eq!(reused.barrier, fresh.barrier);
        }
    }
}

#[test]
fn dom_tag_filter_matches_string_tag_filter_byte_for_byte() {
    let html = "<html><head><title>T &amp; C</title><script type=\"application/ld+json\">{\"@type\":\"Article\"}</script></head>\
        <body><nav>menu</nav><article class=\"post\"><h1>A</h1><p>中文与 Emoji 🚀<span class=\"ad\">ad</span></p></article>\
        <div id=\"side\"><article><p>nested</p></article></div><footer>f</footer></body></html>";
    let mut scratch = ParseScratch::default();

    for (inc, exc) in [
        (
            vec![],
            vec!["nav".to_string(), ".ad".into(), "#side".into()],
        ),
        (
            vec!["article".to_string(), "footer".into()],
            vec![".ad".to_string()],
        ),
        (vec!["#side".to_string(), "article".into()], vec![]),
    ] {
        let legacy = TagFilter::new(&inc, &exc).unwrap().unwrap();
        let dom_filter = DomTagFilter::new(&inc, &exc).unwrap().unwrap();
        let want = legacy.apply(html);
        let (got, has_ld) = dom_filter.apply_with_scratch_then(html, &mut scratch, |dom| {
            dom.root()
                .descendants()
                .any(|n| n.as_element().is_some_and(|e| e.name() == "script"))
        });
        assert_eq!(got, want);
        assert!(has_ld);
    }

    assert!(DomTagFilter::new(&[], &[" ".into()]).unwrap().is_none());
    assert_eq!(
        DomTagFilter::new(&["p[".into()], &[]).unwrap_err(),
        TagFilter::new(&["p[".into()], &[]).unwrap_err()
    );
    let titles: Vec<Option<String>> = ["<title>Alpha</title>", "<title>Beta &amp; Gamma</title>"]
        .into_iter()
        .map(|h| {
            with_parsed_dom(h, &mut scratch, |dom| {
                dom.root()
                    .descendants()
                    .find(|n| n.as_element().is_some_and(|e| e.name() == "title"))
                    .map(|n| n.text())
            })
        })
        .collect();
    assert_eq!(
        titles,
        vec![Some("Alpha".into()), Some("Beta & Gamma".into())]
    );
}
