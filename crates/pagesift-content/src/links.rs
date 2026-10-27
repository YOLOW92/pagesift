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

/// 导航链接、分页、frame 文档和 HTMX GET 入口；不收集资源预加载或写操作。
/// 只保留 http(s)，按出现顺序去重。没有基准 URL 时相对链接无法转换，跳过。
pub(crate) fn links(dom: &Html<'_>, base: Option<&Url>) -> Vec<Link> {
    let mut seen = HashSet::new();
    let mut out = Vec::new();
    for node in dom.root().descendants() {
        let Some(el) = element(&node) else { continue };
        let target = match el.name() {
            "a" | "area" => el.attr("href"),
            "iframe" | "frame" if el.attr("srcdoc").is_none() => el.attr("src"),
            "link"
                if el.attr("rel").is_some_and(|r| {
                    r.split_ascii_whitespace()
                        .any(|v| v.eq_ignore_ascii_case("next") || v.eq_ignore_ascii_case("prev"))
                }) =>
            {
                el.attr("href")
            }
            _ => None,
        };
        // HTMX's empty GET target means the current document, not a new URL.
        // Never infer a GET from hx-post/put/patch/delete; script candidates
        // are handled separately and require an explicit navigation/GET primitive.
        for target in target
            .into_iter()
            .chain(el.attr("hx-get"))
            .chain(el.attr("data-hx-get"))
        {
            let Some(mut url) = absolute(base, target) else {
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
    }
    // Inspect executable inline scripts only, with a document-wide byte budget.
    let mut budget = 2 * 1024 * 1024;
    for node in dom.root().descendants() {
        let Some(el) = element(&node) else {
            continue;
        };
        if el.name() != "script"
            || el.attr("src").is_some()
            || el.attr("type").is_some_and(|v| {
                !matches!(
                    v.trim(),
                    "" | "module" | "text/javascript" | "application/javascript"
                )
            })
        {
            continue;
        }
        let code = node.text();
        if code.len() > budget {
            continue;
        }
        budget -= code.len();
        for url in crate::script_links::discover(&code, base) {
            if seen.insert(url.clone()) {
                let internal = base.is_some_and(|b| same_site(b, &url));
                out.push(Link {
                    url,
                    text: String::new(),
                    internal,
                });
            }
        }
    }
    out
}

/// 页面里的 `<img>`，过滤掉 data: URI 和 1×1 的追踪像素，按地址去重。
pub(crate) fn images(dom: &Html<'_>, base: Option<&Url>) -> Vec<Image> {
    let mut seen = HashSet::new();
    let mut out = Vec::new();
    // 同一个 <picture> 下 <source> 的最佳候选只算一次：一个 picture 里挂大量 img 时逐个重扫、
    // 重新解析是平方复杂度（模糊测试找到：160 万字节的页面 52 秒）
    type Best<'s> = Option<(&'s str, bool, f64)>;
    let mut picture_best: Option<(pagesift_html::NodeId, Best<'_>)> = None;
    for node in dom.root().descendants() {
        let Some(el) = element(&node) else { continue };
        if el.name() != "img" || is_tracking_pixel(el) {
            continue;
        }
        let sets: Vec<&str> = ["srcset", "data-srcset", "data-lazy-srcset"]
            .iter()
            .filter_map(|a| el.attr(a))
            .collect();
        let mut best = best_candidate(&sets);
        // img 自己的候选在前、picture 里 source 的在后
        if let Some(picture) = node.parent().filter(|p| tag(p) == Some("picture")) {
            if picture_best.is_none_or(|(id, _)| id != picture.id()) {
                let sources: Vec<&str> = picture
                    .children()
                    .filter(|c| tag(c) == Some("source"))
                    .filter_map(|c| element(&c)?.attr("srcset"))
                    .collect();
                picture_best = Some((picture.id(), best_candidate(&sources)));
            }
            if let Some((_, from_sources)) = picture_best
                && later_wins(best, from_sources)
            {
                best = from_sources;
            }
        }
        let largest = best.and_then(|(u, _, _)| absolute(base, u));
        let src = ["src", "data-src", "data-lazy-src", "data-original"]
            .iter()
            .filter_map(|a| el.attr(a))
            .find_map(|s| absolute(base, s))
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
#[cfg(test)]
fn largest_candidate<'a>(sets: &[&'a str]) -> Option<&'a str> {
    best_candidate(sets).map(|(u, _, _)| u)
}

/// 候选项里（宽度描述符优先、数值大的优先）最大的第一个：(URL, 是否宽度描述符, 数值)。
/// 比较是全序（NaN 按解析失败处理），所以分段求出的结果可以用 [`later_wins`] 合并。
fn best_candidate<'a>(sets: &[&'a str]) -> Option<(&'a str, bool, f64)> {
    let parse = |v: &str, default: f64| {
        v.parse()
            .ok()
            .filter(|x: &f64| !x.is_nan())
            .unwrap_or(default)
    };
    let mut best: Option<(&str, bool, f64)> = None;
    for set in sets {
        for (url, desc) in srcset(set) {
            if url.starts_with("data:") {
                continue;
            }
            let (is_w, value) = if let Some(w) = desc.strip_suffix('w') {
                (true, parse(w, 0.0))
            } else if let Some(x) = desc.strip_suffix('x') {
                (false, parse(x, 1.0))
            } else {
                (false, 1.0)
            };
            let cand = Some((url, is_w, value));
            if later_wins(best, cand) {
                best = cand;
            }
        }
    }
    best
}

/// 后出现的候选 `b` 是否胜过前面已选出的 `a`（严格更大才算，相同时保留先出现的）。
fn later_wins(a: Option<(&str, bool, f64)>, b: Option<(&str, bool, f64)>) -> bool {
    match (a, b) {
        (_, None) => false,
        (None, Some(_)) => true,
        (Some((_, aw, av)), Some((_, bw, bv))) => (bw, bv) > (aw, av),
    }
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

pub fn site_key(host: &str) -> String {
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
