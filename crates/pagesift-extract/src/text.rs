use ego_tree::NodeRef;
use scraper::Node;

/// 元素的可见文本：拼接后代文本节点，空白折叠成单个空格并去掉首尾空白。
pub(crate) fn node_text(node: NodeRef<'_, Node>) -> String {
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
    match node.value() {
        Node::Text(t) => push(t, &mut out),
        Node::Comment(c) => push(c, &mut out),
        _ => {
            for d in node.descendants() {
                if let Node::Text(t) = d.value()
                    && !in_hidden_parent(d)
                {
                    push(t, &mut out);
                }
            }
        }
    }
    out
}

fn in_hidden_parent(n: NodeRef<'_, Node>) -> bool {
    n.parent()
        .and_then(|p| {
            p.value()
                .as_element()
                .map(|e| matches!(e.name(), "script" | "style" | "template"))
        })
        .unwrap_or(false)
}

#[cfg(test)]
mod tests {
    use scraper::Html;

    #[test]
    fn collapses_whitespace_and_skips_scripts() {
        let h =
            Html::parse_fragment("<div>  Hello\n <b>big</b>world <script>x()</script> ! </div>");
        let div = h.root_element().first_child().unwrap();
        assert_eq!(super::node_text(div), "Hello bigworld !");
    }
}
