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
fn short_text(node: &NodeR<'_>, max: usize) -> bool {
    let mut n = 0;
    for d in node.descendants() {
        if let Some(t) = d.as_text() {
            n += t.chars().filter(|c| !c.is_whitespace()).count();
            if n > max {
                return false;
            }
        }
    }
    true
}

pub(crate) fn is_zero_width(c: char) -> bool {
    matches!(
        c,
        '\u{200b}' | '\u{200c}' | '\u{200d}' | '\u{2060}' | '\u{feff}'
    )
}

/// 把空白折叠成单个空格，去掉零宽字符，`&nbsp;` 视为普通空格。
pub(crate) fn collapse_whitespace(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    collapse_whitespace_into(s, &mut out);
    out
}

pub(crate) fn collapse_whitespace_into(s: &str, out: &mut String) {
    out.reserve(s.len());
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
