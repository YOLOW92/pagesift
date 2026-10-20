use pagesift_html::{NodeRef, Tag};

/// 元素的可见文本：拼接后代文本节点，空白折叠成单个空格并去掉首尾空白。
pub(crate) fn node_text(node: NodeRef<'_>) -> String {
    let mut out = String::new();
    let mut pending_space = false;
    let mut push = |s: &str, out: &mut String| {
        for ch in s.chars() {
            if ch.is_whitespace() {
                pending_space = true;
            } else {
                if pending_space && !out.is_empty() {
                    out.push(' ');
                }
                pending_space = false;
                out.push(ch);
            }
        }
    };
    if let Some(t) = node.as_text().or_else(|| node.as_comment()) {
        push(t, &mut out);
        return out;
    }
    for d in node.descendants() {
        if let Some(t) = d.as_text()
            && !in_hidden_parent(d)
        {
            push(t, &mut out);
        }
    }
    out
}

fn in_hidden_parent(n: NodeRef<'_>) -> bool {
    n.parent()
        .and_then(|p| p.as_element())
        .is_some_and(|e| match e.tag() {
            Tag::Script | Tag::Style | Tag::Template => true,
            // SVG 里的 <style>、<script> 是 Other
            Tag::Other => matches!(e.name(), "script" | "style"),
            _ => false,
        })
}

#[cfg(test)]
mod tests {
    use pagesift_html::{Html, Tag};

    #[test]
    fn collapses_whitespace_and_skips_scripts() {
        let h = Html::parse(
            "<div>  Hello\n <b>big</b>world <script>x()</script> ! <svg><style>s</style></svg></div>",
        );
        let div = h.root().descendants().find(|n| n.tag() == Some(Tag::Div));
        assert_eq!(super::node_text(div.unwrap()), "Hello bigworld !");
    }
}
