//! 整站爬取和 map（PRD 6.6）：URL 规范化、爬取范围、sitemap 解析、广度优先遍历，
//! 以及带查询时的相关性优先遍历（见 [`crate::focus`]）。
//!
//! 限速、robots 和冷却全部走策略层，所以整站爬取和单页抓取一样守规矩。

use std::collections::HashSet;
use std::sync::{Arc, OnceLock};
use std::time::Duration;

use pagesift_api::types::{
    CrawlRequest, CrawlScope, Document, Format, MapLink, MapRequest, ScrapeOptions,
};
use pagesift_api::{ApiError, CrawlSink, ErrorCode};
use pagesift_net::Url;
use pagesift_policy::{RequestKind, RobotsMode};
use regex::Regex;

use crate::engine::{Scraper, policy_error};
use crate::frontier::{self, Learner};

mod attachments;
mod runner;
mod sitemap;
#[cfg(test)]
mod tests_sitemap;

#[cfg(test)]
use sitemap::{gunzip_if_needed, parse_sitemap};

mod retry;
mod scope;
#[cfg(test)]
mod tests;

use retry::*;
pub use scope::*;

/// sitemapindex 最多展开几层、最多读多少个 sitemap 文件、单个文件解压后最大多少。
/// 读多少由预算决定（VOI 停止）：收集到的 URL 够爬取上限就不再读（多出来的 URL 轮不到抓，
/// 信息价值为 0，见 `sitemap_urls`）。下面三个是安全上限：
/// [策略] 协议只允许 sitemapindex 一层（sitemaps.org 0.9），宽容到三层防循环引用
pub(super) const SITEMAP_MAX_DEPTH: usize = 3;
/// [策略] 文件数上限：防一个站点用海量子 sitemap 拖住爬取的开头
pub(super) const SITEMAP_MAX_FILES: usize = 50;
/// [协议] sitemaps.org 0.9：单个 sitemap 解压后不超过 50 MB（52,428,800 字节），按它防解压炸弹
pub(super) const SITEMAP_MAX_BYTES: u64 = 50 * 1024 * 1024;
/// robots.txt 没有声明 sitemap 时依次尝试的位置（常见 CMS 的默认路径）。
pub(super) const SITEMAP_FALLBACKS: &[&str] =
    &["/sitemap.xml", "/sitemap_index.xml", "/wp-sitemap.xml"];

/// 临时性失败（超时、网络错误、5xx、429、主机冷却）最多重试几次。
/// [策略] 安全上限，同 Scrapy 的 RETRY_TIMES 2；上限之内要不要重试由失败类别的重试成功率决定
/// （[`Retries`]）
const MAX_RETRIES: u32 = 2;
/// 主机冷却或 Retry-After 超过这个时长就不等了，这一页按失败报告。
/// [策略] 一页最多为一次重试等 1 分钟（用户策略：爬取不为单页长时间挂起）
pub(super) const MAX_WAIT: Duration = Duration::from_secs(60);
/// 没有 Retry-After 时，第 n 次重试前等 RETRY_BASE × 2^(n-1)。
/// [策略] 先验包 v0：0.81 的现值（为什么不换成恢复时间分布见 [`Retries`]）
const RETRY_BASE: Duration = Duration::from_millis(500);
/// 种子页的重试次数和等待基数：种子页失败整个爬取就什么都没有了（站点一时超时，0.5 秒、1 秒后
/// 两次重试常常还在同一个波动里），多等几次、等久一点（2、4、8、16 秒）。
/// [策略] 先验包 v0：0.81 的现值；种子页的次数上限同样受重试成功率约束（[`Retries`]）
const SEED_MAX_RETRIES: u32 = 4;
/// [策略] 先验包 v0：0.81 的现值
const SEED_RETRY_BASE: Duration = Duration::from_secs(2);

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

pub(super) fn push_warning(doc: &mut Document, message: String) {
    doc.metadata
        .pagesift
        .get_or_insert_with(Default::default)
        .warnings
        .push(message);
}

/// 页面上的链接（或 sitemap 里的 URL）解析、清洗之后，值得抓的才返回。
/// 先按固定规则清洗，再按这次爬取学到的改写规则改成规范写法。
pub(super) fn candidate(base: &Url, link: &str, learner: &Learner) -> Option<Url> {
    let mut u = base.join(link).ok()?;
    // 页面是 https 拿到的，它链回同一站点的 http 地址（老模板）：直接用 https，省一次跳转，
    // 而且跟不了跳转的环境（只放行 https 的代理）也抓得到
    if u.scheme() == "http"
        && base.scheme() == "https"
        && u.host_str().zip(base.host_str()).is_some_and(|(a, b)| {
            a == b || pagesift_content::site_key(a) == pagesift_content::site_key(b)
        })
    {
        let _ = u.set_scheme("https");
    }
    frontier::clean(&mut u);
    learner.rewrite(&mut u);
    (!frontier::skip(&u)).then_some(u)
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
        self.audit_robots_ignored(RequestKind::Map, mode, &req.url);
        let scope = Scope::new(
            &start,
            req.include_subdomains,
            false,
            CrawlScope::Domain,
            &[],
            &[],
        )?;
        let mut found: Vec<Url> = Vec::new();
        if req.sitemap != "skip" {
            found.extend(
                self.sitemap_urls(&start, mode, req.limit.saturating_mul(2))
                    .await,
            );
        }
        let mut root_error = None;
        if req.sitemap != "only" {
            // map 只要链接，不需要渲染
            let opts = ScrapeOptions {
                formats: vec![Format::new("links")],
                engine: Some("http".into()),
                ..Default::default()
            };
            match self.scrape_with(start.as_str(), &opts, mode).await {
                Ok(doc) => found.extend(
                    doc.links
                        .unwrap_or_default()
                        .iter()
                        .filter_map(|l| Url::parse(l).ok()),
                ),
                Err(error) => root_error = Some(error),
            }
        }
        // 种子页失败且 sitemap 也没给出任何 URL 时，不要返回“只有种子”的假成功。
        if found.is_empty()
            && let Some(error) = root_error
        {
            return Err(error);
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
        self.audit_robots_ignored(RequestKind::Crawl, mode, &req.url);
        if req.attachments && req.query.is_none() {
            return Err(ApiError::invalid("attachments need a query"));
        }
        let scope = Scope::new(
            &start,
            allow_subdomains(req),
            req.allow_external_links,
            if req.crawl_entire_domain {
                CrawlScope::Domain
            } else {
                req.scope.unwrap_or(if req.query.is_some() {
                    CrawlScope::Domain
                } else {
                    CrawlScope::Auto
                })
            },
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
        if let Some(req_graph) = &req.graph
            && (opts.graph.is_none() || req_graph.enabled)
        {
            opts.graph = Some(req_graph.clone());
        }
        if opts.image_assets.is_some() && opts.asset_fetch_cache.is_none() {
            opts.asset_fetch_cache = Some(Default::default());
        }
        for kind in std::iter::once("links").chain(extra.iter().copied()) {
            if !opts.wants(kind) {
                opts.formats.push(Format::new(kind));
            }
        }
        // 允许子域名或外链时会同时访问多个主机：默认并发放到全局上限，每主机仍受各自的并发和间隔约束
        let rate = self.policy().limiter().config();
        let default = if allow_subdomains(req) || req.allow_external_links {
            rate.global_concurrency
        } else {
            rate.per_host_concurrency
        };
        let concurrency = req.max_concurrency.unwrap_or(default).max(1);
        (Arc::new(opts), concurrency)
    }
}

/// 要不要进同站点的其他子域名：没明说时，带查询的爬取要（国际生服务、招生常在子域上），广度优先不要。
pub fn allow_subdomains(req: &CrawlRequest) -> bool {
    req.allow_subdomains.unwrap_or(req.query.is_some())
}

/// 一次爬取在各阶段之间共享的参数。
pub(super) struct Crawl<'a> {
    pub(super) req: &'a CrawlRequest,
    pub(super) sink: &'a dyn CrawlSink,
    pub(super) scope: &'a Scope,
    pub(super) mode: RobotsMode,
}
