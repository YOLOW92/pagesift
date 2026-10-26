use std::collections::{HashSet, VecDeque};
use std::io::Read;

use pagesift_net::{FetchRequest, Url};
use pagesift_policy::RobotsMode;

use crate::engine::Scraper;

use super::{SITEMAP_FALLBACKS, SITEMAP_MAX_BYTES, SITEMAP_MAX_DEPTH, SITEMAP_MAX_FILES};

impl Scraper {
    /// 从 robots.txt 声明的 sitemap（没有时试 `/sitemap.xml`）收集页面 URL，展开 sitemapindex，支持 gzip。
    pub(super) async fn sitemap_urls(
        &self,
        start: &Url,
        mode: RobotsMode,
        limit: usize,
    ) -> Vec<Url> {
        let mut queue: VecDeque<(Url, usize)> = VecDeque::new();
        if let Ok(robots) = self.policy().robots_cache().get(start).await {
            for s in robots.sitemaps() {
                if let Ok(u) = Url::parse(s) {
                    queue.push_back((u, 0));
                }
            }
        }
        // robots.txt 没声明时依次试常见位置，直到有一个给出页面
        let mut fallbacks: VecDeque<Url> = if queue.is_empty() {
            SITEMAP_FALLBACKS
                .iter()
                .filter_map(|p| start.join(p).ok())
                .collect()
        } else {
            VecDeque::new()
        };
        let mut seen_files = HashSet::new();
        let mut pages = Vec::new();
        loop {
            if queue.is_empty()
                && pages.is_empty()
                && let Some(u) = fallbacks.pop_front()
            {
                queue.push_back((u, 0));
            }
            let Some((url, depth)) = queue.pop_front() else {
                break;
            };
            if seen_files.len() >= SITEMAP_MAX_FILES
                || pages.len() >= limit
                || !seen_files.insert(url.to_string())
            {
                continue;
            }
            let Ok(resp) = self
                .policy()
                .fetch(FetchRequest::new(url.as_str()), mode)
                .await
            else {
                continue;
            };
            if resp.status >= 400 {
                continue;
            }
            let Some(xml) = gunzip_if_needed(resp.body) else {
                continue;
            };
            let (locs, children) = parse_sitemap(&xml);
            for loc in locs {
                if pages.len() >= limit {
                    break;
                }
                if let Ok(u) = Url::parse(loc.trim()) {
                    pages.push(u);
                }
            }
            if depth < SITEMAP_MAX_DEPTH {
                for c in children {
                    if let Ok(u) = Url::parse(c.trim()) {
                        queue.push_back((u, depth + 1));
                    }
                }
            }
        }
        pages
    }
}

pub(super) fn gunzip_if_needed(body: Vec<u8>) -> Option<Vec<u8>> {
    if !body.starts_with(&[0x1f, 0x8b]) {
        return Some(body);
    }
    let mut out = Vec::new();
    flate2::read::GzDecoder::new(&body[..])
        .take(SITEMAP_MAX_BYTES)
        .read_to_end(&mut out)
        .ok()?;
    Some(out)
}

/// 解析 sitemap（sitemaps.org 协议允许的三种格式）：
/// - XML：`<urlset>` 里的 `<loc>` 是页面，`<sitemapindex>` 里的 `<loc>` 是子 sitemap；
///   只认 `<url>`/`<sitemap>`（或根元素）下一层的 `<loc>`，图片扩展里同名的 `<image:loc>` 不算（同 Scrapy）；
/// - RSS 2.0 / Atom feed：`<item>` 的 `<link>` 文本、`<entry>` 的 `<link href>` 是页面；
/// - 纯文本：每行一个 URL。
///
/// quick-xml 把文本里的实体（`&amp;` 等）单独作为事件返回，这里累积 `<loc>` 内的各段再还原。
pub(super) fn parse_sitemap(xml: &[u8]) -> (Vec<String>, Vec<String>) {
    use quick_xml::events::Event;
    let body = xml.strip_prefix(b"\xef\xbb\xbf").unwrap_or(xml);
    if !body.trim_ascii_start().starts_with(b"<") {
        let pages = String::from_utf8_lossy(body)
            .lines()
            .map(str::trim)
            .filter(|l| l.starts_with("http://") || l.starts_with("https://"))
            .map(str::to_string)
            .collect();
        return (pages, Vec::new());
    }
    let mut reader = quick_xml::Reader::from_reader(xml);
    let (mut pages, mut children) = (Vec::new(), Vec::new());
    let mut in_sitemap = false;
    // feed 的条目（RSS 的 item、Atom 的 entry）里
    let mut in_item = false;
    let atom_href = |e: &quick_xml::events::BytesStart| -> Option<String> {
        let rel = e
            .try_get_attribute("rel")
            .ok()
            .flatten()
            .map(|a| a.value.to_string());
        if rel.is_some_and(|r| r != "alternate") {
            return None;
        }
        let href = e.try_get_attribute("href").ok().flatten()?;
        Some(href.value.replace("&amp;", "&"))
    };
    let mut loc: Option<String> = None;
    // 打开着的元素的本地名，用来判断 <loc> 的上一层
    let mut open: Vec<String> = Vec::new();
    let mut buf = Vec::new();
    loop {
        match reader.read_event_into(&mut buf) {
            Ok(Event::Start(e)) => {
                let name = e.local_name();
                match name.as_ref() {
                    "sitemap" => in_sitemap = true,
                    "item" | "entry" => in_item = true,
                    "loc" if matches!(
                        open.last().map(String::as_str),
                        Some("url" | "sitemap" | "urlset" | "sitemapindex")
                    ) =>
                    {
                        loc = Some(String::new())
                    }
                    "link" if in_item => match atom_href(&e) {
                        Some(href) => pages.push(href),
                        None => loc = Some(String::new()),
                    },
                    _ => {}
                }
                open.push(name.as_ref().to_owned());
            }
            Ok(Event::Empty(e)) if in_item && e.local_name().as_ref() == "link" => {
                pages.extend(atom_href(&e));
            }
            Ok(Event::End(e)) => {
                open.pop();
                match e.local_name().as_ref() {
                    "sitemap" => in_sitemap = false,
                    "item" | "entry" => in_item = false,
                    "loc" | "link" => {
                        if let Some(text) = loc
                            .take()
                            .map(|t| t.trim().to_string())
                            .filter(|t| !t.is_empty())
                        {
                            if in_sitemap {
                                children.push(text)
                            } else {
                                pages.push(text)
                            }
                        }
                    }
                    _ => {}
                }
            }
            Ok(Event::Text(t)) => {
                if let Some(l) = loc.as_mut() {
                    l.push_str(&t.xml10_content());
                }
            }
            Ok(Event::CData(t)) => {
                if let Some(l) = loc.as_mut() {
                    l.push_str(&t.xml10_content());
                }
            }
            Ok(Event::GeneralRef(r)) => {
                if let Some(l) = loc.as_mut() {
                    if let Ok(Some(c)) = r.resolve_char_ref() {
                        l.push(c);
                    } else {
                        match r.into_inner().as_ref() {
                            "amp" => l.push('&'),
                            "lt" => l.push('<'),
                            "gt" => l.push('>'),
                            "quot" => l.push('"'),
                            "apos" => l.push('\''),
                            _ => {}
                        }
                    }
                }
            }
            Ok(Event::Eof) | Err(_) => break,
            _ => {}
        }
        buf.clear();
    }
    (pages, children)
}
