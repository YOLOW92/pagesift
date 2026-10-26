//! 行内渲染：强调、链接、图片、行内代码。

use super::*;

impl Renderer<'_> {
    /// 同类强调已经在外层包过（`<b><b>…`、`<b><strong>…`）就不再重复加标记：
    /// 层层嵌套的强调在 Markdown 里没有意义，只会输出满屏的 `*`。
    pub(super) fn emphasis(&self, node: NodeR<'_>, same: &[Tag], marker: &str, out: &mut String) {
        // 最多往上看 32 层：病态的十万层行内嵌套里没有块级元素，不设上限就成了平方复杂度
        let nested = node
            .ancestors()
            .take(32)
            .take_while(|a| a.tag().is_none_or(|t| !is_block_tag(t)))
            .any(|a| a.tag().is_some_and(|t| same.contains(&t)));
        if nested {
            self.inline_children_into(node, out);
        } else {
            out.push_str(&wrap(&self.inline_children(node), marker));
        }
    }

    pub(super) fn inline_children(&self, node: NodeR<'_>) -> String {
        let mut s = String::new();
        self.inline_children_into(node, &mut s);
        s
    }

    pub(super) fn inline_children_into(&self, node: NodeR<'_>, out: &mut String) {
        for child in node.children() {
            if !self.skip(&child) {
                self.inline_into(child, out);
            }
        }
    }

    /// 行内渲染，直接写进 `out`；只有需要包裹的元素（加粗、链接等）才先渲染到临时串。
    pub(super) fn inline_into(&self, node: NodeR<'_>, out: &mut String) {
        if let Some(t) = node.as_text() {
            push_text(t, out, self.skipper.has_math());
            return;
        }
        let Some(el) = element(&node) else {
            return;
        };
        if let Some(m) = self.math(&node) {
            if let Math::Tex(tex, display) = m {
                let d = if display { "$$" } else { "$" };
                out.push_str(d);
                out.push_str(&tex);
                out.push_str(d);
            }
            return;
        }
        if self.skip(&node) {
            return;
        }
        match el.tag() {
            Tag::Br => out.push('\n'),
            Tag::Wbr => {}
            Tag::Img => out.push_str(&self.image(el)),
            Tag::Strong | Tag::B => self.emphasis(node, &[Tag::Strong, Tag::B], "**", out),
            Tag::Em | Tag::I => self.emphasis(node, &[Tag::Em, Tag::I], "*", out),
            Tag::Del | Tag::S | Tag::Strike => {
                self.emphasis(node, &[Tag::Del, Tag::S, Tag::Strike], "~~", out)
            }
            Tag::Code | Tag::Kbd | Tag::Samp | Tag::Tt => {
                out.push_str(&inline_code(&collapse_whitespace(&raw_text(&node))))
            }
            Tag::A => out.push_str(&self.link(node, el)),
            t if is_block_tag(t) => {
                out.push(' ');
                self.inline_children_into(node, out);
                out.push(' ');
            }
            _ => self.inline_children_into(node, out),
        }
    }

    pub(super) fn link(&self, node: NodeR<'_>, el: Element<'_>) -> String {
        let inner = self.inline_children(node);
        let text = single_line(&inner);
        let Some(href) = el.attr("href").map(str::trim) else {
            return inner;
        };
        if href.is_empty()
            || href.starts_with('#')
            || href.to_ascii_lowercase().starts_with("javascript:")
        {
            return inner;
        }
        if text.is_empty() {
            return String::new();
        }
        let url = self.resolve(href);
        let (lead, trail) = edges(&inner);
        format!("{lead}[{text}]({url}){trail}")
    }

    pub(super) fn image(&self, el: Element<'_>) -> String {
        let Some(src) = image_src(el) else {
            return String::new();
        };
        let alt = single_line(&collapse_whitespace(el.attr("alt").unwrap_or("")));
        let title = single_line(&collapse_whitespace(el.attr("title").unwrap_or("")));
        let title = if title.is_empty() {
            String::new()
        } else {
            format!(" \"{}\"", title.replace('\\', "\\\\").replace('"', "\\\""))
        };
        format!(
            "![{}]({}{title})",
            alt.replace(['[', ']'], ""),
            self.resolve(src)
        )
    }

    pub(super) fn resolve(&self, href: &str) -> String {
        let resolved = match self.base {
            Some(base) => base.join(href).map(|u| u.to_string()).ok(),
            None => Url::parse(href).map(|u| u.to_string()).ok(),
        };
        let s = resolved.unwrap_or_else(|| href.to_string());
        s.replace(' ', "%20")
            .replace('(', "%28")
            .replace(')', "%29")
    }
}

pub(super) fn class_contains(node: &NodeR<'_>, needle: &str) -> bool {
    element(node).is_some_and(|el| class_contains_el(el, needle))
}

pub(super) fn class_contains_el(el: Element<'_>, needle: &str) -> bool {
    el.attr("class")
        .is_some_and(|class| crate::classes::contains_ascii_ci(class, needle))
}

/// 图片地址。懒加载的图片 `src` 常是空的或 `data:` 占位图，真实地址在 `data-src`、
/// `data-lazy-src`、`data-original` 或 `srcset` 里；`data:` 地址本身不输出。
pub(super) fn image_src(el: Element<'_>) -> Option<&str> {
    let usable = |s: &&str| {
        !s.is_empty()
            && !s.starts_with('#')
            && match Url::parse(s) {
                Ok(u) => matches!(u.scheme(), "http" | "https"),
                Err(_) => true,
            }
    };
    ["src", "data-src", "data-lazy-src", "data-original"]
        .iter()
        .filter_map(|a| el.attr(a).map(str::trim))
        .find(usable)
        .or_else(|| {
            ["srcset", "data-srcset", "data-lazy-srcset"]
                .iter()
                .filter_map(|a| el.attr(a))
                .filter_map(|v| crate::links::srcset(v).first().map(|c| c.0))
                .find(usable)
        })
}

pub(super) fn edges(s: &str) -> (&'static str, &'static str) {
    let lead = if s.starts_with(char::is_whitespace) {
        " "
    } else {
        ""
    };
    let trail = if s.ends_with(char::is_whitespace) {
        " "
    } else {
        ""
    };
    (lead, trail)
}

pub(super) fn wrap(inner: &str, marker: &str) -> String {
    let text = single_line(inner);
    if text.is_empty() {
        return inner.to_string();
    }
    let (lead, trail) = edges(inner);
    format!("{lead}{marker}{text}{marker}{trail}")
}

pub(super) fn inline_code(text: &str) -> String {
    let text = text.trim();
    if text.is_empty() {
        return String::new();
    }
    let mut ticks = "`".to_string();
    while text.contains(&ticks) {
        ticks.push('`');
    }
    let pad = if text.starts_with('`') || text.ends_with('`') {
        " "
    } else {
        ""
    };
    format!("{ticks}{pad}{text}{pad}{ticks}")
}

/// 折叠成一行：换行和连续空格都变成单个空格，去掉首尾空白。
pub(super) fn single_line(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    push_single_line(s, &mut out);
    out
}

pub(super) fn push_single_line(s: &str, out: &mut String) {
    for (i, w) in s.split_whitespace().enumerate() {
        if i > 0 {
            out.push(' ');
        }
        out.push_str(w);
    }
}
