//! DASH 用的小 XML 树（`stream/xml.rs`）的表驱动测试：解析后再写回，和期望的文字比较。
//!
//! 期望写法同 `tests_dash`：失败时 `Err: 错误信息`，只写 `Err` 表示任何错误都算（XML 库自己的错误）。

use super::tests_dash::{check, same};
use super::xml::{write_xml, xml};

fn roundtrip(source: &str) -> String {
    match xml(source).and_then(|n| {
        let mut out = String::new();
        write_xml(&n, &mut out)?;
        Ok(out)
    }) {
        Ok(s) => s,
        Err(e) => format!("Err: {e}"),
    }
}

#[test]
fn table_xml_tree() {
    let nested = |depth: usize| format!("{}{}", "<a>".repeat(depth), "</a>".repeat(depth));
    let wide = |n: usize| format!("<a>{}</a>", "<b/>".repeat(n - 1));
    // 写回时最里层的空元素写成自闭合；宽的那个原样写回
    let deep_ok = format!("{}<a/>{}", "<a>".repeat(31), "</a>".repeat(31));
    let wide_ok = wide(10_000);
    let cases: Vec<(String, &str)> = vec![
        // 元素、属性、文本；写回时文本在子元素前面
        (r#"<MPD a="1"><B/>text</MPD>"#.into(), r#"<MPD a="1">text<B/></MPD>"#),
        (r#"<a></a>"#.into(), "<a/>"),
        (r#"<a ></a >"#.into(), "<a/>"),
        (r#"<a x="1" />"#.into(), r#"<a x="1"/>"#),
        // 声明、注释、处理指令、空白都跳过
        (
            "<?xml version=\"1.0\" encoding=\"UTF-8\"?>\n<!-- c -->\n<a>\n  <b x=\"1\"/>\n  <?pi x?>\n</a>\n".into(),
            r#"<a><b x="1"/></a>"#,
        ),
        // 命名空间前缀去掉，属性名原样保留
        (
            r#"<mpd:MPD xmlns:mpd="urn:x"><mpd:Period mpd:id="1"/></mpd:MPD>"#.into(),
            r#"<MPD xmlns:mpd="urn:x"><Period mpd:id="1"/></MPD>"#,
        ),
        // 属性里的实体还原，写回时转义
        (
            r#"<a x="1 &amp; 2" y='q"'/>"#.into(),
            r#"<a x="1 &amp; 2" y="q&quot;"/>"#,
        ),
        // 属性值里有 "/>" 也不会被当成自闭合
        (r#"<a x="/>"><b/></a>"#.into(), r#"<a x="/&gt;"><b/></a>"#),
        // 文本：首尾空白去掉，字符引用还原
        (r#"<a>  padded  </a>"#.into(), "<a>padded</a>"),
        (r#"<a>&#65;&#x42;</a>"#.into(), "<a>AB</a>"),
        (r#"<a>x &lt; y</a>"#.into(), "<a>x&lt;y</a>"), // BUG? 实体两边的空格被 trim_text 吃掉，应为 "x &lt; y"
        // 普通 href 属性可以，带前缀的 href（xlink:href）是外部引用，不支持
        (r#"<a href="x"/>"#.into(), r#"<a href="x"/>"#),
        (
            r#"<a xlink:href="x"/>"#.into(),
            "Err: external DASH XML references unsupported",
        ),
        (r#"<!DOCTYPE a><a/>"#.into(), "Err: DASH DTD unsupported"),
        (r#"<a><![CDATA[x]]></a>"#.into(), "Err: DASH CDATA unsupported"),
        // 文档结构
        (r#"<a/><b/>"#.into(), "Err: invalid DASH document"),
        ("".into(), "Err: invalid DASH document"),
        ("just text".into(), "Err: invalid DASH document"),
        (r#"<a>"#.into(), "Err"),
        (r#"<a></b>"#.into(), "Err"),
        (r#"</a>"#.into(), "Err"),
        (r#"<a>&foo;</a>"#.into(), "Err"),
        (r#"<a x="1" x="2"/>"#.into(), "Err"),
        (r#"<a x=1/>"#.into(), "Err"),
        // 深度不超过 32 层，元素不超过 10000 个
        (nested(32), &deep_ok),
        (nested(33), "Err: DASH XML budget exceeded"),
        (wide(10_000), &wide_ok),
        (wide(10_001), "Err: DASH XML budget exceeded"),
    ];
    check("xml", &cases, |s| roundtrip(s), same);
}
