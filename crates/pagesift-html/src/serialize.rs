//! 把子树序列化回 HTML，转义和空元素规则和 html5ever 的序列化器一致（scraper 的 `ElementRef::html` 用的就是它）。
//! 不同的是属性保持源码顺序（scraper 按名字排序），和 BeautifulSoup 一样。

use crate::{NodeRef, Tag};

impl NodeRef<'_> {
    /// 节点自身及其子树的 HTML（元素的 outer HTML）。
    pub fn html(&self) -> String {
        let mut out = String::new();
        write_tree(*self, &mut out, true);
        out
    }

    /// 子节点的 HTML（元素的 inner HTML）。
    pub fn inner_html(&self) -> String {
        let mut out = String::new();
        for c in self.children() {
            write_tree(c, &mut out, false);
        }
        out
    }
}

/// 不写子节点和结束标签的元素。
fn is_void(tag: Option<Tag>) -> bool {
    use Tag::*;
    matches!(
        tag,
        Some(
            Area | Base
                | Basefont
                | Bgsound
                | Br
                | Col
                | Embed
                | Frame
                | Hr
                | Img
                | Input
                | Keygen
                | Link
                | Meta
                | Param
                | Source
                | Track
                | Wbr
        )
    )
}

/// 文本不转义的父元素。noscript 也会被转义：scraper 序列化时关掉了 scripting。
fn is_raw_text(tag: Option<Tag>) -> bool {
    use Tag::*;
    matches!(
        tag,
        Some(Style | Script | Xmp | Iframe | Noembed | Noframes | Plaintext)
    )
}

/// 非递归地先序遍历 `root` 的子树。`root_is_top` 时 `root` 的文本按没有父元素处理（总是转义）。
fn write_tree(root: NodeRef<'_>, out: &mut String, root_is_top: bool) {
    let mut n = root;
    loop {
        let raw = !(root_is_top && n == root) && is_raw_text(n.parent().and_then(|p| p.tag()));
        open(n, out, raw);
        if !is_void(n.tag())
            && let Some(c) = n.first_child()
        {
            n = c;
            continue;
        }
        loop {
            close(n, out);
            if n == root {
                return;
            }
            match n.next_sibling() {
                Some(s) => {
                    n = s;
                    break;
                }
                None => match n.parent() {
                    Some(p) => n = p,
                    None => return,
                },
            }
        }
    }
}

fn open(n: NodeRef<'_>, out: &mut String, raw: bool) {
    if let Some(e) = n.as_element() {
        out.push('<');
        out.push_str(e.name());
        for (k, v) in e.attrs() {
            out.push(' ');
            out.push_str(k);
            out.push_str("=\"");
            escape(v, true, out);
            out.push('"');
        }
        out.push('>');
    } else if let Some(t) = n.as_text() {
        if raw {
            out.push_str(t);
        } else {
            escape(t, false, out);
        }
    } else if let Some(c) = n.as_comment() {
        out.push_str("<!--");
        out.push_str(c);
        out.push_str("-->");
    } else if !n.is_document() {
        // 树里不保留 doctype 的名字，HTML 文档里它总是 html
        out.push_str("<!DOCTYPE html>");
    }
}

fn close(n: NodeRef<'_>, out: &mut String) {
    if let Some(e) = n.as_element()
        && !is_void(Some(e.tag()))
    {
        out.push_str("</");
        out.push_str(e.name());
        out.push('>');
    }
}

fn escape(s: &str, attr: bool, out: &mut String) {
    let b = s.as_bytes();
    let mut last = 0;
    for (i, &c) in b.iter().enumerate() {
        let rep = match c {
            b'&' => "&amp;",
            b'<' => "&lt;",
            b'>' => "&gt;",
            b'"' if attr => "&quot;",
            // U+00A0 的 UTF-8 编码是 C2 A0
            0xA0 if i > 0 && b[i - 1] == 0xC2 => "&nbsp;",
            _ => continue,
        };
        let start = if c == 0xA0 { i - 1 } else { i };
        out.push_str(&s[last..start]);
        out.push_str(rep);
        last = i + 1;
    }
    out.push_str(&s[last..]);
}

#[cfg(test)]
mod tests {
    use crate::{Html, Tag};

    #[test]
    fn outer_and_inner_html() {
        let doc = Html::parse(
            "<div class=\"a&quot;b\" title='x\u{a0}y'>1 &lt; 2 &amp; <br><img src=x>\u{a0}<!--c-->\
             <script>if (a < b) {}</script><style>a>b{}</style><noscript><b>n</b></noscript><svg><style>s&lt;x</style></svg></div>",
        );
        let div = doc
            .root()
            .descendants()
            .find(|n| n.tag() == Some(Tag::Div))
            .unwrap();
        assert_eq!(
            div.html(),
            "<div class=\"a&quot;b\" title=\"x&nbsp;y\">1 &lt; 2 &amp; <br><img src=\"x\">&nbsp;<!--c-->\
             <script>if (a < b) {}</script><style>a>b{}</style><noscript>&lt;b&gt;n&lt;/b&gt;</noscript><svg><style>s&lt;x</style></svg></div>"
        );
        let br = div.children().find(|n| n.tag() == Some(Tag::Br)).unwrap();
        assert_eq!(br.html(), "<br>");
        let script = div
            .children()
            .find(|n| n.tag() == Some(Tag::Script))
            .unwrap();
        assert_eq!(script.inner_html(), "if (a < b) {}");
        assert_eq!(script.first_child().unwrap().html(), "if (a &lt; b) {}");
    }
}
