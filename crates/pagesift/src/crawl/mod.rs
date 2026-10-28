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
/// 种子页的重试次数和等待基数：种子页失败整个爬取就什么都没有了（站点一时超时，0.5 秒、1 秒后
/// 两次重试常常还在同一个波动里），多等几次、等久一点（2、4、8、16 秒）。
const SEED_MAX_RETRIES: u32 = 4;
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

/// 爬取范围的路径前缀：目录部分。
///
/// - `/` → 无前缀（整站）
/// - `/docs/` → `/docs`
/// - `/docs/index.html` → `/docs`
/// - `/docs/python` → `/docs/python`（不像文件名的路径按它自己的前缀，和 Firecrawl 一致；
///   0.71–0.72 曾把它放大到 `/docs`，连 `/docs/java` 也爬进来）
/// - `/docs` → `/docs`（单段路径保留，避免变成整站）
fn scope_path(start: &Url) -> Option<String> {
    let path = start.path();
    let trimmed = path.trim_end_matches('/');
    if trimmed.is_empty() {
        return None;
    }
    if path.ends_with('/') {
        return Some(trimmed.to_string());
    }
    match trimmed.rsplit_once('/') {
        Some(("", last)) => {
            // 根目录下的单段路径：`/index.html` 这种文件名按整站处理；
            // `/docs` 这种栏目根保留自身前缀，避免把范围意外放大到整站。
            (!last.contains('.')).then(|| trimmed.to_string())
        }
        None => Some(trimmed.to_string()),
        // 只有文件名（`index.html`、`page.php`）才换成所在目录
        Some((parent, last)) if last.contains('.') => Some(parent.to_string()),
        Some(_) => Some(trimmed.to_string()),
    }
}

pub(super) fn push_warning(doc: &mut Document, message: String) {
    doc.metadata
        .pagesift
        .get_or_insert_with(Default::default)
        .warnings
        .push(message);
}

/// 爬取范围：哪些发现的 URL 可以进入队列。
pub(crate) struct Scope {
    host: String,
    allow_subdomains: bool,
    allow_external: bool,
    path_prefix: Option<String>,
    /// `auto` 模式下可以放宽到的祖先目录，由近到远，不含整站根；空表示不放宽。
    ancestors: Vec<String>,
    /// 种子页的证据表明它是叶子页之后放宽到其中一个祖先目录，只设一次。
    widened: OnceLock<String>,
    include: Vec<Regex>,
    exclude: Vec<Regex>,
}

impl Scope {
    fn new(
        start: &Url,
        allow_subdomains: bool,
        allow_external: bool,
        mode: CrawlScope,
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
        // 先取最窄的合理范围：种子以文件名结尾时（.../index.html）取所在目录，否则把种子路径本身
        // 当目录。`auto` 再看种子页的链接决定要不要放宽到某个祖先目录（见 `widen_from_seed`），
        // 永远不会自动放宽到整站根。
        let narrow = (mode != CrawlScope::Domain)
            .then(|| scope_path(start))
            .flatten()
            .filter(|p| !p.is_empty());
        let mut ancestors = Vec::new();
        if let Some(mut dir) = narrow.as_deref().filter(|_| seed_names_page(start)) {
            while let Some((up, _)) = dir.rsplit_once('/').filter(|(up, _)| !up.is_empty()) {
                ancestors.push(up.to_string());
                dir = up;
            }
        }
        let (path_prefix, ancestors) = match mode {
            CrawlScope::Domain => (None, Vec::new()),
            CrawlScope::Exact => (narrow, Vec::new()),
            CrawlScope::Auto => (narrow, ancestors),
            // 按日期分目录的文章（`/blog/2026/07/21/post/`）的上一级是年月日目录，没有意义：
            // 跳过日期目录取所在栏目；栏目就是整站根时爬整个主机
            CrawlScope::Parent if !ancestors.is_empty() => (
                ancestors.into_iter().find(|dir| !is_date_dir(dir)),
                Vec::new(),
            ),
            CrawlScope::Parent => (narrow, Vec::new()),
        };
        Ok(Self {
            host: start
                .host_str()
                .unwrap_or_default()
                .trim_start_matches("www.")
                .to_ascii_lowercase(),
            allow_subdomains,
            allow_external,
            path_prefix,
            ancestors,
            widened: OnceLock::new(),
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
        // 同一站点的任何子域名（可注册域名相同）：`iss.example.edu` 和 `www.example.edu`，
        // 不只是起点主机的后代
        let sub = pagesift_content::site_key(bare) == pagesift_content::site_key(&self.host);
        if !(same || (self.allow_subdomains && sub) || self.allow_external) {
            return false;
        }
        let path = u.path();
        if same
            && let Some(prefix) = self.widened.get().or(self.path_prefix.as_ref())
            && !under(path, prefix)
        {
            return false;
        }
        if !self.include.is_empty() && !self.include.iter().any(|r| r.is_match(path)) {
            return false;
        }
        !self.exclude.iter().any(|r| r.is_match(path))
    }

    /// 还能不能按种子页的证据放宽（`auto` 且还没放宽过）。
    pub(super) fn can_widen(&self) -> bool {
        !self.ancestors.is_empty() && self.widened.get().is_none()
    }

    /// 在当前范围外、但放宽后可能进入范围的 URL（同主机、在最远的祖先目录下，且通过 include/exclude）。
    /// 已经放宽过或不能放宽时恒为 false。
    pub(super) fn may_widen_to(&self, u: &Url) -> bool {
        let (Some(narrow), Some(top)) = (&self.path_prefix, self.ancestors.last()) else {
            return false;
        };
        if self.widened.get().is_some() || self.allows(u) {
            return false;
        }
        let host = u.host_str().unwrap_or_default();
        let path = u.path();
        host.trim_start_matches("www.")
            .eq_ignore_ascii_case(&self.host)
            && under(path, top)
            && !under(path, narrow)
            && (self.include.is_empty() || self.include.iter().any(|r| r.is_match(path)))
            && !self.exclude.iter().any(|r| r.is_match(path))
    }

    /// `auto`：用种子页的链接判断种子是不是叶子页。子页链接很少（< [`WIDEN_MAX_CHILDREN`]）时，
    /// 由近到远找第一个有足够同级链接（≥ [`WIDEN_MIN_SIBLINGS`]）的祖先目录放宽过去，返回说明原因的警告。
    /// 博客文章、新闻报道、文档里的一页都是这种形状；目录页、分类页有自己的子页，不放宽。
    /// 按日期分目录的博客（`/blog/2026/07/21/post/`）同一天往往没有别的文章，
    /// 所以不只看上一级：同级文章分散在 `/blog/2026/…` 下时放宽到最近的那一层。
    /// 种子页自己要有正文（去掉链接后至少 [`WIDEN_MIN_PROSE`] 个字母/数字）：只列链接的空分类页、
    /// 导航页不算叶子内容页。
    pub(super) fn widen_from_seed<'a>(
        &self,
        seed: &Url,
        markdown: &str,
        links: impl IntoIterator<Item = &'a Url>,
    ) -> Option<String> {
        let Some(narrow) = &self.path_prefix else {
            return None;
        };
        if self.ancestors.is_empty() || prose_chars(markdown) < WIDEN_MIN_PROSE {
            return None;
        }
        let seed_path = seed.path().trim_end_matches('/');
        let mut children: HashSet<&str> = HashSet::new();
        let mut outside: HashSet<&str> = HashSet::new();
        for u in links {
            let host = u.host_str().unwrap_or_default();
            if !host
                .trim_start_matches("www.")
                .eq_ignore_ascii_case(&self.host)
            {
                continue;
            }
            let path = u.path().trim_end_matches('/');
            if path == seed_path {
                continue;
            }
            if under(path, narrow) {
                children.insert(path);
            } else {
                outside.insert(path);
            }
        }
        if children.len() >= WIDEN_MAX_CHILDREN {
            return None;
        }
        // 祖先目录本身和它的首页是“上一级”链接（面包屑），不算同级
        let (dir, siblings) = self.ancestors.iter().find_map(|dir| {
            let n = outside
                .iter()
                .filter(|p| under(p, dir) && **p != dir.as_str() && !is_index_of(p, dir))
                .count();
            (n >= WIDEN_MIN_SIBLINGS).then_some((dir, n))
        })?;
        self.widened.set(dir.clone()).ok()?;
        Some(format!(
            "crawl_scope: widened path scope from {narrow}/ to {dir}/ (start page links {} page(s) below it and {siblings} sibling page(s)); use --scope exact to keep it narrow",
            children.len(),
        ))
    }

    /// 从断点继续时放宽状态不落盘：入过队的 URL 里有窄范围外的，说明上次已经放宽过，
    /// 恢复到能装下它们的最近的祖先目录。
    pub(super) fn restore_widened<'a>(&self, enqueued: impl IntoIterator<Item = &'a Url>) {
        let outside: Vec<&Url> = enqueued
            .into_iter()
            .filter(|u| self.may_widen_to(u))
            .collect();
        if outside.is_empty() {
            return;
        }
        if let Some(dir) = self
            .ancestors
            .iter()
            .find(|dir| outside.iter().all(|u| under(u.path(), dir)))
        {
            let _ = self.widened.set(dir.clone());
        }
    }
}

/// 放宽一级的证据门槛：子页链接少于这个数，种子才算叶子页。
const WIDEN_MAX_CHILDREN: usize = 3;
/// 放宽一级的证据门槛：同级链接至少这么多。
const WIDEN_MIN_SIBLINGS: usize = 3;

/// 放宽一级的证据门槛：种子页去掉链接后的正文至少这么多字母/数字（约 40 个英文词或一段中文）。
const WIDEN_MIN_PROSE: usize = 200;

/// Markdown 里链接以外的字母/数字个数：`[文字](地址)`、`![图](地址)` 整段不算。
fn prose_chars(md: &str) -> usize {
    let b = md.as_bytes();
    let (mut i, mut n) = (0, 0);
    while i < md.len() {
        if b[i] == b'[' || (b[i] == b'!' && b.get(i + 1) == Some(&b'[')) {
            // 找配对的 `](...)`；找不到就当普通字符
            let open = if b[i] == b'!' { i + 1 } else { i };
            if let Some(close) = md[open..].find("](").map(|k| open + k)
                && let Some(end) = md[close + 2..].find(')').map(|k| close + 2 + k)
                && !md[open..close].contains('\n')
            {
                i = end + 1;
                continue;
            }
        }
        let c = md[i..].chars().next().unwrap_or(' ');
        n += usize::from(c.is_alphanumeric());
        i += c.len_utf8();
    }
    n
}

/// `path` 是不是 `prefix` 本身或它下面的路径（按路径段边界，`/docs/python` 不包含 `/docs/pythonic`）。
fn under(path: &str, prefix: &str) -> bool {
    path.strip_prefix(prefix)
        .is_some_and(|rest| rest.is_empty() || rest.starts_with('/'))
}

/// 目录的最后一段是不是日期：年份（4 位数字）、月或日（1–2 位数字）、英文月份缩写（`Dec`）。
fn is_date_dir(dir: &str) -> bool {
    const MONTHS: [&str; 12] = [
        "jan", "feb", "mar", "apr", "may", "jun", "jul", "aug", "sep", "oct", "nov", "dec",
    ];
    let last = dir.rsplit('/').next().unwrap_or_default();
    (last.bytes().all(|b| b.is_ascii_digit()) && matches!(last.len(), 1 | 2 | 4))
        || MONTHS.contains(&last.to_ascii_lowercase().as_str())
}

/// `/blog/index.html`、`/blog/index.php` 这类目录首页。
fn is_index_of(path: &str, dir: &str) -> bool {
    path.strip_prefix(dir)
        .and_then(|rest| rest.strip_prefix('/'))
        .is_some_and(|name| name.starts_with("index.") || name.starts_with("default."))
}

/// 种子路径最后一段是不是页面名（`/blog/post-3`、`/blog/2026/07/21/post/`），而不是文件名
/// （`/docs/index.html`）。只有页面名种子的范围可能放宽：文件名种子本来就取的是所在目录。
/// 结尾带 `/` 的种子也可能是页面（很多博客这样写文章地址）；是不是目录页交给子页链接判断。
fn seed_names_page(start: &Url) -> bool {
    start
        .path()
        .trim_end_matches('/')
        .rsplit('/')
        .next()
        .is_some_and(|last| !last.is_empty() && !last.contains('.'))
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

/// 等着重试的页面：到点了才重新调度。
#[derive(Default)]
pub(super) struct Retries {
    /// (到点时间, 地址, 深度, 下一次是第几次, 上一次的错误)
    items: Vec<(tokio::time::Instant, Url, usize, u32, Option<ApiError>)>,
}

impl Retries {
    /// 第 `attempt` 次失败的结果值得重试就排进来，返回 true；否则返回 false，结果照常交付。
    /// `seed` 说它是不是种子页（重试得多、等得久）。
    pub(super) fn offer(
        &mut self,
        result: &Result<pagesift_api::types::Document, ApiError>,
        url: &Url,
        depth: usize,
        attempt: u32,
        seed: bool,
    ) -> bool {
        let (max, base) = if seed {
            (SEED_MAX_RETRIES, SEED_RETRY_BASE)
        } else {
            (MAX_RETRIES, RETRY_BASE)
        };
        match retry_wait(result, attempt, base) {
            Some(wait) if attempt < max && wait <= MAX_WAIT => {
                self.items.push((
                    tokio::time::Instant::now() + wait,
                    url.clone(),
                    depth,
                    attempt + 1,
                    result.as_ref().err().cloned(),
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
        let (_, u, d, a, _) = self.items.swap_remove(i);
        Some((u, d, a))
    }

    /// 取出这个主机上所有等着重试的页面和它们上一次的错误（主机已经停掉，不再重试）。
    pub(super) fn drop_host(&mut self, host: &str) -> Vec<(Url, Option<ApiError>)> {
        let mut out = Vec::new();
        self.items.retain(|(_, u, _, _, e)| {
            let keep = u.host_str() != Some(host);
            if !keep {
                out.push((u.clone(), e.clone()));
            }
            keep
        });
        out
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
    base: Duration,
) -> Option<Duration> {
    let backoff = base * 2u32.pow(attempt.min(6));
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
pub(crate) fn allow_subdomains(req: &CrawlRequest) -> bool {
    req.allow_subdomains.unwrap_or(req.query.is_some())
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
    fn same_host_http_links_upgrade_to_https() {
        let learner = Learner::default();
        let base = Url::parse("https://www.e.edu/study/").unwrap();
        let same = candidate(&base, "http://www.e.edu/fees/", &learner).unwrap();
        assert_eq!(same.as_str(), "https://www.e.edu/fees/");
        // 同一站点的子域名也升级；别的站点不知道有没有 https：照原样
        let sub = candidate(&base, "http://grad.e.edu/", &learner).unwrap();
        assert_eq!(sub.scheme(), "https");
        let other = candidate(&base, "http://other.org/", &learner).unwrap();
        assert_eq!(other.scheme(), "http");
        // 种子本来就是 http：不动
        let http_base = Url::parse("http://www.e.edu/").unwrap();
        let kept = candidate(&http_base, "http://www.e.edu/fees/", &learner).unwrap();
        assert_eq!(kept.scheme(), "http");
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
    fn scope_with_file_ending_seed_uses_its_directory() {
        let file = u("https://example.com/travel_2/index.html");
        let s = Scope::new(&file, false, false, CrawlScope::Auto, &[], &[]).unwrap();
        assert!(s.allows(&u("https://example.com/travel_2/page-2.html")));
        assert!(s.allows(&u("https://example.com/travel_2/")));
        assert!(s.allows(&u("https://example.com/travel_2")));
        assert!(!s.allows(&u("https://example.com/travel_3/page.html")));
        assert!(!s.allows(&u("https://example.com/")));

        // 嵌套文件名种子
        let deep = u("https://example.com/catalogue/books/travel_2/index.html");
        let s = Scope::new(&deep, false, false, CrawlScope::Auto, &[], &[]).unwrap();
        assert!(s.allows(&u("https://example.com/catalogue/books/travel_2/a.html")));
        assert!(!s.allows(&u("https://example.com/catalogue/books/other/a.html")));

        // 单段路径保持原范围，不放大到整站
        let section = u("https://example.com/docs");
        let s = Scope::new(&section, false, false, CrawlScope::Auto, &[], &[]).unwrap();
        assert!(s.allows(&u("https://example.com/docs/intro")));
        assert!(!s.allows(&u("https://example.com/blog/x")));

        // 不像文件名的多段路径按自身前缀，不放大到上级目录
        let lang = u("https://example.com/docs/python");
        let s = Scope::new(&lang, false, false, CrawlScope::Auto, &[], &[]).unwrap();
        assert!(s.allows(&u("https://example.com/docs/python/tutorial")));
        assert!(s.allows(&u("https://example.com/docs/python")));
        assert!(!s.allows(&u("https://example.com/docs/java")));
        assert!(!s.allows(&u("https://example.com/docs/")));

        // 根路径仍是整站
        let root = u("https://example.com/");
        let s = Scope::new(&root, false, false, CrawlScope::Auto, &[], &[]).unwrap();
        assert!(s.allows(&u("https://example.com/blog/x")));

        // 根目录下的文件名种子也按整站处理
        let root_file = u("https://example.com/index.html");
        let s = Scope::new(&root_file, false, false, CrawlScope::Auto, &[], &[]).unwrap();
        assert!(s.allows(&u("https://example.com/a.html")));
    }

    #[test]
    fn auto_scope_widens_from_leaf_seed_only() {
        let urls = |ps: &[&str]| -> Vec<Url> {
            ps.iter()
                .map(|p| u(&format!("https://example.com{p}")))
                .collect()
        };
        // 博客文章：没有子页，链向同栏目的其他文章 → 放宽到 /blog
        let post = u("https://example.com/blog/post-3");
        let s = Scope::new(&post, false, false, CrawlScope::Auto, &[], &[]).unwrap();
        assert!(!s.allows(&u("https://example.com/blog/post-4")));
        assert!(s.may_widen_to(&u("https://example.com/blog/post-4")));
        let links = urls(&[
            "/blog/",
            "/blog/post-1",
            "/blog/post-2",
            "/blog/post-4",
            "/about",
        ]);
        let prose = "word ".repeat(60);
        let w = s.widen_from_seed(&post, &prose, &links).expect("widened");
        assert!(w.starts_with("crawl_scope:"), "{w}");
        assert!(s.allows(&u("https://example.com/blog/post-4")));
        assert!(
            !s.allows(&u("https://example.com/about")),
            "never widened to the root"
        );
        assert!(!s.may_widen_to(&u("https://example.com/blog/post-5")));

        // 目录页：有自己的子页 → 不放宽，同级栏目照样挡住
        let lang = u("https://example.com/docs/python");
        let s = Scope::new(&lang, false, false, CrawlScope::Auto, &[], &[]).unwrap();
        let links = urls(&[
            "/docs/python/tutorial",
            "/docs/python/api",
            "/docs/python/faq",
            "/docs/java",
            "/docs/go",
            "/docs/rust",
        ]);
        assert!(s.widen_from_seed(&lang, &prose, &links).is_none());
        assert!(!s.allows(&u("https://example.com/docs/java")));

        // 同级链接太少、只链到上级目录首页 → 不放宽
        let s = Scope::new(&post, false, false, CrawlScope::Auto, &[], &[]).unwrap();
        let links = urls(&["/blog/", "/blog/index.html", "/blog/post-1", "/blog/post-3"]);
        assert!(s.widen_from_seed(&post, &prose, &links).is_none());

        // 只列链接、没有正文的页面（空分类页）不放宽
        let s = Scope::new(&post, false, false, CrawlScope::Auto, &[], &[]).unwrap();
        let links = urls(&[
            "/blog/post-1",
            "/blog/post-2",
            "/blog/post-4",
            "/blog/post-5",
        ]);
        let only_links: String = links
            .iter()
            .map(|l| format!("- [{l} long anchor text]({l})\n"))
            .collect();
        assert!(s.widen_from_seed(&post, &only_links, &links).is_none());
        assert!(prose_chars(&only_links) == 0);
        assert_eq!(prose_chars("中文正文 [链接](/x) ![图](/y.png) abc"), 7);

        // 按日期分目录的博客：同一天没有别的文章，放宽到最近的有同级文章的那一层
        let dated = u("https://example.com/blog/2026/07/21/nice-things/");
        let s = Scope::new(&dated, false, false, CrawlScope::Auto, &[], &[]).unwrap();
        let links = urls(&[
            "/blog/",
            "/blog/2026/07/17/sqlite/",
            "/blog/2026/09/27/lights/",
            "/blog/2025/12/01/year/",
            "/about",
        ]);
        let w = s.widen_from_seed(&dated, &prose, &links).expect("widened");
        assert!(w.contains("to /blog/"), "{w}");
        assert!(s.allows(&u("https://example.com/blog/2024/01/01/old/")));
        assert!(!s.allows(&u("https://example.com/about")));
        // 同一年就有 3 篇时停在那一年
        let s = Scope::new(&dated, false, false, CrawlScope::Auto, &[], &[]).unwrap();
        let links = urls(&[
            "/blog/2026/07/17/a/",
            "/blog/2026/09/27/b/",
            "/blog/2026/01/02/c/",
        ]);
        assert!(
            s.widen_from_seed(&dated, &prose, &links)
                .expect("widened")
                .contains("to /blog/2026/")
        );
        // 结尾带 / 的目录页有自己的子页 → 不放宽
        let dir = u("https://example.com/docs/concepts/workloads/");
        let s = Scope::new(&dir, false, false, CrawlScope::Auto, &[], &[]).unwrap();
        let links = urls(&[
            "/docs/concepts/workloads/pods/",
            "/docs/concepts/workloads/jobs/",
            "/docs/concepts/workloads/cron/",
            "/docs/concepts/services/",
            "/docs/concepts/storage/",
            "/docs/concepts/security/",
        ]);
        assert!(s.widen_from_seed(&dir, &prose, &links).is_none());

        // 根下单段页面、目录种子、文件名种子都不放宽
        for seed in ["/about", "/docs/", "/docs/page.html"] {
            let seed = u(&format!("https://example.com{seed}"));
            let s = Scope::new(&seed, false, false, CrawlScope::Auto, &[], &[]).unwrap();
            let links = urls(&["/a", "/b", "/c", "/docs/a", "/docs/b", "/docs/c"]);
            assert!(s.widen_from_seed(&seed, &prose, &links).is_none(), "{seed}");
        }

        // exact 从不放宽；parent 一开始就是上一级；domain 是整站
        let s = Scope::new(&post, false, false, CrawlScope::Exact, &[], &[]).unwrap();
        let links = urls(&["/blog/post-1", "/blog/post-2", "/blog/post-4"]);
        assert!(s.widen_from_seed(&post, &prose, &links).is_none());
        let s = Scope::new(&post, false, false, CrawlScope::Parent, &[], &[]).unwrap();
        assert!(s.allows(&u("https://example.com/blog/post-4")));
        assert!(!s.allows(&u("https://example.com/about")));
        // parent 跳过日期目录取所在栏目；栏目是整站根时爬整个主机
        let s = Scope::new(&dated, false, false, CrawlScope::Parent, &[], &[]).unwrap();
        assert!(s.allows(&u("https://example.com/blog/2025/01/02/x/")));
        assert!(!s.allows(&u("https://example.com/about")));
        let root_dated = u("https://example.com/2024/Dec/31/llms/");
        let s = Scope::new(&root_dated, false, false, CrawlScope::Parent, &[], &[]).unwrap();
        assert!(s.allows(&u("https://example.com/2023/Jan/02/other/")));
        assert!(s.allows(&u("https://example.com/about")));
        let s = Scope::new(&post, false, false, CrawlScope::Domain, &[], &[]).unwrap();
        assert!(s.allows(&u("https://example.com/about")));

        // 从断点继续：入过队的 URL 在上一级范围内 → 恢复放宽
        let s = Scope::new(&post, false, false, CrawlScope::Auto, &[], &[]).unwrap();
        s.restore_widened(&urls(&["/blog/post-3", "/blog/post-7"]));
        assert!(s.allows(&u("https://example.com/blog/post-9")));
    }

    #[test]
    fn scope_rules() {
        let start = u("https://www.example.com/docs/");
        let s = Scope::new(
            &start,
            false,
            false,
            CrawlScope::Auto,
            &[],
            &["/docs/private".into()],
        )
        .unwrap();
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
        let wide = Scope::new(
            &start,
            true,
            false,
            CrawlScope::Domain,
            &["^/docs/".into()],
            &[],
        )
        .unwrap();
        assert!(wide.allows(&u("https://api.example.com/docs/x")));
        assert!(
            !wide.allows(&u("https://example.com/blog/x")),
            "include pattern"
        );
        assert!(Scope::new(&start, false, false, CrawlScope::Auto, &["(".into()], &[]).is_err());
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

    #[test]
    fn the_seed_page_is_retried_longer_than_other_pages() {
        let timeout: Result<pagesift_api::types::Document, ApiError> =
            Err(ApiError::new(ErrorCode::Timeout, "first byte timeout"));
        let url = Url::parse("https://example.com/").unwrap();
        // 普通页面：两次重试，0.5 秒、1 秒
        let mut r = Retries::default();
        assert!(r.offer(&timeout, &url, 1, 0, false));
        assert!(r.offer(&timeout, &url, 1, 1, false));
        assert!(!r.offer(&timeout, &url, 1, 2, false));
        assert_eq!(
            retry_wait(&timeout, 0, RETRY_BASE),
            Some(Duration::from_millis(500))
        );
        // 种子页：四次重试，2、4、8、16 秒
        let mut s = Retries::default();
        for attempt in 0..4 {
            assert!(s.offer(&timeout, &url, 0, attempt, true), "{attempt}");
            assert_eq!(
                retry_wait(&timeout, attempt, SEED_RETRY_BASE),
                Some(Duration::from_secs(2 << attempt))
            );
        }
        assert!(!s.offer(&timeout, &url, 0, 4, true));
    }
}
