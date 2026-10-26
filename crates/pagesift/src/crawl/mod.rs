//! 整站爬取和 map（PRD 6.6）：URL 规范化、爬取范围、sitemap 解析、广度优先遍历，
//! 以及带查询时的相关性优先遍历（见 [`crate::focus`]）。
//!
//! 限速、robots 和冷却全部走策略层，所以整站爬取和单页抓取一样守规矩。

use std::collections::HashSet;
use std::sync::Arc;
use std::time::Duration;

use pagesift_api::types::{CrawlRequest, Format, MapLink, MapRequest, ScrapeOptions};
use pagesift_api::{ApiError, CrawlSink, ErrorCode};
use pagesift_net::Url;
use pagesift_policy::{RequestKind, RobotsMode};
use regex::Regex;

use crate::engine::{Scraper, policy_error};
use crate::frontier::{self, Learner};

mod runner;
mod sitemap;

#[cfg(test)]
use sitemap::{gunzip_if_needed, parse_sitemap};
/// sitemapindex 最多展开几层、最多读多少个 sitemap 文件、单个文件解压后最大多少。
pub(super) const SITEMAP_MAX_DEPTH: usize = 3;
pub(super) const SITEMAP_MAX_FILES: usize = 50;
pub(super) const SITEMAP_MAX_BYTES: u64 = 50 * 1024 * 1024;
/// robots.txt 没有声明 sitemap 时依次尝试的位置（常见 CMS 的默认路径）。
pub(super) const SITEMAP_FALLBACKS: &[&str] =
    &["/sitemap.xml", "/sitemap_index.xml", "/wp-sitemap.xml"];

/// 临时性失败（超时、网络错误、5xx、429、主机冷却）最多重试几次（同 Scrapy 的 RETRY_TIMES）。
const MAX_RETRIES: u32 = 2;
/// 主机冷却或 Retry-After 超过这个时长就不等了，这一页按失败报告。
pub(super) const MAX_WAIT: Duration = Duration::from_secs(60);
/// 没有 Retry-After 时，第 n 次重试前等 RETRY_BASE × 2^(n-1)。
const RETRY_BASE: Duration = Duration::from_millis(500);

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

/// 爬取去重表里存的是去重键的 64 位哈希，不是 URL 本身：每条 8 字节，原来是一两百字节。
/// 一百万个 URL 里出现一次碰撞（误判为已见过而漏抓一页）的概率约 3×10⁻⁸。
pub(super) fn seen_hash(url: &Url, ignore_query: bool) -> u64 {
    use std::hash::{Hash, Hasher};
    // DefaultHasher::new() 的密钥固定，同一个键总是得到同一个哈希
    let mut h = std::hash::DefaultHasher::new();
    dedup_key(url, ignore_query).hash(&mut h);
    h.finish()
}

/// 爬取范围：哪些发现的 URL 可以进入队列。
pub(crate) struct Scope {
    host: String,
    allow_subdomains: bool,
    allow_external: bool,
    path_prefix: Option<String>,
    /// 起始 URL 是 `.../section.html` 时，`.../section/` 下的子页面也算在范围内。
    section_prefix: Option<String>,
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
        // 静态文档站（mdBook、Sphinx 等）把章节页放在 section.html，子页面放在 section/ 下
        let section_prefix = path_prefix.as_deref().and_then(|p| {
            let stem = p.strip_suffix(".html").or_else(|| p.strip_suffix(".htm"))?;
            (!stem.ends_with('/')).then(|| stem.to_string())
        });
        Ok(Self {
            host: start
                .host_str()
                .unwrap_or_default()
                .trim_start_matches("www.")
                .to_ascii_lowercase(),
            allow_subdomains,
            allow_external,
            path_prefix,
            section_prefix,
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
            && !self
                .section_prefix
                .as_ref()
                .is_some_and(|s| path.starts_with(&format!("{s}/")))
        {
            return false;
        }
        if !self.include.is_empty() && !self.include.iter().any(|r| r.is_match(path)) {
            return false;
        }
        !self.exclude.iter().any(|r| r.is_match(path))
    }
}

/// 页面上的链接（或 sitemap 里的 URL）解析、清洗之后，值得抓的才返回。
/// 先按固定规则清洗，再按这次爬取学到的改写规则改成规范写法。
pub(super) fn candidate(base: &Url, link: &str, learner: &Learner) -> Option<Url> {
    let mut u = base.join(link).ok()?;
    frontier::clean(&mut u);
    learner.rewrite(&mut u);
    (!frontier::skip(&u)).then_some(u)
}

/// 等着重试的页面：到点了才重新调度。
#[derive(Default)]
pub(super) struct Retries {
    items: Vec<(tokio::time::Instant, Url, usize, u32)>,
}

impl Retries {
    /// 第 `attempt` 次失败的结果值得重试就排进来，返回 true；否则返回 false，结果照常交付。
    pub(super) fn offer(
        &mut self,
        result: &Result<pagesift_api::types::Document, ApiError>,
        url: &Url,
        depth: usize,
        attempt: u32,
    ) -> bool {
        match retry_wait(result, attempt) {
            Some(wait) if attempt < MAX_RETRIES && wait <= MAX_WAIT => {
                self.items.push((
                    tokio::time::Instant::now() + wait,
                    url.clone(),
                    depth,
                    attempt + 1,
                ));
                true
            }
            _ => false,
        }
    }

    /// 已经到点的一个（最早的）。
    pub(super) fn due(&mut self) -> Option<(Url, usize, u32)> {
        let now = tokio::time::Instant::now();
        let i = (0..self.items.len())
            .filter(|&i| self.items[i].0 <= now)
            .min_by_key(|&i| self.items[i].0)?;
        let (_, u, d, a) = self.items.swap_remove(i);
        Some((u, d, a))
    }

    pub(super) fn is_empty(&self) -> bool {
        self.items.is_empty()
    }

    pub(super) fn next_due(&self) -> Option<tokio::time::Instant> {
        self.items.iter().map(|r| r.0).min()
    }
}

/// 这次结果值不值得重试，值得的话先等多久。
fn retry_wait(
    result: &Result<pagesift_api::types::Document, ApiError>,
    attempt: u32,
) -> Option<Duration> {
    let backoff = RETRY_BASE * 2u32.pow(attempt.min(6));
    match result {
        Ok(doc) => matches!(
            doc.metadata.status_code,
            408 | 429 | 500 | 502 | 503 | 504 | 522 | 524
        )
        .then_some(backoff),
        Err(e) => match e.code {
            ErrorCode::HostCooldown | ErrorCode::RateLimited => {
                Some(e.retry_after.map_or(backoff, |s| {
                    Duration::from_secs(s).max(Duration::from_millis(100))
                }))
            }
            ErrorCode::Timeout
            | ErrorCode::NetworkError
            | ErrorCode::UpstreamError
            | ErrorCode::Busy => Some(backoff),
            _ => None,
        },
    }
}

impl Scraper {
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
        for mut u in std::iter::once(start.clone()).chain(found) {
            frontier::clean(&mut u);
            if scope.allows(&u) && seen.insert(dedup_key(&u, req.ignore_query_parameters)) {
                links.push(normalize(&u, req.ignore_query_parameters));
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

    /// 整站爬取。深度按发现顺序计算：起始页和 sitemap 里的页面是 0。
    /// 带 `query` 时按相关性优先调度（见 [`crate::focus`]），否则广度优先。
    pub async fn crawl(
        &self,
        req: &CrawlRequest,
        sink: Arc<dyn CrawlSink>,
    ) -> Result<(), ApiError> {
        if req.adaptive.is_some() && req.query.is_none() {
            return Err(ApiError::invalid("adaptive crawling needs a query"));
        }
        if let Some(a) = &req.adaptive
            && !(a.coverage_threshold > 0.0
                && a.coverage_threshold <= 1.0
                && (0.0..=1.0).contains(&a.min_gain))
        {
            return Err(ApiError::invalid(
                "adaptive.coverageThreshold must be in (0, 1] and adaptive.minGain in [0, 1]",
            ));
        }
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
        let crawl = Crawl {
            req,
            sink: sink.as_ref(),
            scope: &scope,
            mode,
        };
        match req.query.as_deref() {
            Some(q) => self.crawl_focused(crawl, start, q).await,
            None => self.crawl_bfs(crawl, start).await,
        }
    }

    /// 新任务开始时 sitemap 里的页面（`sitemap: skip` 时为空）。从断点继续的任务不再读 sitemap。
    pub(super) async fn initial_sitemap(&self, c: &Crawl<'_>, start: &Url) -> Vec<Url> {
        if c.req.sitemap == "skip" {
            return Vec::new();
        }
        self.sitemap_urls(start, c.mode, c.req.limit.saturating_mul(2))
            .await
    }

    /// 需要链接来继续发现页面；用户没要 links 格式时，内部照样取，输出前去掉。
    /// 只爬一个主机时，同时进行的请求数超过每主机并发上限也只是排队，所以并发默认就取这个上限；
    /// 允许子域名或外链时取全局上限。调用方显式给的 maxConcurrency 照用。
    pub(super) fn crawl_settings(
        &self,
        req: &CrawlRequest,
        extra: &[&str],
    ) -> (Arc<ScrapeOptions>, usize) {
        let mut opts = req.scrape_options.clone();
        for kind in std::iter::once("links").chain(extra.iter().copied()) {
            if !opts.wants(kind) {
                opts.formats.push(Format::new(kind));
            }
        }
        // 允许子域名或外链时会同时访问多个主机：默认并发放到全局上限，每主机仍受各自的并发和间隔约束
        let rate = self.policy().limiter().config();
        let default = if req.allow_subdomains || req.allow_external_links {
            rate.global_concurrency
        } else {
            rate.per_host_concurrency
        };
        let concurrency = req.max_concurrency.unwrap_or(default).max(1);
        (Arc::new(opts), concurrency)
    }
}

/// 一次爬取在各阶段之间共享的参数。
pub(super) struct Crawl<'a> {
    pub(super) req: &'a CrawlRequest,
    pub(super) sink: &'a dyn CrawlSink,
    pub(super) scope: &'a Scope,
    pub(super) mode: RobotsMode,
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
    fn scope_from_a_section_page_covers_its_subdirectory() {
        let start = u("https://example.com/book/flow_control.html");
        let s = Scope::new(&start, false, false, false, &[], &[]).unwrap();
        assert!(s.allows(&start));
        assert!(s.allows(&u("https://example.com/book/flow_control/loop.html")));
        assert!(s.allows(&u("https://example.com/book/flow_control/loop/nested.html")));
        assert!(
            !s.allows(&u("https://example.com/book/flow_control_extra.html")),
            "sibling sharing the stem"
        );
        assert!(
            !s.allows(&u("https://example.com/book/types.html")),
            "sibling page"
        );
        let htm = Scope::new(
            &u("https://example.com/a/b.htm"),
            false,
            false,
            false,
            &[],
            &[],
        )
        .unwrap();
        assert!(htm.allows(&u("https://example.com/a/b/c.htm")));
        let plain = Scope::new(
            &u("https://example.com/docs/intro"),
            false,
            false,
            false,
            &[],
            &[],
        )
        .unwrap();
        assert!(
            !plain.allows(&u("https://example.com/docs/intro.html")),
            "no extension, no section rule"
        );
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
    fn parses_feeds_and_text_sitemaps() {
        let rss =
            br#"<?xml version="1.0"?><rss version="2.0"><channel><link>https://example.com/</link>
            <item><title>A</title><link>https://example.com/a?x=1&amp;y=2</link></item>
            <item><link><![CDATA[https://example.com/b]]></link></item></channel></rss>"#;
        assert_eq!(
            parse_sitemap(rss).0,
            vec!["https://example.com/a?x=1&y=2", "https://example.com/b"]
        );
        let atom = br#"<feed xmlns="http://www.w3.org/2005/Atom"><link href="https://example.com/"/>
            <entry><link rel="alternate" href="https://example.com/c"/><link rel="edit" href="https://example.com/edit/c"/></entry>
            <entry><link href="https://example.com/d"></link></entry></feed>"#;
        assert_eq!(
            parse_sitemap(atom).0,
            vec!["https://example.com/c", "https://example.com/d"]
        );
        let txt = b"\xef\xbb\xbfhttps://example.com/e\r\n\n  http://example.com/f  \nnot a url\n";
        assert_eq!(
            parse_sitemap(txt),
            (
                vec![
                    "https://example.com/e".to_string(),
                    "http://example.com/f".to_string()
                ],
                vec![]
            )
        );
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
