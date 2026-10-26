//! RSS / Atom / RDF feed 解析：feed 不是网页，按条目输出成 Markdown 列表。
//!
//! 0.71 及以前把 feed 当 HTML 解析，标签被剥掉后条目粘连在一起，转义碎片也留在里面。
//! 这里按 XML 结构读，保留标题、链接、日期和摘要。

use pagesift_content::{Document, Metadata};

#[derive(Default)]
struct Item {
    title: String,
    link: String,
    summary: String,
    date: String,
}

#[derive(Default)]
struct Feed {
    title: String,
    description: String,
    language: String,
    link: String,
    items: Vec<Item>,
}

/// 识别并解析 feed。不是 feed 时返回 `None`，调用方继续走 HTML 提取。
pub(super) fn parse(xml: &str, wants_links: bool) -> Option<Document> {
    let body = xml.strip_prefix('\u{feff}').unwrap_or(xml).trim_start();
    if !body.starts_with('<') {
        return None;
    }
    // 按字符边界截：开头 4 KB 里常有中日韩标题，按字节截会切在字符中间 panic。
    let mut end = body.len().min(4096);
    while !body.is_char_boundary(end) {
        end -= 1;
    }
    let head = &body[..end];
    let feed_like = head.contains("<rss")
        || head.contains("<feed")
        || head.contains("<rdf:RDF")
        || head.contains("<RDF");
    if !feed_like || !(body.contains("<item") || body.contains("<entry")) {
        return None;
    }
    let mut feed = read(body)?;
    if feed.items.is_empty() {
        return None;
    }
    // 条目链接来自不可信的 XML：只留 http(s)，相对地址按 feed 自己的链接补全。
    let base = pagesift_net::Url::parse(&feed.link).ok();
    for item in &mut feed.items {
        item.link = safe_link(&item.link, base.as_ref()).unwrap_or_default();
    }

    let mut markdown = String::new();
    if !feed.title.is_empty() {
        markdown.push_str("# ");
        markdown.push_str(&feed.title);
        markdown.push_str("\n\n");
    }
    if !feed.description.is_empty() {
        markdown.push_str(&feed.description);
        markdown.push_str("\n\n");
    }
    for item in &feed.items {
        let title = if item.title.is_empty() {
            "Untitled"
        } else {
            &item.title
        };
        match item.link.is_empty() {
            false => {
                markdown.push_str(&format!("- [{}]({})", escape_link_text(title), item.link));
            }
            true => {
                markdown.push_str("- ");
                markdown.push_str(&escape_link_text(title));
            }
        }
        if !item.date.is_empty() {
            markdown.push_str(" — ");
            markdown.push_str(&item.date);
        }
        markdown.push('\n');
        if !item.summary.is_empty() {
            markdown.push_str("  ");
            markdown.push_str(&item.summary);
            markdown.push('\n');
        }
    }

    let links = wants_links.then(|| {
        feed.items
            .iter()
            .filter(|i| !i.link.is_empty())
            .map(|i| pagesift_content::Link {
                url: i.link.clone(),
                text: i.title.clone(),
                internal: base
                    .as_ref()
                    .is_some_and(|b| pagesift_content::same_site(b, &i.link)),
            })
            .collect()
    });
    Some(Document {
        markdown,
        metadata: Metadata {
            title: (!feed.title.is_empty()).then_some(feed.title),
            description: (!feed.description.is_empty()).then_some(feed.description),
            language: (!feed.language.is_empty()).then_some(feed.language),
            ..Default::default()
        },
        links,
        barrier: None,
        plain_text: None,
        html: None,
        images: None,
        fingerprints: None,
        coverage: Default::default(),
    })
}

fn read(xml: &str) -> Option<Feed> {
    use quick_xml::events::Event;
    let mut reader = quick_xml::Reader::from_reader(xml.as_bytes());
    let mut feed = Feed::default();
    let mut in_item = false;
    let mut item = Item::default();
    // 当前字段：channel/feed 级的 title、description、language、link，
    // 或 item/entry 里的 title、link、summary、date。
    let mut field: Option<&'static str> = None;
    let mut text = String::new();
    let mut buf = Vec::new();
    let mut depth = 0usize;
    loop {
        match reader.read_event_into(&mut buf) {
            Ok(Event::Start(e)) => {
                depth += 1;
                match e.local_name().as_ref() {
                    "item" | "entry" => {
                        in_item = true;
                        item = Item::default();
                    }
                    "title" => field = Some("title"),
                    "link" if in_item => {
                        // Atom: <link href="…"/>；RSS: <link>…</link>
                        if let Some(href) = attr(&e, "href") {
                            if item.link.is_empty() {
                                item.link = href;
                            }
                            field = None;
                        } else {
                            field = Some("link");
                        }
                    }
                    "link" => {
                        if let Some(href) = attr(&e, "href") {
                            if feed.link.is_empty() {
                                feed.link = href;
                            }
                            field = None;
                        } else {
                            field = Some("feedlink");
                        }
                    }
                    "description" | "summary" | "content" if in_item => field = Some("summary"),
                    "description" if !in_item => field = Some("feeddesc"),
                    "language" => field = Some("language"),
                    "pubDate" | "published" | "updated" | "date" if in_item => field = Some("date"),
                    _ => field = None,
                }
                if field.is_some() {
                    text.clear();
                }
            }
            Ok(Event::Empty(e)) if in_item => {
                if e.local_name().as_ref() == "link"
                    && let Some(href) = attr(&e, "href")
                    && item.link.is_empty()
                {
                    item.link = href;
                }
            }
            Ok(Event::Text(t)) => {
                if field.is_some() {
                    text.push_str(&t.xml10_content());
                }
            }
            Ok(Event::CData(t)) => {
                if field.is_some() {
                    text.push_str(&t.xml10_content());
                }
            }
            Ok(Event::GeneralRef(r)) => {
                if field.is_some() {
                    if let Ok(Some(c)) = r.resolve_char_ref() {
                        text.push(c);
                    } else {
                        match r.into_inner().as_ref() {
                            "amp" => text.push('&'),
                            "lt" => text.push('<'),
                            "gt" => text.push('>'),
                            "quot" => text.push('"'),
                            "apos" => text.push('\''),
                            _ => {}
                        }
                    }
                }
            }
            Ok(Event::End(e)) => {
                depth = depth.saturating_sub(1);
                match (field.take(), e.local_name().as_ref()) {
                    (Some("title"), "title") => {
                        if in_item {
                            item.title = clean(&text);
                        } else {
                            feed.title = clean(&text);
                        }
                    }
                    (Some("link"), "link") => {
                        let link = text.trim();
                        if item.link.is_empty() {
                            item.link = link.to_string();
                        }
                    }
                    (Some("feedlink"), "link") => {
                        if feed.link.is_empty() {
                            feed.link = text.trim().to_string();
                        }
                    }
                    (Some("summary"), "description" | "summary" | "content") => {
                        item.summary = strip_html(&text)
                    }
                    (Some("feeddesc"), "description") => feed.description = strip_html(&text),
                    (Some("language"), "language") => feed.language = clean(&text),
                    (Some("date"), "pubDate" | "published" | "updated" | "date") => {
                        item.date = clean(&text)
                    }
                    _ => {}
                }
                if matches!(e.local_name().as_ref(), "item" | "entry") {
                    if !item.title.is_empty() || !item.link.is_empty() {
                        feed.items.push(std::mem::take(&mut item));
                    }
                    in_item = false;
                }
            }
            Ok(Event::Eof) | Err(_) => break,
            _ => {}
        }
        buf.clear();
        if depth > 128 {
            break;
        }
    }
    Some(feed)
}

fn attr(e: &quick_xml::events::BytesStart<'_>, name: &str) -> Option<String> {
    // 只认 rel="alternate"（或没有 rel）的链接，Atom 的 self/next 不算文章链接。
    let rel = e
        .try_get_attribute("rel")
        .ok()
        .flatten()
        .map(|a| a.value.to_string());
    if rel.is_some_and(|r| r != "alternate") {
        return None;
    }
    let value = e.try_get_attribute(name).ok().flatten()?;
    Some(value.value.replace("&amp;", "&"))
}

fn clean(s: &str) -> String {
    s.split_whitespace().collect::<Vec<_>>().join(" ")
}

fn escape_link_text(s: &str) -> String {
    s.replace('[', "\\[").replace(']', "\\]")
}

/// 绝对 http(s) 地址原样保留；相对地址按 `base` 补全；`javascript:`、`data:` 等一律丢弃。
fn safe_link(raw: &str, base: Option<&pagesift_net::Url>) -> Option<String> {
    let raw = raw.trim();
    if raw.is_empty() {
        return None;
    }
    let url = match pagesift_net::Url::parse(raw) {
        Ok(url) => url,
        Err(_) => base?.join(raw).ok()?,
    };
    matches!(url.scheme(), "http" | "https").then(|| url.to_string())
}

/// feed 的 description 常是转义后的 HTML。剥标签、丢掉脚本和样式的内容、还原常见实体，压成一行。
/// 结果按纯文本放进 Markdown，所以方括号要转义，免得拼出可点击的链接。
fn strip_html(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    let mut rest = s;
    while let Some(lt) = rest.find('<') {
        out.push_str(&rest[..lt]);
        let tag = &rest[lt..];
        let Some(gt) = tag.find('>') else {
            rest = "";
            break;
        };
        let name: String = tag[1..gt]
            .chars()
            .take_while(|c| c.is_ascii_alphanumeric())
            .collect::<String>()
            .to_ascii_lowercase();
        rest = &tag[gt + 1..];
        if name == "script" || name == "style" {
            let close = format!("</{name}");
            rest = match rest.to_ascii_lowercase().find(&close) {
                Some(i) => rest[i..].find('>').map_or("", |g| &rest[i + g + 1..]),
                None => "",
            };
        }
        out.push(' ');
    }
    out.push_str(rest);
    let out = out
        .replace("&nbsp;", " ")
        .replace("&quot;", "\"")
        .replace("&#39;", "'")
        .replace("&apos;", "'")
        .replace("&lt;", "<")
        .replace("&gt;", ">")
        .replace("&amp;", "&");
    escape_link_text(&clean(&out))
}

#[cfg(test)]
#[path = "tests_feed.rs"]
mod tests;
