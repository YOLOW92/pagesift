use pagesift_html::{Element, NodeRef, Tag};

pub(crate) type NodeR<'a> = NodeRef<'a>;

pub(crate) fn element<'a>(node: &NodeR<'a>) -> Option<Element<'a>> {
    node.as_element()
}

pub(crate) fn tag<'a>(node: &NodeR<'a>) -> Option<&'a str> {
    element(node).map(|e| e.name())
}

/// 永远不产出正文的元素。
pub(crate) fn is_non_content_tag(tag: Tag) -> bool {
    use Tag::*;
    matches!(
        tag,
        Script
            | Style
            | Noscript
            | Template
            | Head
            | Title
            | Meta
            | Link
            | Iframe
            | Svg
            | Canvas
            | Object
            | Embed
            | Select
            | Option
            | Input
            | Textarea
            | Button
            | Dialog
            | Math
    )
}

/// 显式隐藏的元素：`hidden`、内联样式 `display:none`，以及文字很少的 `visibility:hidden`。
/// `aria-hidden="true"` 只让读屏软件跳过，浏览器照样显示，所以只对文字很少的元素生效
/// （图标字体如 `arrow_forward`、装饰性短标签）；网站打开弹窗时常把整个页面框架标成
/// aria-hidden，那种大块内容保留，否则整页正文会被删光。
pub(crate) fn is_hidden(node: &NodeR<'_>) -> bool {
    let Some(el) = element(node) else {
        return false;
    };
    // 每个元素都要查：一次遍历属性，不按名字查三遍
    let (mut aria, mut style, mut cloaked) = (None, None, false);
    for (name, value) in el.attrs() {
        match name {
            "hidden" => return true,
            "aria-hidden" => {
                aria.get_or_insert(value);
            }
            "style" => {
                style.get_or_insert(value);
            }
            // Alpine / Vue / Angular 在水合前用这些属性配合 CSS 隐藏模板；HTTP 抽取拿到的是
            // 水合前的 HTML，里面的错误模板、未编译占位不该进正文。
            "x-cloak" | "data-cloak" | "v-cloak" | "ng-cloak" => cloaked = true,
            _ => {}
        }
    }
    if cloaked && !is_root_element(el.name()) && !contains_landmark(node) && !is_app_root(node) {
        return true;
    }
    if aria.is_some_and(|v| v.eq_ignore_ascii_case("true"))
        && short_text(node, ARIA_HIDDEN_MAX_CHARS)
    {
        return true;
    }
    if let Some(style) = style {
        if style_contains(style, b"display:none") {
            return true;
        }
        // visibility:hidden 常用来在 JS 排好版之前先藏起整块内容（Wix 等建站工具），
        // JS 执行后就可见；只对短内容（工具提示、占位标签）当作隐藏
        if style_contains(style, b"visibility:hidden") && short_text(node, ARIA_HIDDEN_MAX_CHARS) {
            return true;
        }
    }
    false
}

/// `<body>` 下直接挂着、带 cloak 且文字很多的容器是水合前的应用根（`<div id="app" v-cloak>`），
/// 服务端已经把整页渲染在里面。弹窗、错误模板这类 cloak 内容很少超过这个字数。
const CLOAK_APP_ROOT_MIN_CHARS: usize = 1000;

fn is_app_root(node: &NodeR<'_>) -> bool {
    node.parent()
        .and_then(|p| element(&p).map(|el| el.name().eq_ignore_ascii_case("body")))
        .unwrap_or(false)
        && !short_text(node, CLOAK_APP_ROOT_MIN_CHARS)
}

/// 节点在 `<template>` 里（含自身）。模板内容是惰性的，浏览器不渲染；选正文根时要排除。
pub(crate) fn in_template(node: &NodeR<'_>) -> bool {
    node.tag() == Some(pagesift_html::Tag::Template)
        || node
            .ancestors()
            .any(|a| a.tag() == Some(pagesift_html::Tag::Template))
}

fn is_root_element(name: &str) -> bool {
    name.eq_ignore_ascii_case("html") || name.eq_ignore_ascii_case("body")
}

/// 水合前的应用根节点常带 `v-cloak` / `x-cloak`，但里面就是真实页面。只要子树里有
/// `main` / `article` / `role=main`，就按页面主体保留，避免把整页正文当模板删掉。
fn contains_landmark(node: &NodeR<'_>) -> bool {
    let is_landmark = |n: &NodeR<'_>| {
        element(n).is_some_and(|el| {
            matches!(el.name(), "main" | "article")
                || el
                    .attrs()
                    .any(|(name, value)| name == "role" && value.eq_ignore_ascii_case("main"))
        })
    };
    is_landmark(node) || node.descendants().any(|d| is_landmark(&d))
}

/// 内联样式里有没有 `needle`（小写、不含空白）：比较时忽略空白和 ASCII 大小写，不分配内存。
/// 每个带 style 的元素都要查，这里是热路径。
fn style_contains(style: &str, needle: &[u8]) -> bool {
    let bytes = style.as_bytes();
    let ws = |b: u8| b.is_ascii_whitespace() || b == b'\x0b';
    (0..bytes.len()).any(|start| {
        let mut i = start;
        for &want in needle {
            while i < bytes.len() && ws(bytes[i]) && i > start {
                i += 1;
            }
            if i >= bytes.len() || bytes[i].to_ascii_lowercase() != want {
                return false;
            }
            i += 1;
        }
        true
    })
}

/// aria-hidden、visibility:hidden 的元素文字超过这么多（非空白字符）就当作页面内容，不跳过。
const ARIA_HIDDEN_MAX_CHARS: usize = 200;

/// 子树里的非空白字符数不超过 `max`（数到超过就停）。
pub(crate) fn short_text(node: &NodeR<'_>, max: usize) -> bool {
    let mut n = 0;
    for d in node.descendants() {
        if let Some(t) = d.as_text() {
            n += count_non_whitespace(t);
            if n > max {
                return false;
            }
        }
    }
    true
}

/// 去掉的零宽字符：零宽空格、词连接符、BOM。零宽不连字（U+200C）和零宽连字（U+200D）要保留：
/// 波斯语、印地语等文字靠它们决定字形（کسب‌وکار），emoji 组合序列也靠 U+200D。
pub(crate) fn is_zero_width(c: char) -> bool {
    matches!(c, '\u{200b}' | '\u{2060}' | '\u{feff}')
}

/// 非空白字符数。纯 ASCII 文本（绝大多数）按字节数，不逐个解码字符。
pub(crate) fn count_non_whitespace(s: &str) -> usize {
    // 一遍按字节查表：ASCII 空白和 UTF-8 续字节不计，其余字节各是一个非空白字符的开头；
    // 可能是 Unicode 空白（U+0085、U+00A0、U+1680、U+2000–U+205F、U+3000）的首字节才解码判断。
    // 和 `s.chars().filter(|c| !c.is_whitespace()).count()` 结果相同，测试里穷举了所有字符。
    const CLASS: [u8; 256] = {
        let mut t = [0u8; 256];
        let mut i = 0x80;
        while i < 0xC0 {
            t[i] = 1;
            i += 1;
        }
        t[b' ' as usize] = 1;
        t[b'\t' as usize] = 1;
        t[b'\n' as usize] = 1;
        t[0x0b] = 1;
        t[0x0c] = 1;
        t[b'\r' as usize] = 1;
        t[0xC2] = 2;
        t[0xE1] = 2;
        t[0xE2] = 2;
        t[0xE3] = 2;
        t
    };
    let b = s.as_bytes();
    let mut n = 0;
    for (i, &x) in b.iter().enumerate() {
        match CLASS[x as usize] {
            0 => n += 1,
            1 => {}
            _ => n += usize::from(!s[i..].chars().next().is_some_and(char::is_whitespace)),
        }
    }
    n
}

/// 把空白折叠成单个空格，去掉零宽字符，`&nbsp;` 视为普通空格。
pub(crate) fn collapse_whitespace(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    collapse_whitespace_into(s, &mut out);
    out
}

pub(crate) fn collapse_whitespace_into(s: &str, out: &mut String) {
    out.reserve(s.len());
    if s.is_ascii() {
        // 纯 ASCII：没有零宽字符，非空白的连续片段整段复制
        let bytes = s.as_bytes();
        let ws = |b: u8| matches!(b, b' ' | b'\t' | b'\n' | b'\x0b' | b'\x0c' | b'\r');
        let mut i = 0;
        while i < bytes.len() {
            if ws(bytes[i]) {
                out.push(' ');
                while i < bytes.len() && ws(bytes[i]) {
                    i += 1;
                }
            } else {
                let start = i;
                while i < bytes.len() && !ws(bytes[i]) {
                    i += 1;
                }
                out.push_str(&s[start..i]);
            }
        }
        return;
    }
    let mut in_space = false;
    for c in s.chars() {
        if is_zero_width(c) {
            continue;
        }
        if c.is_whitespace() {
            if !in_space {
                out.push(' ');
                in_space = true;
            }
        } else {
            out.push(c);
            in_space = false;
        }
    }
}

/// 子树里所有文本，原样拼接（用于 `<pre>` 等需要保留空白的场合）。
pub(crate) fn raw_text(node: &NodeR<'_>) -> String {
    let mut out = String::new();
    for d in node.descendants() {
        if let Some(t) = d.as_text() {
            out.extend(t.chars().filter(|c| !is_zero_width(*c)));
        } else if d.tag() == Some(Tag::Br) {
            out.push('\n');
        }
    }
    out
}

/// Headings emitted as generic containers by older CMS templates.
pub(crate) fn is_heading(node: &NodeR<'_>) -> bool {
    node.as_element().is_some_and(|e| {
        e.attr("role") == Some("heading")
            || (e.tag() == Tag::Div
                && e.attr("class").is_some_and(|s| {
                    s.split_ascii_whitespace().any(|c| {
                        c.split(['-', '_'])
                            .any(|word| matches!(word, "subheadline" | "subheading"))
                    })
                }))
    })
}

#[cfg(test)]
mod tests {
    #[test]
    fn count_non_whitespace_matches_char_is_whitespace() {
        let reference = |s: &str| s.chars().filter(|c| !c.is_whitespace()).count();
        // 每个字符单独、夹在 ASCII 和空白之间
        for c in (0..=0x10FFFF).filter_map(char::from_u32) {
            for s in [c.to_string(), format!("a{c} b\u{a0}{c}\n")] {
                assert_eq!(super::count_non_whitespace(&s), reference(&s), "{:?}", c);
            }
        }
    }
}
