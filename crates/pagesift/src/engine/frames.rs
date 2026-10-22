use super::*;

/// A thin page carrying client code must still get a chance to render even
/// after static neighbors taught the learner that rendering is usually useless.
/// Data-only scripts do not invalidate that optimization. Unknown script types
/// are conservative: an extra render is preferable to silently losing content.
pub(super) fn has_client_code(html: &str) -> bool {
    let dom = pagesift_html::Html::parse(html);
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

/// 可能承载正文的 iframe：同源的 `src`（属性值和解析后的地址）或内联的 `srcdoc`
/// （属性值是文档本身，地址为 `None`）。跨源的 iframe 多是统计、广告、视频和地图嵌入，正文提取本来就不要；
/// 看不见的、空的和 `about:blank` 也不算。
pub(super) fn content_iframes(html: &str, base: &Url) -> Vec<(String, Option<Url>)> {
    if !html
        .as_bytes()
        .windows(7)
        .any(|w| w[0] == b'<' && w[1..].eq_ignore_ascii_case(b"iframe"))
    {
        return Vec::new();
    }
    let doc = pagesift_html::Html::parse(html);
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
}

/// 每页最多用 HTTP 内联几个 iframe，多出来的不管（Chrome 系也有上限）。
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
    let bodies = futures_util::future::join_all(fetches.iter().map(|(_, u)| async move {
        let mut req = FetchRequest::new(u.as_str());
        req.timeout = Some(timeout);
        let mut resp = fetcher.fetch(req).await.ok()?;
        let html_like = resp
            .content_type()
            .is_none_or(|t| t.contains("html") || t.contains("xml"));
        Some(((200..300).contains(&resp.status) && html_like).then(|| resp.take_text().text))
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
    let lower = doc.as_bytes();
    let find = |pat: &[u8], from: usize| {
        lower[from..]
            .windows(pat.len())
            .position(|w| w.eq_ignore_ascii_case(pat))
            .map(|p| p + from)
    };
    let Some(open) = find(b"<body", 0) else {
        return doc;
    };
    let Some(start) = lower[open..]
        .iter()
        .position(|&c| c == b'>')
        .map(|p| open + p + 1)
    else {
        return doc;
    };
    let end = find(b"</body", start).unwrap_or(doc.len());
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
    while let Some(off) = b[i..]
        .windows(7)
        .position(|w| w[0] == b'<' && w[1..].eq_ignore_ascii_case(b"iframe"))
    {
        let start = i + off;
        let Some(tag_end) = b[start..]
            .iter()
            .position(|&c| c == b'>')
            .map(|p| start + p + 1)
        else {
            return false;
        };
        i = tag_end;
        let tag = pagesift_html::Html::parse(&html[start..tag_end]);
        let hit = tag.root().descendants().any(|n| {
            n.as_element()
                .is_some_and(|e| e.tag() == pagesift_html::Tag::Iframe && pred(&e))
        });
        if !hit {
            continue;
        }
        let end = b[tag_end..]
            .windows(8)
            .position(|w| w.eq_ignore_ascii_case(b"</iframe"))
            .and_then(|p| {
                let close = tag_end + p;
                b[close..]
                    .iter()
                    .position(|&c| c == b'>')
                    .map(|q| close + q + 1)
            })
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
        let s: String = s
            .chars()
            .filter(|c| !c.is_whitespace())
            .collect::<String>()
            .to_ascii_lowercase();
        s.contains("display:none")
            || s.contains("visibility:hidden")
            || s.contains("width:0")
            || s.contains("height:0")
    })
}
