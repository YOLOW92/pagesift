//! 全页的链接和图片列表（对应 Firecrawl 的 `links`、`images` 格式）。

use std::collections::HashSet;

use pagesift_html::Element;
use pagesift_html::Html;
use url::{Host, Url};

use crate::dom::{collapse_whitespace, element, tag};

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Link {
    /// 绝对地址，去掉了 fragment。
    pub url: String,
    /// 第一次出现时的链接文字。
    pub text: String,
    /// 和页面属于同一个站点（见 [`same_site`]）。
    pub internal: bool,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Image {
    /// 绝对地址：`src`（或 `data-src`），没有时取 `srcset` 里最大的一张。
    pub src: String,
    /// `srcset`（含外层 `<picture>` 的 `<source>`）里最大的一张。
    pub srcset_largest: Option<String>,
    pub alt: Option<String>,
}

/// `<a href>` 和 `<area href>`，只保留 http(s)，按出现顺序去重。没有基准 URL 时相对链接无法转换，跳过。
pub(crate) fn links(dom: &Html<'_>, base: Option<&Url>) -> Vec<Link> {
    let mut seen = HashSet::new();
    let mut out = Vec::new();
    for node in dom.root().descendants() {
        let Some(el) = element(&node) else { continue };
        if !matches!(el.name(), "a" | "area") {
            continue;
        }
        let Some(mut url) = el.attr("href").and_then(|h| absolute(base, h)) else {
            continue;
        };
        url.set_fragment(None);
        let url = url.to_string();
        if !seen.insert(url.clone()) {
            continue;
        }
        let text: String = node
            .descendants()
            .filter_map(|d| d.as_text().map(|t| t.to_string()))
            .collect();
        let internal = base.is_some_and(|b| same_site(b, &url));
        out.push(Link {
            text: collapse_whitespace(&text).trim().to_string(),
            url,
            internal,
        });
    }
    out
}

/// 页面里的 `<img>`，过滤掉 data: URI 和 1×1 的追踪像素，按地址去重。
pub(crate) fn images(dom: &Html<'_>, base: Option<&Url>) -> Vec<Image> {
    let mut seen = HashSet::new();
    let mut out = Vec::new();
    for node in dom.root().descendants() {
        let Some(el) = element(&node) else { continue };
        if el.name() != "img" || is_tracking_pixel(el) {
            continue;
        }
        let mut sets: Vec<&str> = el.attr("srcset").into_iter().collect();
        if let Some(picture) = node.parent().filter(|p| tag(p) == Some("picture")) {
            sets.extend(
                picture
                    .children()
                    .filter(|c| tag(c) == Some("source"))
                    .filter_map(|c| element(&c)?.attr("srcset")),
            );
        }
        let largest = largest_candidate(&sets).and_then(|u| absolute(base, u));
        let src = el
            .attr("src")
            .filter(|s| !s.trim().is_empty())
            .or_else(|| el.attr("data-src"))
            .and_then(|s| absolute(base, s))
            .or_else(|| largest.clone());
        let Some(src) = src else { continue };
        let src = src.to_string();
        if !seen.insert(src.clone()) {
            continue;
        }
        let alt = el
            .attr("alt")
            .map(|a| collapse_whitespace(a).trim().to_string())
            .filter(|a| !a.is_empty());
        out.push(Image {
            src,
            srcset_largest: largest.map(|u| u.to_string()),
            alt,
        });
    }
    out
}

/// 转成绝对地址，只接受 http(s)。
fn absolute(base: Option<&Url>, href: &str) -> Option<Url> {
    let href = href.trim();
    if href.is_empty() || href.starts_with('#') {
        return None;
    }
    let url = match base {
        Some(b) => b.join(href).ok()?,
        None => Url::parse(href).ok()?,
    };
    matches!(url.scheme(), "http" | "https").then_some(url)
}

fn is_tracking_pixel(el: Element<'_>) -> bool {
    let dim = |name: &str| {
        el.attr(name).map(|v| {
            let digits: String = v.trim().chars().take_while(char::is_ascii_digit).collect();
            digits.parse::<u32>().unwrap_or(u32::MAX)
        })
    };
    match (dim("width"), dim("height")) {
        (Some(w), Some(h)) => w <= 1 && h <= 1,
        (Some(d), None) | (None, Some(d)) => d <= 1,
        (None, None) => false,
    }
}

/// 按 HTML 规范解析 srcset，取宽度描述（`800w`）最大的；都没有宽度时取像素密度（`2x`）最大的。
/// 跳过 data: URI。
fn largest_candidate<'a>(sets: &[&'a str]) -> Option<&'a str> {
    let mut best: Option<(&str, bool, f64)> = None;
    for set in sets {
        for (url, desc) in srcset(set) {
            if url.starts_with("data:") {
                continue;
            }
            let (is_w, value) = if let Some(w) = desc.strip_suffix('w') {
                (true, w.parse().unwrap_or(0.0))
            } else if let Some(x) = desc.strip_suffix('x') {
                (false, x.parse().unwrap_or(1.0))
            } else {
                (false, 1.0)
            };
            let better = match best {
                None => true,
                Some((_, bw, bv)) => (is_w, value) > (bw, bv) || (is_w && !bw),
            };
            if better {
                best = Some((url, is_w, value));
            }
        }
    }
    best.map(|(u, _, _)| u)
}

/// srcset 的候选项：URL 可以带逗号（如 `w_100,h_100`），候选项之间用"逗号加空白"分隔。
pub(crate) fn srcset(s: &str) -> Vec<(&str, &str)> {
    let mut out = Vec::new();
    let mut rest = s;
    loop {
        rest = rest.trim_start_matches(|c: char| c.is_whitespace() || c == ',');
        if rest.is_empty() {
            break;
        }
        let end = rest.find(char::is_whitespace).unwrap_or(rest.len());
        let mut url = &rest[..end];
        rest = &rest[end..];
        let desc = if url.ends_with(',') {
            url = url.trim_end_matches(',');
            ""
        } else {
            let end = rest.find(',').unwrap_or(rest.len());
            let d = rest[..end].trim();
            rest = &rest[end..];
            d.split_whitespace().next().unwrap_or("")
        };
        if !url.is_empty() {
            out.push((url, desc));
        }
    }
    out
}

/// 是否同一个站点。没有引入公共后缀列表（它的数据是 MPL-2.0，需要单独放行许可证），
/// 用近似的"注册域"：取主机名最后两段；如果顶级域是两个字母的国家域、倒数第二段是
/// `co`、`com`、`net`、`org`、`gov`、`edu`、`ac` 这类常见二级域，取最后三段。
/// IP 地址要求完全相同。`github.io` 这类托管平台的子域会被当成同一站点。
pub fn same_site(base: &Url, url: &str) -> bool {
    let Ok(u) = Url::parse(url) else {
        return false;
    };
    match (base.host(), u.host()) {
        (Some(Host::Domain(a)), Some(Host::Domain(b))) => site_key(a) == site_key(b),
        (Some(a), Some(b)) => a == b,
        _ => false,
    }
}

fn site_key(host: &str) -> String {
    let host = host.trim_end_matches('.').to_ascii_lowercase();
    let labels: Vec<&str> = host.split('.').collect();
    let n = labels.len();
    if n <= 2 {
        return host;
    }
    let sld = labels[n - 2];
    let keep = if labels[n - 1].len() == 2
        && matches!(
            sld,
            "co" | "com"
                | "net"
                | "org"
                | "gov"
                | "edu"
                | "ac"
                | "or"
                | "ne"
                | "go"
                | "gob"
                | "mil"
                | "nic"
                | "ltd"
                | "plc"
                | "sch"
                | "nom"
        ) {
        3
    } else {
        2
    };
    labels[n.saturating_sub(keep)..].join(".")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_srcset() {
        assert_eq!(
            srcset("a.jpg 1x, b.jpg 2x"),
            vec![("a.jpg", "1x"), ("b.jpg", "2x")]
        );
        assert_eq!(
            srcset("https://c.com/w_100,h_100/a.jpg 100w,https://c.com/b.jpg 900w"),
            vec![
                ("https://c.com/w_100,h_100/a.jpg", "100w"),
                ("https://c.com/b.jpg", "900w")
            ]
        );
        assert_eq!(
            largest_candidate(&["a.jpg 300w, b.jpg 1200w, c.jpg 800w"]),
            Some("b.jpg")
        );
        assert_eq!(largest_candidate(&["a.jpg, b.jpg 3x"]), Some("b.jpg"));
        assert_eq!(largest_candidate(&["data:image/png;base64,xx 2x"]), None);
    }

    #[test]
    fn approximates_registrable_domain() {
        assert_eq!(site_key("www.example.com"), "example.com");
        assert_eq!(site_key("a.b.example.co.uk"), "example.co.uk");
        assert_eq!(site_key("news.bbc.co.uk"), "bbc.co.uk");
        assert_eq!(site_key("shop.example.de"), "example.de");
        assert_eq!(site_key("localhost"), "localhost");
        let base = Url::parse("https://blog.example.com/x").unwrap();
        assert!(same_site(&base, "https://example.com/"));
        assert!(!same_site(&base, "https://example.org/"));
    }
}
