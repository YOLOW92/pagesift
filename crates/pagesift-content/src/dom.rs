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

/// 显式隐藏的元素：`hidden`、`aria-hidden="true"`、内联样式 `display:none` / `visibility:hidden`。
pub(crate) fn is_hidden(el: Element<'_>) -> bool {
    if el.attr("hidden").is_some() {
        return true;
    }
    if el
        .attr("aria-hidden")
        .is_some_and(|v| v.eq_ignore_ascii_case("true"))
    {
        return true;
    }
    if let Some(style) = el.attr("style") {
        let style: String = style
            .chars()
            .filter(|c| !c.is_whitespace())
            .collect::<String>()
            .to_ascii_lowercase();
        if style.contains("display:none") || style.contains("visibility:hidden") {
            return true;
        }
    }
    false
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
    out
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
