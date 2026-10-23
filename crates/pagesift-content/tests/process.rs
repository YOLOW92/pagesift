use pagesift_content::{Options, process};
use url::Url;

fn opts(base: &str) -> Options {
    Options {
        base_url: Some(Url::parse(base).unwrap()),
        only_main_content: true,
        ..Default::default()
    }
}

fn md(html: &str) -> String {
    process(html, &Options::default()).markdown
}

const ARTICLE: &str = r#"<!doctype html>
<html lang="en"><head>
<title>  Rust 2026 roadmap | Example Blog </title>
<meta name="description" content="What is coming next.">
<meta property="og:title" content="Rust 2026 roadmap">
<meta property="article:published_time" content="2026-10-01T08:00:00Z">
<link rel="canonical" href="/posts/rust-2026">
</head><body>
<header class="site-header"><nav><a href="/">Home</a> <a href="/about">About</a></nav></header>
<div class="layout">
  <aside class="sidebar"><h3>Popular</h3><ul><li><a href="/a">Some other post</a></li></ul></aside>
  <div class="post-content">
    <h1>Rust 2026 roadmap</h1>
    <p>The project goals for next year focus on async, tooling, and the compiler, which together cover most of what users ask for.</p>
    <p>Second paragraph with a <a href="/docs/async">relative link</a>, some <strong>bold text</strong>, and <code>inline_code()</code>.</p>
    <div class="share-buttons">Share on X · Share on Facebook</div>
    <p>Third paragraph, long enough to count as real content in the scoring pass, with commas, clauses, and more.</p>
  </div>
</div>
<div id="cookie-banner">We use cookies. Accept all?</div>
<footer>© 2026 Example</footer>
</body></html>"#;

#[test]
fn extracts_main_content_and_drops_chrome() {
    let doc = process(ARTICLE, &opts("https://example.com/posts/rust-2026"));
    let m = &doc.markdown;
    assert!(m.starts_with("# Rust 2026 roadmap\n\n"), "{m}");
    assert!(
        m.contains("[relative link](https://example.com/docs/async)"),
        "{m}"
    );
    assert!(m.contains("**bold text**"));
    assert!(m.contains("`inline_code()`"));
    for noise in ["Home", "Popular", "Share on", "cookies", "© 2026"] {
        assert!(!m.contains(noise), "noise {noise:?} leaked:\n{m}");
    }
}

#[test]
fn extracts_metadata() {
    let doc = process(ARTICLE, &opts("https://example.com/posts/rust-2026"));
    let meta = doc.metadata;
    assert_eq!(
        meta.title.as_deref(),
        Some("Rust 2026 roadmap | Example Blog")
    );
    assert_eq!(meta.description.as_deref(), Some("What is coming next."));
    assert_eq!(meta.language.as_deref(), Some("en"));
    assert_eq!(
        meta.canonical_url.as_deref(),
        Some("https://example.com/posts/rust-2026")
    );
    assert_eq!(
        meta.favicon.as_deref(),
        Some("https://example.com/favicon.ico")
    );
    assert_eq!(meta.published_time.as_deref(), Some("2026-10-01T08:00:00Z"));
    assert!(
        meta.extra
            .iter()
            .any(|(k, v)| k == "og:title" && v == "Rust 2026 roadmap")
    );
}

#[test]
fn full_page_keeps_everything_visible() {
    let doc = process(
        ARTICLE,
        &Options {
            base_url: None,
            only_main_content: false,
            ..Default::default()
        },
    );
    assert!(doc.markdown.contains("Home"));
    assert!(doc.markdown.contains("© 2026 Example"));
}

#[test]
fn chinese_article_beats_link_heavy_sidebar() {
    let html = r#"<html><body>
    <div class="nav-links"><a href="/1">首页</a><a href="/2">新闻</a><a href="/3">体育</a><a href="/4">财经</a></div>
    <div id="artibody">
      <p>国家统计局今天发布数据，前三季度国内生产总值同比增长，其中第三产业增加值增长较快，消费对经济增长的贡献率继续提高。</p>
      <p>分行业看，信息传输、软件和信息技术服务业增加值增长明显，高技术制造业投资保持较快增长，新动能持续壮大。</p>
    </div>
    <div class="related"><a href="/x">相关阅读：另一篇文章的标题很长很长很长很长很长很长</a></div>
    </body></html>"#;
    let m = md(html);
    assert!(m.starts_with("国家统计局今天发布数据"), "{m}");
    assert!(m.contains("新动能持续壮大。"));
    assert!(!m.contains("首页"));
    assert!(!m.contains("相关阅读"));
}

#[test]
fn nested_and_ordered_lists() {
    let html = r#"<article><h2>Steps</h2>
    <ol start="3"><li>Install</li><li>Configure<ul><li>edit <code>pagesift.toml</code></li><li>set the port</li></ul></li><li>Run</li></ol>
    <p>That is the whole setup process, described in enough words to be the main content of this page.</p></article>"#;
    let m = md(html);
    let expected = "3. Install\n4. Configure\n   - edit `pagesift.toml`\n   - set the port\n5. Run";
    assert!(m.contains(expected), "{m}");
}

#[test]
fn fenced_code_with_language_and_backticks() {
    let html = r#"<article><p>Example code follows below, with enough text here to be scored as content.</p>
    <pre><code class="language-rust">fn main() {
    println!("```");
}
</code></pre></article>"#;
    let m = md(html);
    assert!(
        m.contains("````rust\nfn main() {\n    println!(\"```\");\n}\n````"),
        "{m}"
    );
}

#[test]
fn data_table_becomes_gfm() {
    let html = r#"<article><p>Benchmark results are summarised in the table below for all three engines.</p>
    <table><thead><tr><th>Engine</th><th>Recall</th></tr></thead>
    <tbody><tr><td>pagesift</td><td>n/a</td></tr><tr><td>a | b</td><td>1</td></tr></tbody></table></article>"#;
    let m = md(html);
    assert!(
        m.contains("| Engine | Recall |\n| --- | --- |\n| pagesift | n/a |\n| a \\| b | 1 |"),
        "{m}"
    );
}

#[test]
fn layout_table_is_not_a_table() {
    let html = r#"<body><table><tr><td><p>Only one cell holding the entire article body, which is long enough.</p></td></tr></table></body>"#;
    let m = md(html);
    assert_eq!(
        m,
        "Only one cell holding the entire article body, which is long enough.\n"
    );
}

#[test]
fn images_links_and_base_href() {
    let html = r##"<html><head><base href="https://cdn.example.org/blog/"></head><body><article>
    <p>An illustrated paragraph that is long enough to be selected as the main content block.</p>
    <p><img src="img/a b.png" alt="A [diagram]"> <img src="data:image/png;base64,AAAA" alt="inline"></p>
    <p>Links in running text: a <a href="javascript:void(0)">fake</a> one, an <a href="#top">anchor</a>, and a real <a href="post(1)">paren</a> link to another article on this site.</p>
    </article></body></html>"##;
    let doc = process(html, &opts("https://example.com/page"));
    let m = doc.markdown;
    assert!(
        m.contains("![A diagram](https://cdn.example.org/blog/img/a%20b.png)"),
        "{m}"
    );
    assert!(!m.contains("data:image"));
    assert!(
        m.contains("a fake one, an anchor, and a real [paren](https://cdn.example.org/blog/post%281%29) link"),
        "{m}"
    );
}

#[test]
fn hidden_and_zero_width_are_removed() {
    let html = "<article><p>Visible\u{200b} text that is long enough to be the main content of the page.</p>\
        <p style=\"display: none\">secret</p><p hidden>also secret</p><div aria-hidden=\"true\">icon</div></article>";
    let m = md(html);
    assert_eq!(
        m,
        "Visible text that is long enough to be the main content of the page.\n"
    );
}

#[test]
fn blockquote_and_line_breaks() {
    let html = "<article><blockquote><p>First line<br>second line</p><p>Next paragraph in the quote, long enough.</p></blockquote></article>";
    let m = md(html);
    assert_eq!(
        m,
        // <br> 是硬换行（行尾两个空格）
        "> First line  \n> second line\n>\n> Next paragraph in the quote, long enough.\n"
    );
}

#[test]
fn empty_document() {
    assert_eq!(md(""), "");
    assert_eq!(
        md("<html><body><script>var a = 1;</script></body></html>"),
        ""
    );
}

#[test]
fn content_split_into_sections_is_kept_whole() {
    let para = |topic: &str| {
        format!(
            "<p>{topic} is discussed here at length, with examples, caveats, and references to earlier work.</p>"
        )
    };
    let html = format!(
        r#"<body><nav><a href="/">Home</a></nav><div class="mw-parser-output">
        <section><p>Lead paragraph introducing the topic, explaining why it matters to readers.</p></section>
        <section><h2>History <span class="mw-editsection">[<a href="/edit">edit</a>]</span></h2>{a}{b}</section>
        <section><h2>Legal issues</h2>{c}
          <section><h3>United States</h3>{d}{e}{f}{g}</section>
        </section>
        </div></body>"#,
        a = para("Early history"),
        b = para("Later history"),
        c = para("Legality"),
        d = para("A first case"),
        e = para("A second case"),
        f = para("A third case"),
        g = para("A fourth case"),
    );
    let m = md(&html);
    assert!(m.starts_with("Lead paragraph"), "{m}");
    assert!(m.contains("## History\n\nEarly history"), "{m}");
    assert!(m.contains("### United States"));
    assert!(!m.contains("edit"), "{m}");
    assert!(!m.contains("Home"));
}

#[test]
fn over_eager_skips_fall_back_to_relaxed_rules() {
    // 正文在一个类名带 sidebar 的容器里，被软规则删掉；页面其余部分是一大块导航。
    // 删完几乎没剩东西，于是放宽规则重试：正文回来了，导航仍然不要。
    let links: String = (0..60)
        .map(|i| format!("<li><a href=\"/c/{i}\">Category number {i}</a></li>"))
        .collect();
    let html = format!(
        r#"<body><nav><ul>{links}</ul></nav>
        <div class="sidebar-layout"><p>Product overview: durable neoprene sheets for gaskets, seals and padding.</p></div></body>"#
    );
    let m = md(&html);
    assert!(
        m.contains("Product overview: durable neoprene sheets"),
        "{m}"
    );
    assert!(!m.contains("Category number"), "{m}");
}

#[test]
fn aria_hidden_hides_icons_but_not_a_whole_page_behind_a_modal() {
    // 打开弹窗时网站把整个页面框架标成 aria-hidden：浏览器照样显示，正文要保留
    let body =
        "This paragraph is the real content of the page and it is long enough to count. ".repeat(4);
    let html = format!(
        "<body><div class=\"app\" aria-hidden=\"true\"><article><h1>Title</h1><p>{body}<span class=\"material-icons\" aria-hidden=\"true\">arrow_forward</span></p></article></div>\
         <div role=\"dialog\">Cookie consent</div></body>"
    );
    let m = md(&html);
    assert!(m.contains("# Title"), "{m}");
    assert!(m.contains("real content of the page"), "{m}");
    assert!(
        !m.contains("arrow_forward"),
        "icon glyph names stay out: {m}"
    );
}

#[test]
fn visibility_hidden_blocks_waiting_for_scripts_are_kept() {
    // 建站工具先用 visibility:hidden 藏起整块内容，等 JS 排版后再显示；短的工具提示仍然算隐藏
    let body = "Managed cloud services and business internet for small offices across the county. "
        .repeat(4);
    let html = format!(
        "<article><h1>Services</h1><fluid-columns-repeater style=\"visibility:hidden\"><p>{body}</p></fluid-columns-repeater>\
         <span style=\"visibility: hidden\">Copied!</span></article>"
    );
    let m = md(&html);
    assert!(m.contains("Managed cloud services"), "{m}");
    assert!(!m.contains("Copied!"), "{m}");
}

#[test]
fn link_lists_inside_main_are_content() {
    // <main> 里的链接列表（相册、分类目录）是正文；<main> 外面的同样结构还是当菜单
    let items: String = (0..6)
        .map(|i| format!("<li><a href=\"/album/{i}\">Album {i}</a></li>"))
        .collect();
    let html = format!(
        "<body><div class=\"top\"><ul>{items}</ul></div><main><h1>Gallery</h1><ul>{items}</ul>\
         <p>Browse the albums above to see photos from every trip we have taken so far.</p></main></body>"
    );
    let m = md(&html);
    assert_eq!(m.matches("[Album 3]").count(), 1, "{m}");
    assert!(m.contains("# Gallery"), "{m}");
}

#[test]
fn relaxing_keeps_menus_out_when_it_can() {
    // 正文被类名删掉、结果太短：先只放回类名删掉的块，链接菜单（没打标签）照样不要
    let menu: String = (0..8)
        .map(|i| format!("<li><a href=\"/c/{i}\">Item {i}</a></li>"))
        .collect();
    let legal = "Terms of service and privacy notice for every visitor of this site. ".repeat(2);
    let html = format!(
        r#"<body><div class="top"><ul>{menu}</ul></div>
        <div class="sidebar-layout"><p>Product overview: durable neoprene sheets for gaskets, seals and padding.</p></div>
        <div class="site-footer"><p>{legal}</p></div><div class="newsletter"><p>{legal}</p></div></body>"#
    );
    let m = md(&html);
    assert!(m.contains("Product overview"), "{m}");
    assert!(!m.contains("Item 7"), "{m}");
}

#[test]
fn class_names_mixing_frame_and_content_words_are_frame() {
    // `related-content-boxes`、`menu-content` 是框架里的部件；`main-content has-sidebar` 是正文容器
    let body = "The article body is long enough to be the main content of this page. ".repeat(4);
    let html = format!(
        r#"<body><div class="main-content has-sidebar"><h1>Title</h1><p>{body}</p></div>
        <div class="related-content-boxes"><h2>You may also like</h2><p>Another story entirely</p></div>
        <div class="menu-content"><span>API access</span><span>Status codes</span></div></body>"#
    );
    let m = md(&html);
    assert!(m.contains("# Title"), "{m}");
    assert!(!m.contains("You may also like"), "{m}");
    assert!(!m.contains("API access"), "{m}");
}

#[test]
fn hero_banners_and_comments_are_content() {
    let body = "The article body is long enough to be the main content of this page. ".repeat(4);
    let html = format!(
        r#"<body><div class="hero-banner"><h1>Spring collection</h1></div><article><p>{body}</p></article>
        <section class="comments"><p>Great write-up, the second section answered my question.</p></section></body>"#
    );
    let m = md(&html);
    assert!(m.contains("# Spring collection"), "{m}");
    assert!(m.contains("Great write-up"), "{m}");
}

#[test]
fn buttons_with_real_text_are_content() {
    // 手风琴标题这类装着成段文字的按钮是内容；"Menu""Submit"这类短控件不是
    let body = "Provision of expert advice on migration strategies and data centre operations. ";
    let html = format!(
        r#"<article><button>Menu</button><div class="accordion">
        <button aria-expanded="false">Category C - Data centre</button><div><p>{body}</p></div></div>
        <button type="submit">Submit</button></article>"#
    );
    let m = md(&html);
    assert!(m.contains("Category C - Data centre"), "{m}");
    assert!(!m.contains("Menu"), "{m}");
    assert!(!m.contains("Submit"), "{m}");
}

#[test]
fn lazy_images_use_the_real_address() {
    let html = r#"<article><p>Intro paragraph that is long enough to be the content of the page.</p>
        <img src="data:image/gif;base64,R0lGOD" data-lazy-src="/img/chart.png" alt="Sales chart">
        <img src="data:image/svg+xml;base64,PHN2Zz4=" srcset="/img/a.png 1x, /img/a@2x.png 2x" alt="Team photo">
        <img src="data:image/gif;base64,R0lGOD" alt="Placeholder only"></article>"#;
    let m = process(html, &opts("https://example.com/post")).markdown;
    assert!(
        m.contains("![Sales chart](https://example.com/img/chart.png)"),
        "{m}"
    );
    assert!(
        m.contains("![Team photo](https://example.com/img/a.png)"),
        "{m}"
    );
    assert!(!m.contains("data:"), "{m}");
    assert!(!m.contains("Placeholder only"), "{m}");
}

#[test]
fn joiners_needed_by_the_script_are_kept() {
    // 波斯语的零宽不连字（U+200C）决定字形，不能删；零宽空格可以删
    let html = "<article><p>کسب\u{200c}وکار و فناوری\u{200b} های نوظهور در این مقاله بررسی می شود و متن کافی دارد.</p></article>";
    let m = md(html);
    assert!(m.contains("کسب\u{200c}وکار"), "{m}");
    assert!(!m.contains('\u{200b}'), "{m}");
}

#[test]
fn article_parts_are_kept() {
    // <article> 里的目录（<nav>）、署名（<footer>）、标签链接是文章的一部分；外面的导航照样去掉
    let body = "The article body is long enough to be the main content of this page. ".repeat(4);
    let html = format!(
        r##"<body><nav><a href="/">Home</a> <a href="/blog">Blog</a> <a href="/about">About</a></nav>
        <article><h1>Title</h1><nav class="toc"><a href="#one">Part one</a> <a href="#two">Part two</a></nav>
        <p>{body}</p><div class="tags"><a href="/t/a">rust</a> <a href="/t/b">parsing</a> <a href="/t/c">html</a></div>
        <footer>Written by Jane Doe</footer></article></body>"##
    );
    let m = md(&html);
    assert!(m.contains("Part one"), "{m}");
    assert!(m.contains("[parsing]"), "{m}");
    assert!(m.contains("Written by Jane Doe"), "{m}");
    assert!(!m.contains("About"), "{m}");
}

#[test]
fn landmark_wrapping_the_whole_page_is_content() {
    // 整页包在 <aside> 里：它装着全页大部分文字、链接又不多，不能当侧边栏删掉
    let body = "Consulting services that help teams adopt new tools and processes. ".repeat(6);
    let html = format!(
        "<body><aside class=\"page-wrapper\"><h1>Services</h1><p>{body}</p></aside></body>"
    );
    let m = md(&html);
    assert!(m.contains("# Services"), "{m}");
    assert!(m.contains("Consulting services"), "{m}");
}

#[test]
fn thumbnail_link_grids_are_content() {
    // 链接一半以上包着图片的是相册、商品卡片，不是菜单
    let cards: String = (0..6)
        .map(|i| format!("<li><a href=\"/p/{i}\"><img src=\"/t/{i}.jpg\" alt=\"\"></a><a href=\"/p/{i}\">Lake {i}</a></li>"))
        .collect();
    let menu: String = (0..6)
        .map(|i| format!("<li><a href=\"/m/{i}\">Menu {i}</a></li>"))
        .collect();
    let html = format!(
        "<body><div class=\"top\"><ul>{menu}</ul></div><div class=\"gallery\"><h1>Gallery</h1><ul>{cards}</ul>\
         <p>Photos from every trip we have taken so far, sorted by place.</p></div></body>"
    );
    let m = md(&html);
    assert!(m.contains("[Lake 3]"), "{m}");
    assert!(!m.contains("Menu 3"), "{m}");
}

#[test]
fn index_pages_keep_their_table_of_contents() {
    // 文档目录页：正文就是 role="main" 里的一串链接。删完只剩标题时放宽规则，目录回来，顶部菜单仍然不要
    let menu: String = (0..8)
        .map(|i| format!("<li><a href=\"/m/{i}\">Menu {i}</a></li>"))
        .collect();
    let toc: String = (0..10)
        .map(|i| format!("<li><a href=\"/guide/{i}\">Setup step {i}</a></li>"))
        .collect();
    let html = format!(
        "<body><div class=\"top\"><ul>{menu}</ul></div>\
         <div role=\"main\"><h1>Deploying</h1><div class=\"toctree\"><ul>{toc}</ul></div></div></body>"
    );
    let m = md(&html);
    assert!(m.contains("[Setup step 4]"), "{m}");
    assert!(!m.contains("Menu 4"), "{m}");
}

#[test]
fn literal_markdown_characters_are_escaped_only_where_needed() {
    let html = "<article><p>Use *stars* and _under_ but a_b_c, 2 * 3 and 50% stay; &lt;tag&gt; and [x](y) too.</p>\
        <p>1. not a list</p><p># not a heading</p><p>- not an item</p>\
        <table><tr><th>n</th><th>v</th></tr><tr><td>1.</td><td>- x</td></tr></table></article>";
    let m = process(
        html,
        &Options {
            only_main_content: false,
            ..Options::default()
        },
    )
    .markdown;
    assert!(
        m.contains(
            r"Use \*stars\* and \_under\_ but a_b_c, 2 * 3 and 50% stay; \<tag> and \[x](y) too."
        ),
        "{m}"
    );
    assert!(m.contains(r"1\. not a list"), "{m}");
    assert!(m.contains(r"\# not a heading"), "{m}");
    assert!(m.contains(r"\- not an item"), "{m}");
    assert!(
        m.contains("| 1. | - x |"),
        "table cells need no block escapes: {m}"
    );
}

#[test]
fn math_becomes_tex() {
    let html = r#"<article><p>Einstein wrote <span class="katex"><span class="katex-mathml"><math><semantics><mrow><mi>E</mi></mrow><annotation encoding="application/x-tex">E=mc^2</annotation></semantics></math></span><span class="katex-html" aria-hidden="true">E=mc2</span></span> in 1905, a famous paper.</p>
        <script type="math/tex; mode=display">\int_0^1 x\,dx</script></article>"#;
    let m = process(
        html,
        &Options {
            only_main_content: false,
            ..Options::default()
        },
    )
    .markdown;
    assert!(m.contains("Einstein wrote $E=mc^2$ in 1905"), "{m}");
    assert!(m.contains("$$\n\\int_0^1 x\\,dx\n$$"), "{m}");
    assert!(!m.contains("mc2"), "the visual copy is dropped: {m}");
}

#[test]
fn raw_tex_in_text_is_left_alone() {
    let html = r#"<html><head><script src="https://cdn.example/mathjax/tex-mml-chtml.js"></script></head><body><article><p>Let \(x_i^2 + y_i\) be given and $$\sum_{i=1}^n a_i$$ holds, while $a_b$ too; prices $5 and $10 stay *escaped*.</p></article></body></html>"#;
    let m = process(
        html,
        &Options {
            only_main_content: false,
            ..Options::default()
        },
    )
    .markdown;
    assert!(m.contains(r"\(x_i^2 + y_i\)"), "{m}");
    assert!(m.contains(r"$$\sum_{i=1}^n a_i$$"), "{m}");
    assert!(m.contains(r"$a_b$"), "{m}");
    assert!(m.contains(r"\*escaped\*"), "{m}");
}

#[test]
fn arxiv_abstract_strips_labstabs_and_endorsers() {
    let html = r##"
    <main>
      <div id="content">
        <h1 class="title mathjax"><span class="descriptor">Title:</span>Attention Is All You Need</h1>
        <div class="authors"><span class="descriptor">Authors:</span>Ashish Vaswani, Noam Shazeer</div>
        <blockquote class="abstract mathjax"><span class="descriptor">Abstract:</span>The dominant sequence transduction models...</blockquote>
        <div class="bookmarks"><h3>Bookmark</h3><a href="#">Reddit</a></div>
        <div id="labstabs">
          <div class="labstabs" role="tablist">
            <div id="tabpanel-one" class="tab" role="tabpanel">
              <h1>Bibliographic and Citation Tools</h1>
              <div class="lab-row"><span>CORE Recommender</span></div>
            </div>
            <div id="tabpanel-five" class="tab" role="tabpanel">
              <h1>arXivLabs: experimental projects with community collaborators</h1>
              <p>arXivLabs is a framework that allows collaborators to develop...</p>
            </div>
          </div>
        </div>
        <div class="endorsers">
          <a href="#">Which authors of this paper are endorsers?</a>
        </div>
      </div>
    </main>
    "##;
    let doc = process(html, &Options::default());
    let md = doc.markdown;
    assert!(md.contains("Attention Is All You Need"), "keeps paper title: {md}");
    assert!(md.contains("Abstract:"), "keeps abstract: {md}");
    assert!(!md.contains("arXivLabs"), "strips arXivLabs boilerplate: {md}");
    assert!(!md.contains("CORE Recommender"), "strips CORE recommender: {md}");
    assert!(!md.contains("Which authors of this paper are endorsers"), "strips endorsers: {md}");
    assert!(!md.contains("Bookmark"), "strips bookmarks: {md}");
}
