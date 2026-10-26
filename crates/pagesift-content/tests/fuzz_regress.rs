//! stable 模糊测试（`pagesift-fuzz markdown`，见 scripts/fuzz.sh）找到的用例。

use std::time::{Duration, Instant};

fn render(html: &str) -> (String, Duration) {
    let t = Instant::now();
    let doc = pagesift_content::process(html, &pagesift_content::Options::default());
    (doc.markdown, t.elapsed())
}

/// 文本转义时每个 `[` 都往后找 `]`：大量不配对的 `[`（比如 HTML 里当文本出现的
/// `<![CDATA[`）曾经让 70 万字节的页面渲染 19 秒。TeX 片段的开头找不到收尾时同理。
#[test]
fn unmatched_brackets_and_tex_openers_are_linear() {
    for unit in ["[", "x[CDATA[ ", "\\[ ", "\\( ", "$$ "] {
        let html = format!(
            "<article><p>{}</p></article>",
            unit.repeat(200_000 / unit.len())
        );
        let (md, took) = render(&html);
        assert!(!md.is_empty(), "{unit}");
        assert!(took < Duration::from_secs(2), "{unit}: {took:?}");
    }
}

/// 转义规则本身不变：成对的才转义，公式原样保留。
#[test]
fn escaping_unchanged() {
    let (md, _) =
        render("<article><p>see [a](b) and [c] then [d][e], $x_1 + y_2$ and a_b_c</p></article>");
    assert!(md.contains("\\[a](b)"), "{md}");
    assert!(md.contains("[c] then"), "{md}");
    assert!(md.contains("\\[d][e]"), "{md}");
    assert!(md.contains("$x_1 + y_2$"), "{md}");
    assert!(md.contains("a_b_c"), "{md}");
}
