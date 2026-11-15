use pagesift_content::{Options, ProcessWorkspace, process, process_with_workspace};

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
