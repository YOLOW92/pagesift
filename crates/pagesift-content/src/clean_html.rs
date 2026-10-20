//! 清洗后的 HTML（Firecrawl 的 `html` 格式）：套用和 Markdown 相同的跳过规则，
//! 去掉脚本、样式、注释、事件属性和内联 `style`，`href`、`src` 转成绝对地址。

use url::Url;

use crate::dom::{NodeR, element};
use crate::extract::Skipper;

pub(crate) fn render(root: NodeR<'_>, skipper: &Skipper, base: Option<&Url>) -> String {
    let mut out = String::new();
    for child in root.children() {
        write_node(child, skipper, base, &mut out);
    }
    out.trim().to_string()
}

fn write_node(node: NodeR<'_>, skipper: &Skipper, base: Option<&Url>, out: &mut String) {
    if let Some(t) = node.value().as_text() {
        escape(t, false, out);
        return;
    }
    let Some(el) = element(&node) else { return };
    // script、style 等由跳过规则的硬跳过部分处理
    if skipper.skip(&node) {
        return;
    }
    let name = el.name();
    out.push('<');
    out.push_str(name);
    for (key, value) in el.attrs() {
        if key.len() > 2 && key.get(..2).is_some_and(|p| p.eq_ignore_ascii_case("on"))
            || key == "style"
        {
            continue;
        }
        let value = if matches!(key, "href" | "src") {
            let v = value.trim();
            if v.get(..11)
                .is_some_and(|p| p.eq_ignore_ascii_case("javascript:"))
            {
                continue;
            }
            match base.and_then(|b| b.join(v).ok()) {
                Some(u) => u.to_string(),
                None => v.to_string(),
            }
        } else if key == "srcset" {
            crate::links::srcset(value)
                .into_iter()
                .map(|(u, d)| {
                    let u = base
                        .and_then(|b| b.join(u).ok())
                        .map_or_else(|| u.to_string(), |x| x.to_string());
                    if d.is_empty() { u } else { format!("{u} {d}") }
                })
                .collect::<Vec<_>>()
                .join(", ")
        } else {
            value.to_string()
        };
        out.push(' ');
        out.push_str(key);
        out.push_str("=\"");
        escape(&value, true, out);
        out.push('"');
    }
    out.push('>');
    if is_void(name) {
        return;
    }
    for child in node.children() {
        write_node(child, skipper, base, out);
    }
    out.push_str("</");
    out.push_str(name);
    out.push('>');
}

fn escape(s: &str, attr: bool, out: &mut String) {
    for c in s.chars() {
        match c {
            '&' => out.push_str("&amp;"),
            '<' if !attr => out.push_str("&lt;"),
            '>' if !attr => out.push_str("&gt;"),
            '"' if attr => out.push_str("&quot;"),
            '\u{a0}' => out.push_str("&nbsp;"),
            c => out.push(c),
        }
    }
}

fn is_void(name: &str) -> bool {
    matches!(
        name,
        "area"
            | "base"
            | "br"
            | "col"
            | "embed"
            | "hr"
            | "img"
            | "input"
            | "link"
            | "meta"
            | "source"
            | "track"
            | "wbr"
    )
}
