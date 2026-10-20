//! 整站爬取和 map（PRD 6.6）：URL 规范化、爬取范围、sitemap 解析、广度优先遍历。
//!
//! 限速、robots 和冷却全部走策略层，所以整站爬取和单页抓取一样守规矩。

use std::collections::{HashSet, VecDeque};
use std::io::Read;
use std::sync::Arc;
use std::time::Duration;

use futures_util::StreamExt;
use futures_util::stream::FuturesUnordered;
use pagesift_api::types::{CrawlRequest, Format, MapLink, MapRequest, ScrapeOptions};
use pagesift_api::{ApiError, CrawlSink, ErrorCode};
use pagesift_net::{FetchRequest, Url};
use pagesift_policy::{RequestKind, RobotsMode};
use regex::Regex;

use crate::engine::{Scraper, policy_error};

/// sitemapindex 最多展开几层、最多读多少个 sitemap 文件、单个文件解压后最大多少。
const SITEMAP_MAX_DEPTH: usize = 3;
const SITEMAP_MAX_FILES: usize = 50;
const SITEMAP_MAX_BYTES: u64 = 50 * 1024 * 1024;
const DEFAULT_CRAWL_CONCURRENCY: usize = 10;

/// URL 规范化（F-6.2），用来去重：去掉 fragment；查询参数排序，或者整个去掉。
/// 协议和主机的小写、默认端口、`..` 路径由 URL 解析本身处理。
pub fn normalize(url: &Url, ignore_query: bool) -> String {
    let mut u = url.clone();
    u.set_fragment(None);
    if ignore_query {
        u.set_query(None);
    } else if u.query().is_some() {
        let mut pairs: Vec<(String, String)> = u
            .query_pairs()
            .map(|(k, v)| (k.into_owned(), v.into_owned()))
            .collect();
        pairs.sort();
        u.query_pairs_mut().clear().extend_pairs(pairs);
    }
    u.to_string()
}

/// 去重用的键：在 [`normalize`] 的基础上把 `www.` 视为同一主机，避免同一页面因写法不同被抓两次。
fn dedup_key(url: &Url, ignore_query: bool) -> String {
    normalize(url, ignore_query).replacen("://www.", "://", 1)
}

/// 爬取范围：哪些发现的 URL 可以进入队列。
pub(crate) struct Scope {
    host: String,
    allow_subdomains: bool,
    allow_external: bool,
    path_prefix: Option<String>,
    include: Vec<Regex>,
    exclude: Vec<Regex>,
}

impl Scope {
    fn new(
        start: &Url,
        allow_subdomains: bool,
        allow_external: bool,
        entire_domain: bool,
        include: &[String],
        exclude: &[String],
    ) -> Result<Self, ApiError> {
        let compile = |ps: &[String]| -> Result<Vec<Regex>, ApiError> {
            ps.iter()
                .map(|p| {
                    Regex::new(p)
                        .map_err(|e| ApiError::invalid(format!("invalid path pattern {p:?}: {e}")))
                })
                .collect()
        };
        // 默认只爬起始路径下面的页面（和 Firecrawl 一致），crawlEntireDomain 时放开到整站
        let path_prefix = (!entire_domain)
            .then(|| start.path().trim_end_matches('/').to_string())
            .filter(|p| !p.is_empty());
        Ok(Self {
            host: start
                .host_str()
                .unwrap_or_default()
                .trim_start_matches("www.")
                .to_ascii_lowercase(),
            allow_subdomains,
            allow_external,
            path_prefix,
            include: compile(include)?,
            exclude: compile(exclude)?,
        })
    }

    pub(crate) fn allows(&self, u: &Url) -> bool {
        if !matches!(u.scheme(), "http" | "https") {
            return false;
        }
        let host = u.host_str().unwrap_or_default().to_ascii_lowercase();
        let bare = host.trim_start_matches("www.");
        let same = bare == self.host;
        let sub = host.ends_with(&format!(".{}", self.host));
        if !(same || (self.allow_subdomains && sub) || self.allow_external) {
            return false;
        }
        let path = u.path();
        if same
            && let Some(prefix) = &self.path_prefix
            && !(path == prefix || path.starts_with(&format!("{prefix}/")))
        {
            return false;
        }
        if !self.include.is_empty() && !self.include.iter().any(|r| r.is_match(path)) {
            return false;
        }
        !self.exclude.iter().any(|r| r.is_match(path))
    }
}

impl Scraper {
    /// 从 robots.txt 声明的 sitemap（没有时试 `/sitemap.xml`）收集页面 URL，展开 sitemapindex，支持 gzip。
    async fn sitemap_urls(&self, start: &Url, mode: RobotsMode, limit: usize) -> Vec<Url> {
        let mut queue: VecDeque<(Url, usize)> = VecDeque::new();
        if let Ok(robots) = self.policy().robots_cache().get(start).await {
            for s in robots.sitemaps() {
                if let Ok(u) = Url::parse(s) {
                    queue.push_back((u, 0));
                }
            }
        }
        if queue.is_empty()
            && let Ok(u) = start.join("/sitemap.xml")
        {
            queue.push_back((u, 0));
        }
        let mut seen_files = HashSet::new();
        let mut pages = Vec::new();
        while let Some((url, depth)) = queue.pop_front() {
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

    pub async fn map(&self, req: &MapRequest) -> Result<Vec<MapLink>, ApiError> {
        let work = self.map_inner(req);
        match req.timeout {
            Some(ms) => tokio::time::timeout(Duration::from_millis(ms), work)
                .await
                .map_err(|_| ApiError::new(ErrorCode::Timeout, "map timed out"))?,
            None => work.await,
        }
    }

    async fn map_inner(&self, req: &MapRequest) -> Result<Vec<MapLink>, ApiError> {
        let start = Url::parse(&req.url).map_err(|e| ApiError::invalid(e.to_string()))?;
        let mode = self
            .policy()
            .robots_mode(RequestKind::Map, req.ignore_robots_txt)
            .map_err(policy_error)?;
        let scope = Scope::new(&start, req.include_subdomains, false, true, &[], &[])?;
        let mut found: Vec<Url> = Vec::new();
        if req.sitemap != "skip" {
            found.extend(
                self.sitemap_urls(&start, mode, req.limit.saturating_mul(2))
                    .await,
            );
        }
        if req.sitemap != "only" {
            // map 只要链接，不需要渲染
            let opts = ScrapeOptions {
                formats: vec![Format::new("links")],
                engine: Some("http".into()),
                ..Default::default()
            };
            if let Ok(doc) = self.scrape_with(start.as_str(), &opts, mode).await {
                found.extend(
                    doc.links
                        .unwrap_or_default()
                        .iter()
                        .filter_map(|l| Url::parse(l).ok()),
                );
            }
        }
        let mut seen = HashSet::new();
        let mut links: Vec<String> = Vec::new();
        for u in std::iter::once(start.clone()).chain(found) {
            if scope.allows(&u) {
                if seen.insert(dedup_key(&u, req.ignore_query_parameters)) {
                    links.push(normalize(&u, req.ignore_query_parameters));
                }
            }
        }
        if let Some(search) = req.search.as_deref().filter(|s| !s.trim().is_empty()) {
            // 按命中的搜索词个数排序，命中多的在前；没命中的保留在后面
            let terms: Vec<String> = search.split_whitespace().map(str::to_lowercase).collect();
            let hits = |u: &str| {
                let l = u.to_lowercase();
                terms.iter().filter(|t| l.contains(t.as_str())).count()
            };
            links.sort_by_key(|u| std::cmp::Reverse(hits(u)));
        }
        links.truncate(req.limit);
        Ok(links
            .into_iter()
            .map(|url| MapLink {
                url,
                title: None,
                description: None,
            })
            .collect())
    }

    /// 广度优先爬取。深度按发现顺序计算：起始页和 sitemap 里的页面是 0。
    pub async fn crawl(
        &self,
        req: &CrawlRequest,
        sink: Arc<dyn CrawlSink>,
    ) -> Result<(), ApiError> {
        let start = Url::parse(&req.url).map_err(|e| ApiError::invalid(e.to_string()))?;
        let mode = self
            .policy()
            .robots_mode(RequestKind::Crawl, req.ignore_robots_txt)
            .map_err(policy_error)?;
        let scope = Scope::new(
            &start,
            req.allow_subdomains,
            req.allow_external_links,
            req.crawl_entire_domain,
            &req.include_paths,
            &req.exclude_paths,
        )?;
        let follow_links = req.sitemap != "only";
        let max_depth = req.max_discovery_depth.unwrap_or(usize::MAX);

        let mut seen: HashSet<String> = HashSet::new();
        let mut queue: VecDeque<(Url, usize)> = VecDeque::new();
        seen.insert(dedup_key(&start, req.ignore_query_parameters));
        queue.push_back((start.clone(), 0));
        if req.sitemap != "skip" {
            for u in self
                .sitemap_urls(&start, mode, req.limit.saturating_mul(2))
                .await
            {
                if scope.allows(&u) && seen.insert(dedup_key(&u, req.ignore_query_parameters)) {
                    queue.push_back((u, 0));
                }
            }
        }

        // 需要链接来继续发现页面；用户没要 links 格式时，内部照样取，输出前去掉
        let wants_links = req.scrape_options.wants("links");
        let mut opts = req.scrape_options.clone();
        if !wants_links {
            opts.formats.push(Format::new("links"));
        }
        let opts = Arc::new(opts);
        let concurrency = req
            .max_concurrency
            .unwrap_or(DEFAULT_CRAWL_CONCURRENCY)
            .max(1);
        let delay = req.delay.map(Duration::from_secs_f64);

        let mut scheduled = 0usize;
        let mut inflight = FuturesUnordered::new();
        sink.total(queue.len().min(req.limit)).await;
        loop {
            if sink.cancelled().await {
                return Ok(());
            }
            while inflight.len() < concurrency && scheduled < req.limit {
                let Some((url, depth)) = queue.pop_front() else {
                    break;
                };
                scheduled += 1;
                let opts = opts.clone();
                inflight.push(async move {
                    let result = self.scrape_with(url.as_str(), &opts, mode).await;
                    (url, depth, result)
                });
                if let Some(d) = delay {
                    tokio::time::sleep(d).await;
                }
            }
            let Some((url, depth, result)) = inflight.next().await else {
                break;
            };
            match result {
                Ok(mut doc) => {
                    if follow_links && depth < max_depth {
                        let base = doc
                            .metadata
                            .url
                            .as_deref()
                            .and_then(|u| Url::parse(u).ok())
                            .unwrap_or(url.clone());
                        for link in doc.links.iter().flatten() {
                            let Ok(u) = base.join(link) else { continue };
                            if scope.allows(&u)
                                && seen.insert(dedup_key(&u, req.ignore_query_parameters))
                            {
                                queue.push_back((u, depth + 1));
                            }
                        }
                    }
                    if !wants_links {
                        doc.links = None;
                    }
                    sink.page(doc).await;
                }
                Err(e) => sink.error(url.as_str(), e).await,
            }
            sink.total((scheduled + queue.len()).min(req.limit)).await;
        }
        Ok(())
    }
}

fn gunzip_if_needed(body: Vec<u8>) -> Option<Vec<u8>> {
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

/// 解析 sitemap：`<urlset>` 里的 `<loc>` 是页面，`<sitemapindex>` 里的 `<loc>` 是子 sitemap。
/// quick-xml 把文本里的实体（`&amp;` 等）单独作为事件返回，这里累积 `<loc>` 内的各段再还原。
fn parse_sitemap(xml: &[u8]) -> (Vec<String>, Vec<String>) {
    use quick_xml::events::Event;
    let mut reader = quick_xml::Reader::from_reader(xml);
    let (mut pages, mut children) = (Vec::new(), Vec::new());
    let mut in_sitemap = false;
    let mut loc: Option<String> = None;
    let mut buf = Vec::new();
    loop {
        match reader.read_event_into(&mut buf) {
            Ok(Event::Start(e)) => match e.local_name().as_ref() {
                "sitemap" => in_sitemap = true,
                "loc" => loc = Some(String::new()),
                _ => {}
            },
            Ok(Event::End(e)) => match e.local_name().as_ref() {
                "sitemap" => in_sitemap = false,
                "loc" => {
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
            },
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

#[cfg(test)]
mod tests {
    use super::*;

    fn u(s: &str) -> Url {
        Url::parse(s).unwrap()
    }

    #[test]
    fn normalizes_for_dedup() {
        assert_eq!(
            normalize(&u("HTTPS://Example.COM:443/a/../b?z=1&a=2#frag"), false),
            "https://example.com/b?a=2&z=1"
        );
        assert_eq!(
            normalize(&u("https://example.com/b?z=1"), true),
            "https://example.com/b"
        );
    }

    #[test]
    fn www_variants_dedup_to_one_key() {
        assert_eq!(
            dedup_key(&u("https://www.example.com/a"), false),
            dedup_key(&u("https://example.com/a"), false)
        );
        assert_ne!(
            dedup_key(&u("https://wwwexample.com/a"), false),
            dedup_key(&u("https://example.com/a"), false)
        );
    }

    #[test]
    fn scope_rules() {
        let start = u("https://www.example.com/docs/");
        let s = Scope::new(&start, false, false, false, &[], &["/docs/private".into()]).unwrap();
        assert!(s.allows(&u("https://example.com/docs/intro")));
        assert!(s.allows(&u("https://www.example.com/docs")));
        assert!(
            !s.allows(&u("https://example.com/blog/x")),
            "outside the start path"
        );
        assert!(!s.allows(&u("https://api.example.com/docs/x")), "subdomain");
        assert!(!s.allows(&u("https://other.com/docs/x")), "external");
        assert!(
            !s.allows(&u("https://example.com/docs/private/x")),
            "excluded"
        );
        assert!(!s.allows(&u("mailto:a@example.com")));
        let wide = Scope::new(&start, true, false, true, &["^/docs/".into()], &[]).unwrap();
        assert!(wide.allows(&u("https://api.example.com/docs/x")));
        assert!(
            !wide.allows(&u("https://example.com/blog/x")),
            "include pattern"
        );
        assert!(Scope::new(&start, false, false, false, &["(".into()], &[]).is_err());
    }

    #[test]
    fn parses_urlsets_and_indexes() {
        let urlset =
            br#"<?xml version="1.0"?><urlset xmlns="http://www.sitemaps.org/schemas/sitemap/0.9">
            <url><loc>https://example.com/a</loc><lastmod>2026-01-01</lastmod></url>
            <url><loc><![CDATA[https://example.com/b?x=1&y=2]]></loc></url>
            <url><loc>https://example.com/c?p=1&amp;q=&#50;</loc></url></urlset>"#;
        let (pages, children) = parse_sitemap(urlset);
        assert_eq!(
            pages,
            vec![
                "https://example.com/a",
                "https://example.com/b?x=1&y=2",
                "https://example.com/c?p=1&q=2"
            ]
        );
        assert!(children.is_empty());
        let index = br#"<sitemapindex><sitemap><loc>https://example.com/s1.xml.gz</loc></sitemap></sitemapindex>"#;
        assert_eq!(
            parse_sitemap(index),
            (vec![], vec!["https://example.com/s1.xml.gz".to_string()])
        );
        assert_eq!(parse_sitemap(b"not xml <<<"), (vec![], vec![]));
    }

    #[test]
    fn gunzips_sitemaps() {
        use std::io::Write;
        let mut enc = flate2::write::GzEncoder::new(Vec::new(), flate2::Compression::default());
        enc.write_all(b"<urlset><url><loc>https://example.com/z</loc></url></urlset>")
            .unwrap();
        let xml = gunzip_if_needed(enc.finish().unwrap()).unwrap();
        assert_eq!(parse_sitemap(&xml).0, vec!["https://example.com/z"]);
    }
}
