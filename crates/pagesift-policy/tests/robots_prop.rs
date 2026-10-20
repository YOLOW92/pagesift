//! 不 panic 测试：任意输入都能解析和匹配。

use pagesift_policy::Robots;
use proptest::prelude::*;

fn line() -> impl Strategy<Value = String> {
    let key = prop_oneof![
        Just("user-agent"),
        Just("allow"),
        Just("disallow"),
        Just("crawl-delay"),
        Just("sitemap"),
        Just("junk"),
    ];
    (key, "[ -~\\*\\$%\u{80}-\u{10ffff}]{0,24}").prop_map(|(k, v)| format!("{k}: {v}"))
}

proptest! {
    #![proptest_config(ProptestConfig::with_cases(2000))]

    #[test]
    fn arbitrary_text_never_panics(text in "\\PC{0,512}", path in "\\PC{0,64}", ua in "\\PC{0,16}") {
        let r = Robots::parse(&text);
        let _ = r.is_allowed(&ua, &path);
        let _ = r.crawl_delay(&ua);
    }

    #[test]
    fn structured_robots_never_panics(lines in prop::collection::vec(line(), 0..40), path in "/[ -~%\\*\\$]{0,48}") {
        let r = Robots::parse(&lines.join("\n"));
        for ua in ["pagesift", "*", ""] {
            let _ = r.is_allowed(ua, &path);
        }
        // /robots.txt 永远允许。
        prop_assert!(r.is_allowed("pagesift", "/robots.txt"));
    }

    #[test]
    fn bytes_never_panic(bytes in prop::collection::vec(any::<u8>(), 0..1024)) {
        let text = String::from_utf8_lossy(&bytes);
        let r = Robots::parse(&text);
        let _ = r.is_allowed("pagesift", &text);
    }
}

#[test]
fn pathological_wildcards_are_fast() {
    let pattern = format!("user-agent: *\ndisallow: /{}b\n", "*a".repeat(200));
    let r = Robots::parse(&pattern);
    let path = format!("/{}", "a".repeat(5000));
    let t = std::time::Instant::now();
    assert!(r.is_allowed("pagesift", &path));
    assert!(
        t.elapsed() < std::time::Duration::from_secs(2),
        "{:?}",
        t.elapsed()
    );
}
