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
    if el.attr("hidden").is_some() {
        return true;
    }
    if el
        .attr("aria-hidden")
        .is_some_and(|v| v.eq_ignore_ascii_case("true"))
        && short_text(node, ARIA_HIDDEN_MAX_CHARS)
    {
        return true;
    }
    if let Some(style) = el.attr("style") {
        let style: String = style
            .chars()
            .filter(|c| !c.is_whitespace())
            .collect::<String>()
            .to_ascii_lowercase();
        if style.contains("display:none") {
            return true;
        }
        // visibility:hidden 常用来在 JS 排好版之前先藏起整块内容（Wix 等建站工具），
        // JS 执行后就可见；只对短内容（工具提示、占位标签）当作隐藏
        if style.contains("visibility:hidden") && short_text(node, ARIA_HIDDEN_MAX_CHARS) {
            return true;
        }
    }
    false
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
    if s.is_ascii() {
        // char::is_whitespace 在 ASCII 范围内正好是这几个（含 \x0b，is_ascii_whitespace 不含它）
        return s
            .bytes()
            .filter(|b| !matches!(b, b' ' | b'\t' | b'\n' | b'\x0b' | b'\x0c' | b'\r'))
            .count();
    }
    s.chars().filter(|c| !c.is_whitespace()).count()
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
