use super::*;

fn u(s: &str) -> Url {
    Url::parse(s).unwrap()
}

#[test]
fn same_host_http_links_upgrade_to_https() {
    let learner = Learner::default();
    let base = Url::parse("https://www.e.edu/study/").unwrap();
    let same = candidate(&base, "http://www.e.edu/fees/", &learner).unwrap();
    assert_eq!(same.as_str(), "https://www.e.edu/fees/");
    // 同一站点的子域名也升级；别的站点不知道有没有 https：照原样
    let sub = candidate(&base, "http://grad.e.edu/", &learner).unwrap();
    assert_eq!(sub.scheme(), "https");
    let other = candidate(&base, "http://other.org/", &learner).unwrap();
    assert_eq!(other.scheme(), "http");
    // 种子本来就是 http：不动
    let http_base = Url::parse("http://www.e.edu/").unwrap();
    let kept = candidate(&http_base, "http://www.e.edu/fees/", &learner).unwrap();
    assert_eq!(kept.scheme(), "http");
}

#[test]
fn normalizes_for_dedup() {
    assert_eq!(
        normalize(&u("HTTPS://Example.COM:443/a/../b?z=1&a=2#frag"), false),
        "https://example.com/b?a=2&z=1"
    );
    assert_eq!(
        normalize(&u("https://example.com/b?z=1"), true),
        "https://example.com/b"
    );
}

#[test]
fn www_variants_dedup_to_one_key() {
    assert_eq!(
        dedup_key(&u("https://www.example.com/a"), false),
        dedup_key(&u("https://example.com/a"), false)
    );
    assert_ne!(
        dedup_key(&u("https://wwwexample.com/a"), false),
        dedup_key(&u("https://example.com/a"), false)
    );
}

#[test]
fn scope_with_file_ending_seed_uses_its_directory() {
    let file = u("https://example.com/travel_2/index.html");
    let s = Scope::new(&file, false, false, CrawlScope::Auto, &[], &[]).unwrap();
    assert!(s.allows(&u("https://example.com/travel_2/page-2.html")));
    assert!(s.allows(&u("https://example.com/travel_2/")));
    assert!(s.allows(&u("https://example.com/travel_2")));
    assert!(!s.allows(&u("https://example.com/travel_3/page.html")));
    assert!(!s.allows(&u("https://example.com/")));

    // 嵌套文件名种子
    let deep = u("https://example.com/catalogue/books/travel_2/index.html");
    let s = Scope::new(&deep, false, false, CrawlScope::Auto, &[], &[]).unwrap();
    assert!(s.allows(&u("https://example.com/catalogue/books/travel_2/a.html")));
    assert!(!s.allows(&u("https://example.com/catalogue/books/other/a.html")));

    // 单段路径保持原范围，不放大到整站
    let section = u("https://example.com/docs");
    let s = Scope::new(&section, false, false, CrawlScope::Auto, &[], &[]).unwrap();
    assert!(s.allows(&u("https://example.com/docs/intro")));
    assert!(!s.allows(&u("https://example.com/blog/x")));

    // 不像文件名的多段路径按自身前缀，不放大到上级目录
    let lang = u("https://example.com/docs/python");
    let s = Scope::new(&lang, false, false, CrawlScope::Auto, &[], &[]).unwrap();
    assert!(s.allows(&u("https://example.com/docs/python/tutorial")));
    assert!(s.allows(&u("https://example.com/docs/python")));
    assert!(!s.allows(&u("https://example.com/docs/java")));
    assert!(!s.allows(&u("https://example.com/docs/")));

    // 根路径仍是整站
    let root = u("https://example.com/");
    let s = Scope::new(&root, false, false, CrawlScope::Auto, &[], &[]).unwrap();
    assert!(s.allows(&u("https://example.com/blog/x")));

    // 根目录下的文件名种子也按整站处理
    let root_file = u("https://example.com/index.html");
    let s = Scope::new(&root_file, false, false, CrawlScope::Auto, &[], &[]).unwrap();
    assert!(s.allows(&u("https://example.com/a.html")));
}

#[test]
fn auto_scope_widens_from_leaf_seed_only() {
    let urls = |ps: &[&str]| -> Vec<Url> {
        ps.iter()
            .map(|p| u(&format!("https://example.com{p}")))
            .collect()
    };
    // 博客文章：没有子页，链向同栏目的其他文章 → 放宽到 /blog
    let post = u("https://example.com/blog/post-3");
    let s = Scope::new(&post, false, false, CrawlScope::Auto, &[], &[]).unwrap();
    assert!(!s.allows(&u("https://example.com/blog/post-4")));
    assert!(s.may_widen_to(&u("https://example.com/blog/post-4")));
    let links = urls(&[
        "/blog/",
        "/blog/post-1",
        "/blog/post-2",
        "/blog/post-4",
        "/about",
    ]);
    let prose = "word ".repeat(60);
    let w = s.widen_from_seed(&post, &prose, &links).expect("widened");
    assert!(w.starts_with("crawl_scope:"), "{w}");
    assert!(s.allows(&u("https://example.com/blog/post-4")));
    assert!(
        !s.allows(&u("https://example.com/about")),
        "never widened to the root"
    );
    assert!(!s.may_widen_to(&u("https://example.com/blog/post-5")));

    // 目录页：有自己的子页 → 不放宽，同级栏目照样挡住
    let lang = u("https://example.com/docs/python");
    let s = Scope::new(&lang, false, false, CrawlScope::Auto, &[], &[]).unwrap();
    let links = urls(&[
        "/docs/python/tutorial",
        "/docs/python/api",
        "/docs/python/faq",
        "/docs/java",
        "/docs/go",
        "/docs/rust",
    ]);
    assert!(s.widen_from_seed(&lang, &prose, &links).is_none());
    assert!(!s.allows(&u("https://example.com/docs/java")));

    // 同级链接太少、只链到上级目录首页 → 不放宽
    let s = Scope::new(&post, false, false, CrawlScope::Auto, &[], &[]).unwrap();
    let links = urls(&["/blog/", "/blog/index.html", "/blog/post-1", "/blog/post-3"]);
    assert!(s.widen_from_seed(&post, &prose, &links).is_none());

    // 只列链接、没有正文的页面（空分类页）不放宽
    let s = Scope::new(&post, false, false, CrawlScope::Auto, &[], &[]).unwrap();
    let links = urls(&[
        "/blog/post-1",
        "/blog/post-2",
        "/blog/post-4",
        "/blog/post-5",
    ]);
    let only_links: String = links
        .iter()
        .map(|l| format!("- [{l} long anchor text]({l})\n"))
        .collect();
    assert!(s.widen_from_seed(&post, &only_links, &links).is_none());
    assert!(prose_chars(&only_links) == 0);
    assert_eq!(prose_chars("中文正文 [链接](/x) ![图](/y.png) abc"), 7);

    // 按日期分目录的博客：同一天没有别的文章，放宽到最近的有同级文章的那一层
    let dated = u("https://example.com/blog/2026/07/21/nice-things/");
    let s = Scope::new(&dated, false, false, CrawlScope::Auto, &[], &[]).unwrap();
    let links = urls(&[
        "/blog/",
        "/blog/2026/07/17/sqlite/",
        "/blog/2026/09/27/lights/",
        "/blog/2025/12/01/year/",
        "/about",
    ]);
    let w = s.widen_from_seed(&dated, &prose, &links).expect("widened");
    assert!(w.contains("to /blog/"), "{w}");
    assert!(s.allows(&u("https://example.com/blog/2024/01/01/old/")));
    assert!(!s.allows(&u("https://example.com/about")));
    // 同一年就有 3 篇时停在那一年
    let s = Scope::new(&dated, false, false, CrawlScope::Auto, &[], &[]).unwrap();
    let links = urls(&[
        "/blog/2026/07/17/a/",
        "/blog/2026/09/27/b/",
        "/blog/2026/01/02/c/",
    ]);
    assert!(
        s.widen_from_seed(&dated, &prose, &links)
            .expect("widened")
            .contains("to /blog/2026/")
    );
    // 结尾带 / 的目录页有自己的子页 → 不放宽
    let dir = u("https://example.com/docs/concepts/workloads/");
    let s = Scope::new(&dir, false, false, CrawlScope::Auto, &[], &[]).unwrap();
    let links = urls(&[
        "/docs/concepts/workloads/pods/",
        "/docs/concepts/workloads/jobs/",
        "/docs/concepts/workloads/cron/",
        "/docs/concepts/services/",
        "/docs/concepts/storage/",
        "/docs/concepts/security/",
    ]);
    assert!(s.widen_from_seed(&dir, &prose, &links).is_none());

    // 根下单段页面、目录种子、文件名种子都不放宽
    for seed in ["/about", "/docs/", "/docs/page.html"] {
        let seed = u(&format!("https://example.com{seed}"));
        let s = Scope::new(&seed, false, false, CrawlScope::Auto, &[], &[]).unwrap();
        let links = urls(&["/a", "/b", "/c", "/docs/a", "/docs/b", "/docs/c"]);
        assert!(s.widen_from_seed(&seed, &prose, &links).is_none(), "{seed}");
    }

    // exact 从不放宽；parent 一开始就是上一级；domain 是整站
    let s = Scope::new(&post, false, false, CrawlScope::Exact, &[], &[]).unwrap();
    let links = urls(&["/blog/post-1", "/blog/post-2", "/blog/post-4"]);
    assert!(s.widen_from_seed(&post, &prose, &links).is_none());
    let s = Scope::new(&post, false, false, CrawlScope::Parent, &[], &[]).unwrap();
    assert!(s.allows(&u("https://example.com/blog/post-4")));
    assert!(!s.allows(&u("https://example.com/about")));
    // parent 跳过日期目录取所在栏目；栏目是整站根时爬整个主机
    let s = Scope::new(&dated, false, false, CrawlScope::Parent, &[], &[]).unwrap();
    assert!(s.allows(&u("https://example.com/blog/2025/01/02/x/")));
    assert!(!s.allows(&u("https://example.com/about")));
    let root_dated = u("https://example.com/2024/Dec/31/llms/");
    let s = Scope::new(&root_dated, false, false, CrawlScope::Parent, &[], &[]).unwrap();
    assert!(s.allows(&u("https://example.com/2023/Jan/02/other/")));
    assert!(s.allows(&u("https://example.com/about")));
    let s = Scope::new(&post, false, false, CrawlScope::Domain, &[], &[]).unwrap();
    assert!(s.allows(&u("https://example.com/about")));

    // 从断点继续：入过队的 URL 在上一级范围内 → 恢复放宽
    let s = Scope::new(&post, false, false, CrawlScope::Auto, &[], &[]).unwrap();
    s.restore_widened(&urls(&["/blog/post-3", "/blog/post-7"]));
    assert!(s.allows(&u("https://example.com/blog/post-9")));
}

#[test]
fn scope_rules() {
    let start = u("https://www.example.com/docs/");
    let s = Scope::new(
        &start,
        false,
        false,
        CrawlScope::Auto,
        &[],
        &["/docs/private".into()],
    )
    .unwrap();
    assert!(s.allows(&u("https://example.com/docs/intro")));
    assert!(s.allows(&u("https://www.example.com/docs")));
    assert!(
        !s.allows(&u("https://example.com/blog/x")),
        "outside the start path"
    );
    assert!(!s.allows(&u("https://api.example.com/docs/x")), "subdomain");
    assert!(!s.allows(&u("https://other.com/docs/x")), "external");
    assert!(
        !s.allows(&u("https://example.com/docs/private/x")),
        "excluded"
    );
    assert!(!s.allows(&u("mailto:a@example.com")));
    let wide = Scope::new(
        &start,
        true,
        false,
        CrawlScope::Domain,
        &["^/docs/".into()],
        &[],
    )
    .unwrap();
    assert!(wide.allows(&u("https://api.example.com/docs/x")));
    assert!(
        !wide.allows(&u("https://example.com/blog/x")),
        "include pattern"
    );
    assert!(Scope::new(&start, false, false, CrawlScope::Auto, &["(".into()], &[]).is_err());
}

#[test]
fn gunzips_sitemaps() {
    use std::io::Write;
    let mut enc = flate2::write::GzEncoder::new(Vec::new(), flate2::Compression::default());
    enc.write_all(b"<urlset><url><loc>https://example.com/z</loc></url></urlset>")
        .unwrap();
    let xml = gunzip_if_needed(enc.finish().unwrap()).unwrap();
    assert_eq!(parse_sitemap(&xml).0, vec!["https://example.com/z"]);
}

#[test]
fn the_seed_page_is_retried_longer_than_other_pages() {
    let timeout: Result<pagesift_api::types::Document, ApiError> =
        Err(ApiError::new(ErrorCode::Timeout, "first byte timeout"));
    let url = Url::parse("https://example.com/").unwrap();
    // 普通页面：两次重试，0.5 秒、1 秒
    let mut r = Retries::default();
    assert!(r.offer(&timeout, &url, 1, 0, false));
    assert!(r.offer(&timeout, &url, 1, 1, false));
    assert!(!r.offer(&timeout, &url, 1, 2, false));
    assert_eq!(
        retry_wait(&timeout, 0, RETRY_BASE),
        Some(Duration::from_millis(500))
    );
    // 种子页：四次重试，2、4、8、16 秒
    let mut s = Retries::default();
    for attempt in 0..4 {
        assert!(s.offer(&timeout, &url, 0, attempt, true), "{attempt}");
        assert_eq!(
            retry_wait(&timeout, attempt, SEED_RETRY_BASE),
            Some(Duration::from_secs(2 << attempt))
        );
    }
    assert!(!s.offer(&timeout, &url, 0, 4, true));
}
