use super::*;

/// A thin page carrying client code must still get a chance to render even
/// after static neighbors taught the learner that rendering is usually useless.
/// Data-only scripts do not invalidate that optimization. Unknown script types
/// are conservative: an extra render is preferable to silently losing content.
pub(super) fn has_client_code(html: &str) -> bool {
    super::cpu::with_dom(html, client_code_in)
}

/// [`has_client_code`] 在已经建好的 DOM 上判断（提取用的那棵），不再解析一遍。
pub(super) fn client_code_in(dom: &pagesift_html::Html<'_>) -> bool {
    dom.root().descendants().any(|node| {
        let Some(el) = node.as_element() else {
            return false;
        };
        if el
            .attrs()
            .any(|(name, _)| name.starts_with("on") && name.len() > 2)
        {
            return true;
        }
        if el.name() != "script" {
            return false;
        }
        let kind = el
            .attr("type")
            .unwrap_or("")
            .split(';')
            .next()
            .unwrap_or("")
            .trim();
        ![
            "application/json",
            "application/ld+json",
            "text/plain",
            "importmap",
            "speculationrules",
        ]
        .iter()
        .any(|data| kind.eq_ignore_ascii_case(data))
    })
}

/// Detect existing thin/JS-shell clues inside a parsed DOM + extracted markdown without
/// misclassifying normal short server-rendered pages (which may have analytics scripts).
pub(super) fn detect_js_shell(
    dom: &pagesift_html::Html<'_>,
    markdown: &str,
    client_code: bool,
) -> bool {
    if !client_code {
        return false;
    }
    let md_non_ws = markdown.chars().filter(|c| !c.is_whitespace()).count();
    if md_non_ws == 0 {
        return true;
    }
    if md_non_ws >= 200 {
        return false;
    }
    let has_empty_spa_mount = dom.root().descendants().any(|node| {
        let Some(el) = node.as_element() else {
            return false;
        };
        let is_mount_id = el.attr("id").is_some_and(|id| {
            matches!(
                id.to_ascii_lowercase().as_str(),
                "app" | "root" | "__next" | "__nuxt" | "svelte"
            )
        });
        let is_mount_tag = el.name().eq_ignore_ascii_case("app-root");
        (is_mount_id || is_mount_tag) && node.text().trim().is_empty()
    });
    if has_empty_spa_mount {
        return true;
    }
    let lower = markdown.trim().to_ascii_lowercase();
    lower.contains("enable javascript")
        || lower.contains("javascript is required")
        || lower.contains("requires javascript")
        || lower.contains("turn on javascript")
        || matches!(lower.as_str(), "loading" | "loading..." | "please wait...")
        || lower.starts_with("loading ")
}

/// HTML 标签名后的合法边界字节（空白、`/` 或 `>`），防止 `<iframe-loader>` / `<body-shell>` 误匹配。
#[inline]
fn is_tag_boundary(b: Option<&u8>) -> bool {
    matches!(
        b,
        Some(b' ' | b'\t' | b'\n' | b'\r' | b'\x0c' | b'/' | b'>')
    )
}

/// 从 `from` 起查找大小写无关标签前缀 `pat`（如 `b"<body"` 或 `b"</iframe"`），且其后必须是标签名边界。
fn find_html_tag(bytes: &[u8], pat: &[u8], mut from: usize) -> Option<usize> {
    while from + pat.len() <= bytes.len() {
        let off = bytes[from..]
            .windows(pat.len())
            .position(|w| w.eq_ignore_ascii_case(pat))?;
        let idx = from + off;
        if is_tag_boundary(bytes.get(idx + pat.len())) {
            return Some(idx);
        }
        from = idx + 1;
    }
    None
}

/// 查找开标签结束的 `>`（返回闭合 `>` 之后的字节下标），严格跳过 `=` 后单/双引号属性值内的 `>`。
fn find_open_tag_end(bytes: &[u8], tag_start: usize) -> Option<usize> {
    let mut quote: Option<u8> = None;
    let mut after_eq = false;
    for (off, &c) in bytes[tag_start..].iter().enumerate() {
        if let Some(q) = quote {
            if c == q {
                quote = None;
            }
            continue;
        }
        match c {
            b'>' => return Some(tag_start + off + 1),
            b'=' => after_eq = true,
            b'"' | b'\'' if after_eq => {
                quote = Some(c);
                after_eq = false;
            }
            c if c.is_ascii_whitespace() => {}
            _ => after_eq = false,
        }
    }
    None
}

/// 可能承载正文的 iframe：同源的 `src`（属性值和解析后的地址）或内联的 `srcdoc`
/// （属性值是文档本身，地址为 `None`）。跨源的 iframe 多是统计、广告、视频和地图嵌入，正文提取本来就不要；
/// 看不见的、空的和 `about:blank` 也不算。
pub(super) fn content_iframes(html: &str, base: &Url) -> Vec<(String, Option<Url>)> {
    if find_html_tag(html.as_bytes(), b"<iframe", 0).is_none() {
        return Vec::new();
    }
    super::cpu::with_dom(html, |doc| {
        doc.root()
            .descendants()
            .filter_map(|n| {
                let el = n
                    .as_element()
                    .filter(|e| e.tag() == pagesift_html::Tag::Iframe && !hidden_frame(e))?;
                if let Some(doc) = el.attr("srcdoc").filter(|d| !d.trim().is_empty()) {
                    return Some((doc.to_string(), None));
                }
                let raw = el
                    .attr("src")
                    .map(str::trim)
                    .filter(|s| !s.is_empty() && !s.starts_with("about:"))?;
                let u = base.join(raw).ok().filter(|u| {
                    u.host_str() == base.host_str()
                        && u.port_or_known_default() == base.port_or_known_default()
                })?;
                Some((raw.to_string(), Some(u)))
            })
            .collect()
    })
}

/// [策略] 每页最多用 HTTP 内联几个 iframe，多出来的不管（Chrome 系也有上限）。这些请求用页面已占的名额、
/// 不再单独排限速队列，上限限制的是一页能对同一主机多发多少不排队的请求（礼貌上限，不是增益取舍）。
pub(super) const MAX_HTTP_IFRAMES: usize = 10;

/// 不渲染 iframe 的内核（Lightpanda）渲染完后，用 HTTP 抓同源 iframe 的文档，把 `<body>` 内容
/// 内联进去，格式和 Chrome 系内联的一样（`<div data-pagesift-iframe>`）。iframe 是页面的子资源，
/// 和浏览器的子请求一样不再单独排限速队列（页面的名额还在手里）。非 2xx 或不是 HTML 的
/// 响应直接丢掉：真浏览器也拿不到正文。`srcdoc` 的文档就在属性里，直接内联。返回没能处理的
/// iframe 数（网络错误、找不到标签），大于 0 时由调用方回退到渲染 iframe 的内核。
pub(super) async fn inline_iframes_http(
    fetcher: &Fetcher,
    html: &mut String,
    frames: Vec<(String, Option<Url>)>,
    timeout: Duration,
) -> usize {
    let mut missing = 0;
    let mut fetches = Vec::new();
    for (raw, url) in frames {
        match url {
            Some(u) if fetches.len() < MAX_HTTP_IFRAMES => fetches.push((raw, u)),
            Some(_) => {}
            None => {
                let replacement = format!(
                    "<div data-pagesift-iframe=\"about:srcdoc\">{}</div>",
                    body_inner(&raw)
                );
                let found = replace_iframe(html, &replacement, |el| {
                    !hidden_frame(el) && el.attr("srcdoc") == Some(raw.as_str())
                });
                if !found {
                    tracing::debug!("iframe not inlined: srcdoc tag not found");
                    missing += 1;
                }
            }
        }
    }
    let itna_budget = super::ITNA_NETWORK_BUDGET
        .try_with(std::sync::Arc::clone)
        .ok();
    let allowed_hosts = super::ACTION_ALLOWED_HOSTS
        .try_with(|hosts| hosts.as_ref().clone())
        .unwrap_or_default();
    let bodies = futures_util::future::join_all(fetches.iter().map(|(_, u)| {
        let itna_budget = itna_budget.clone();
        let allowed_hosts = allowed_hosts.clone();
        async move {
            let mut req = FetchRequest::new(u.as_str());
            req.timeout = Some(timeout);
            req.network_budget = itna_budget;
            req.allowed_hosts = allowed_hosts;
            let mut resp = fetcher.fetch(req).await.ok()?;
            let html_like = resp
                .content_type()
                .is_none_or(|t| t.contains("html") || t.contains("xml"));
            Some(((200..300).contains(&resp.status) && html_like).then(|| resp.take_text().text))
        }
    }))
    .await;
    for ((raw, url), body) in fetches.iter().zip(bodies) {
        let body = match body {
            Some(Some(b)) => b,
            Some(None) => {
                // 有响应但没有正文可内联：去掉这个 iframe，和真浏览器的结果一样是空的
                replace_iframe(html, "", |el| src_is(el, raw));
                continue;
            }
            None => {
                tracing::debug!(src = %url, "iframe not inlined: fetch failed");
                missing += 1;
                continue;
            }
        };
        let inner = body_inner(&body);
        let replacement = format!(
            "<div data-pagesift-iframe=\"{}\">{inner}</div>",
            url.as_str().replace('"', "&quot;")
        );
        if !replace_iframe(html, &replacement, |el| src_is(el, raw)) {
            tracing::debug!(src = %url, raw, "iframe not inlined: tag not found");
            missing += 1;
        }
    }
    missing
}

/// 文档 `<body>` 的内容；找不到 body 标签时返回整个文档。
pub(super) fn body_inner(doc: &str) -> &str {
    let bytes = doc.as_bytes();
    let Some(open) = find_html_tag(bytes, b"<body", 0) else {
        return doc;
    };
    let Some(start) = find_open_tag_end(bytes, open) else {
        return doc;
    };
    let end = find_html_tag(bytes, b"</body", start).unwrap_or(doc.len());
    &doc[start..end]
}

pub(super) fn src_is(el: &pagesift_html::Element<'_>, raw: &str) -> bool {
    el.attr("src").map(str::trim) == Some(raw)
}

/// 把第一个满足 `pred` 的 `<iframe>…</iframe>` 换成 `replacement`。
pub(super) fn replace_iframe(
    html: &mut String,
    replacement: &str,
    pred: impl Fn(&pagesift_html::Element<'_>) -> bool,
) -> bool {
    let b = html.as_bytes();
    let mut i = 0;
    while let Some(start) = find_html_tag(b, b"<iframe", i) {
        let Some(tag_end) = find_open_tag_end(b, start) else {
            return false;
        };
        i = tag_end;
        let hit = super::cpu::with_dom(&html[start..tag_end], |tag| {
            tag.root().descendants().any(|n| {
                n.as_element()
                    .is_some_and(|e| e.tag() == pagesift_html::Tag::Iframe && pred(&e))
            })
        });
        if !hit {
            continue;
        }
        let end = find_html_tag(b, b"</iframe", tag_end)
            .and_then(|close| find_open_tag_end(b, close))
            .unwrap_or(tag_end);
        html.replace_range(start..end, replacement);
        return true;
    }
    false
}

/// 看不见的 iframe：统计像素、沙箱脚本、预取用的空框架，不会有正文。
pub(super) fn hidden_frame(el: &pagesift_html::Element<'_>) -> bool {
    if el.attr("hidden").is_some()
        || el
            .attr("aria-hidden")
            .is_some_and(|v| v.eq_ignore_ascii_case("true"))
        || el
            .attr("role")
            .is_some_and(|v| v.eq_ignore_ascii_case("presentation"))
    {
        return true;
    }
    let tiny = |name| {
        el.attr(name).is_some_and(|v| {
            v.trim()
                .trim_end_matches("px")
                .parse::<f32>()
                .is_ok_and(|n| n <= 2.0)
        })
    };
    if tiny("width") || tiny("height") {
        return true;
    }
    el.attr("style").is_some_and(|s| {
        if s.is_empty() {
            return false;
        }
        let mut norm = String::with_capacity(s.len());
        for c in s.chars() {
            if !c.is_whitespace() {
                norm.push(c.to_ascii_lowercase());
            }
        }
        norm.contains("display:none")
            || norm.contains("visibility:hidden")
            || norm.contains("width:0")
            || norm.contains("height:0")
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn replace_iframe_handles_markup_gt_inside_quoted_srcdoc() {
        let base = Url::parse("https://example.com/page").unwrap();
        let mut html = String::from(
            r#"<main><iframe-loader data-x="1"></iframe-loader><iframe srcdoc="<body data-note='a > b'><article><h1>Embedded Title</h1><p>2 > 1 inside markup</p></article></body>"></iframe></main>"#,
        );
        let frames = content_iframes(&html, &base);
        assert_eq!(frames.len(), 1);
        assert!(frames[0].1.is_none());
        let raw = &frames[0].0;
        let inner = body_inner(raw);
        assert_eq!(
            inner,
            "<article><h1>Embedded Title</h1><p>2 > 1 inside markup</p></article>"
        );
        let replacement = format!("<div data-pagesift-iframe=\"about:srcdoc\">{inner}</div>");
        let replaced = replace_iframe(&mut html, &replacement, |el| {
            !hidden_frame(el) && el.attr("srcdoc") == Some(raw.as_str())
        });
        assert!(replaced, "srcdoc containing HTML tags must be matched");
        assert!(html.contains("<iframe-loader data-x=\"1\"></iframe-loader>"));
        assert!(html.contains(
            "<div data-pagesift-iframe=\"about:srcdoc\"><article><h1>Embedded Title</h1><p>2 > 1 inside markup</p></article></div>"
        ));
        assert!(!html.contains("<iframe "));
    }

    #[test]
    fn body_inner_skips_custom_body_prefix_and_quoted_gt() {
        let doc = r#"<html><body-shell data-a="x"><body data-expr="a > b"><p>real body</p></body></body-shell></html>"#;
        assert_eq!(body_inner(doc), "<p>real body</p>");
    }
}
