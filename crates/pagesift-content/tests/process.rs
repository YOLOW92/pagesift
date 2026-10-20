use pagesift_content::{Options, process};
use url::Url;

fn opts(base: &str) -> Options {
    Options {
        base_url: Some(Url::parse(base).unwrap()),
        only_main_content: true,
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
        "> First line\n> second line\n>\n> Next paragraph in the quote, long enough.\n"
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
