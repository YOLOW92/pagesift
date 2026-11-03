use std::collections::HashMap;
use std::sync::{Arc, Mutex};
use std::time::{Duration, SystemTime};

use pagesift_net::{
    BoxError, BoxFuture, FetchRequest, FetchResponse, FetchStream, Fetcher, RedirectGuard, Url,
    product_token,
};
use tokio::time::Instant;

use crate::backoff::{Backoff, BackoffConfig, parse_retry_after};
use crate::egress::{EgressClass, EgressPool, PoolConfig};
use crate::error::Error;
use crate::limiter::{Permit, RateLimitConfig, RateLimiter};
use crate::outage::Outages;
use crate::robots::{MAX_ROBOTS_BYTES, Robots};

#[derive(Debug, Clone)]
pub struct PolicyConfig {
    pub rate: RateLimitConfig,
    pub backoff: BackoffConfig,
    pub robots_ttl: Duration,
    /// robots.txt 返回 5xx（按全部禁止处理）时缓存时间的上限。实际时长按 robots 故障持续时长的
    /// 分布：已持续 `e` 的故障有把握恢复（概率 ≥ 1−δ）的时刻，不超过这个值。
    pub robots_error_ttl: Duration,
    /// 管理员开关：是否允许 crawl 和 map 忽略 robots.txt（F-2.4）。
    pub allow_crawl_ignore_robots: bool,
    /// 管理员总开关：关掉后所有请求都不看 robots.txt，不需要请求里带 `ignoreRobotsTxt`。默认开。
    pub respect_robots: bool,
}

impl Default for PolicyConfig {
    fn default() -> Self {
        Self {
            rate: RateLimitConfig::default(),
            backoff: BackoffConfig::default(),
            robots_ttl: Duration::from_secs(24 * 3600),
            // [策略] 先验包 v0：0.81 的现值，作上限；故障时长分布有把握后按它缩短
            robots_error_ttl: Duration::from_secs(600),
            allow_crawl_ignore_robots: false,
            respect_robots: true,
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RequestKind {
    /// 单页 `/v2/scrape`。
    Scrape,
    Crawl,
    Map,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RobotsMode {
    Respect,
    Ignore,
}

/// 按 F-2.4 决定是否遵守 robots.txt。`ignore_robots_txt` 是请求里的 `ignoreRobotsTxt`。
pub fn robots_mode(
    kind: RequestKind,
    ignore_robots_txt: bool,
    config: &PolicyConfig,
) -> Result<RobotsMode, Error> {
    if !config.respect_robots {
        return Ok(RobotsMode::Ignore);
    }
    match (kind, ignore_robots_txt) {
        (_, false) => Ok(RobotsMode::Respect),
        (RequestKind::Scrape, true) => Ok(RobotsMode::Ignore),
        (_, true) if config.allow_crawl_ignore_robots => Ok(RobotsMode::Ignore),
        (_, true) => Err(Error::UnsupportedOption(
            "ignoreRobotsTxt on crawl and map must be enabled by the administrator".into(),
        )),
    }
}

/// 按源（协议 + 主机 + 端口）缓存 robots.txt。并发请求同一个源只抓一次。
pub struct RobotsCache {
    fetcher: Arc<Fetcher>,
    ttl: Duration,
    error_ttl: Duration,
    entries: Mutex<Entries>,
    /// robots.txt 故障（5xx、429 等按全部禁止处理的回答）持续时长的分布。
    outages: Outages,
}

struct Entries {
    map: HashMap<String, Slot>,
    prune_at: usize,
}

#[derive(Clone)]
struct RobotsEntry {
    expires: Instant,
    robots: Arc<Robots>,
    etag: Option<String>,
    last_modified: Option<SystemTime>,
    /// 这一轮 robots 故障从什么时候开始；正常结果为 `None`。
    failing_since: Option<Instant>,
}

type Slot = Arc<tokio::sync::Mutex<Option<RobotsEntry>>>;

/// [策略] 先验包 v0：0.81 的现值。缓存第一次到这么多源时清理过期项；之后门槛是清理后存活数的
/// 两倍（摊还 O(1)，动态表，CLRS §17.4），缓存不超过存活项的两倍
const ROBOTS_CACHE_PRUNE_AT: usize = 4096;

impl RobotsCache {
    pub fn new(fetcher: Arc<Fetcher>, ttl: Duration, error_ttl: Duration) -> Self {
        Self {
            fetcher,
            ttl,
            error_ttl,
            entries: Mutex::new(Entries {
                map: HashMap::new(),
                prune_at: ROBOTS_CACHE_PRUNE_AT,
            }),
            outages: Outages::default(),
        }
    }

    /// 取 `url` 所在源的 robots.txt。网络错误和 SSRF 拦截原样返回，不缓存。
    pub async fn get(&self, url: &Url) -> Result<Arc<Robots>, Error> {
        let origin = url.origin().ascii_serialization();
        let slot = {
            let mut entries = self.entries.lock().expect("lock poisoned");
            if entries.map.len() >= entries.prune_at && !entries.map.contains_key(&origin) {
                let now = Instant::now();
                entries.map.retain(|_, s| {
                    Arc::strong_count(s) > 1
                        || s.try_lock().map_or(true, |g| {
                            g.as_ref().is_some_and(|entry| entry.expires > now)
                        })
                });
                entries.prune_at = entries
                    .map
                    .len()
                    .saturating_mul(2)
                    .max(ROBOTS_CACHE_PRUNE_AT);
            }
            entries.map.entry(origin.clone()).or_default().clone()
        };
        let mut guard = slot.lock().await;
        if let Some(entry) = guard.as_ref()
            && entry.expires > Instant::now()
        {
            return Ok(entry.robots.clone());
        }
        let refreshed = self.fetch(&origin, guard.as_ref()).await?;
        let now = Instant::now();
        let failing_since = guard.as_ref().and_then(|e| e.failing_since);
        let entry = match refreshed {
            RobotsRefresh::NotModified(ttl) => {
                let mut entry = guard.take().expect("conditional robots entry");
                entry.expires = now + ttl;
                entry
            }
            RobotsRefresh::Replace {
                robots,
                ttl,
                etag,
                last_modified,
                failed,
            } => {
                let (ttl, failing_since) = if failed {
                    // 故障已持续 e：有把握恢复的时刻再来取，不超过配置的上限
                    let since = failing_since.unwrap_or(now);
                    let wait = self
                        .outages
                        .retry_in(now - since)
                        .map_or(ttl, |d| d.min(ttl));
                    (wait, Some(since))
                } else {
                    if let Some(since) = failing_since {
                        self.outages.recovered(now - since);
                    }
                    (ttl, None)
                };
                RobotsEntry {
                    expires: now + ttl,
                    robots: Arc::new(robots),
                    etag,
                    last_modified,
                    failing_since,
                }
            }
        };
        let robots = entry.robots.clone();
        *guard = Some(entry);
        Ok(robots)
    }

    async fn fetch(
        &self,
        origin: &str,
        previous: Option<&RobotsEntry>,
    ) -> Result<RobotsRefresh, Error> {
        let mut req = FetchRequest::new(format!("{origin}/robots.txt"));
        req.max_body_bytes = Some(MAX_ROBOTS_BYTES);
        if let Some(previous) = previous {
            req.if_none_match = previous.etag.clone();
            req.if_modified_since = previous.last_modified;
        }
        match self.fetcher.fetch(req).await {
            Ok(r) if r.status == 304 && previous.is_some() => {
                Ok(RobotsRefresh::NotModified(self.ttl))
            }
            Ok(r) if (200..300).contains(&r.status) => Ok(RobotsRefresh::Replace {
                robots: Robots::parse(&String::from_utf8_lossy(&r.body)),
                ttl: self.ttl,
                etag: r.etag().map(str::to_owned),
                last_modified: r
                    .last_modified()
                    .and_then(|v| httpdate::parse_http_date(v).ok()),
                failed: false,
            }),
            // 4xx（含 429 以外的客户端错误）：没有可用的 robots.txt，全部允许。
            Ok(r) if (400..500).contains(&r.status) && r.status != 429 => {
                Ok(RobotsRefresh::replace(Robots::allow_all(), self.ttl))
            }
            // 5xx、429 以及其他异常状态：按全部禁止处理。
            Ok(_) => Ok(RobotsRefresh::Replace {
                robots: Robots::disallow_all(),
                ttl: self.error_ttl,
                etag: None,
                last_modified: None,
                failed: true,
            }),
            // RFC 9309：重定向超过 5 次可以视为不可用，按全部允许处理。
            Err(pagesift_net::Error::TooManyRedirects(_)) => {
                Ok(RobotsRefresh::replace(Robots::allow_all(), self.ttl))
            }
            Err(e) => Err(e.into()),
        }
    }
}

enum RobotsRefresh {
    NotModified(Duration),
    Replace {
        robots: Robots,
        ttl: Duration,
        etag: Option<String>,
        last_modified: Option<SystemTime>,
        /// 5xx、429 等按全部禁止处理的回答（robots 故障）。
        failed: bool,
    },
}

impl RobotsRefresh {
    fn replace(robots: Robots, ttl: Duration) -> Self {
        Self::Replace {
            robots,
            ttl,
            etag: None,
            last_modified: None,
            failed: false,
        }
    }
}

/// 礼貌抓取的入口：冷却检查 → robots → 限速 → 抓取 → 记录主机健康状况。
pub struct Policy {
    fetcher: Arc<Fetcher>,
    /// 命名的出口（各自走不同的代理）。请求可以指定其中一个；robots.txt 仍走默认出口。
    egress: HashMap<String, Arc<Fetcher>>,
    /// 命名出口的类别，按加入顺序（出口池的编号）。
    egress_order: Vec<(String, EgressClass)>,
    /// 两个及以上命名出口时的出口池：请求没指定出口时由它选。
    pool: Option<EgressPool>,
    config: PolicyConfig,
    robots: RobotsCache,
    limiter: RateLimiter,
    backoff: Backoff,
    token: String,
}

impl Policy {
    pub fn new(fetcher: Arc<Fetcher>, config: PolicyConfig) -> Self {
        Self {
            robots: RobotsCache::new(fetcher.clone(), config.robots_ttl, config.robots_error_ttl),
            limiter: RateLimiter::new(config.rate.clone()),
            backoff: Backoff::new(config.backoff.clone()).with_politeness(config.rate.politeness),
            token: product_token(fetcher.user_agent()).to_string(),
            fetcher,
            egress: HashMap::new(),
            egress_order: Vec::new(),
            pool: None,
            config,
        }
    }

    /// 加一个命名出口（类别 `direct`）。同名的会被替换。
    pub fn add_egress(&mut self, name: impl Into<String>, fetcher: Arc<Fetcher>) {
        self.add_egress_class(name, EgressClass::Direct, fetcher);
    }

    /// 加一个指定类别的命名出口。同名的会被替换。命名出口有两个及以上时建出口池
    /// （统计从头开始，只应在启动时调用）。
    pub fn add_egress_class(
        &mut self,
        name: impl Into<String>,
        class: EgressClass,
        fetcher: Arc<Fetcher>,
    ) {
        let name = name.into();
        match self.egress_order.iter_mut().find(|(n, _)| *n == name) {
            Some(slot) => slot.1 = class,
            None => self.egress_order.push((name.clone(), class)),
        }
        self.egress.insert(name, fetcher);
        self.pool = (self.egress_order.len() >= 2)
            .then(|| EgressPool::new(self.egress_order.clone(), PoolConfig::default()));
    }

    /// 出口池（命名出口少于两个时为 `None`，请求不指定出口就走默认出口，和以前一样）。
    pub fn egress_pool(&self) -> Option<&EgressPool> {
        self.pool.as_ref()
    }

    /// 命名出口的代理地址；名字本身是代理 URL 时返回它。默认出口、`direct` 出口或不认识的名字返回 `None`。
    pub fn egress_proxy(&self, name: &str) -> Option<String> {
        match self.egress.get(name) {
            Some(f) => f.config().proxy.clone(),
            None if is_proxy_url(name) => Some(name.to_string()),
            None => None,
        }
    }

    /// 配置里有这个命名出口。
    pub fn has_egress(&self, name: &str) -> bool {
        self.egress.contains_key(name)
    }

    /// 请求发往 `host` 时是否会带上管理员配置的 Cookie（出口池据此把会话固定在一个出口）。
    pub fn has_configured_cookies(&self, host: &str) -> bool {
        let host = host.trim_end_matches('.');
        self.fetcher.config().credentials.iter().any(|c| {
            !c.cookies.is_empty() && c.host.trim_end_matches('.').eq_ignore_ascii_case(host)
        })
    }

    pub fn config(&self) -> &PolicyConfig {
        &self.config
    }

    pub fn fetcher(&self) -> &Arc<Fetcher> {
        &self.fetcher
    }

    pub fn robots_cache(&self) -> &RobotsCache {
        &self.robots
    }

    pub fn limiter(&self) -> &RateLimiter {
        &self.limiter
    }

    pub fn backoff(&self) -> &Backoff {
        &self.backoff
    }

    /// robots.txt 用的产品标识（从 UA 取）。
    pub fn product_token(&self) -> &str {
        &self.token
    }

    pub fn robots_mode(
        &self,
        kind: RequestKind,
        ignore_robots_txt: bool,
    ) -> Result<RobotsMode, Error> {
        robots_mode(kind, ignore_robots_txt, &self.config)
    }

    /// robots.txt 被忽略（[`RobotsMode::Ignore`]）时的原因，写进审计日志。
    pub fn robots_ignore_reason(&self, kind: RequestKind) -> &'static str {
        match kind {
            _ if !self.config.respect_robots => "respect_robots=false",
            RequestKind::Scrape => "ignoreRobotsTxt",
            RequestKind::Crawl | RequestKind::Map => "ignoreRobotsTxt+allow_crawl_ignore_robots",
        }
    }

    /// 检查 robots.txt，返回适用的 Crawl-delay。
    pub async fn check_robots(&self, url: &Url) -> Result<Option<Duration>, Error> {
        let robots = self.robots.get(url).await?;
        let m = robots.matcher(&self.token);
        let target = match url.query() {
            Some(q) => format!("{}?{q}", url.path()),
            None => url.path().to_string(),
        };
        if m.is_allowed(&target) {
            Ok(m.crawl_delay())
        } else {
            Err(Error::RobotsDisallowed {
                url: pagesift_net::redact_url(url.as_str()),
            })
        }
    }

    /// 礼貌地抓一个页面。429 返回 `RATE_LIMITED`；5xx 照常返回响应（状态如实），但计入主机失败。
    pub async fn fetch(&self, req: FetchRequest, mode: RobotsMode) -> Result<FetchResponse, Error> {
        self.fetch_held(req, mode).await.map(|(resp, _permit)| resp)
    }

    /// 和 [`Policy::fetch`] 一样，但把并发名额一起交给调用方：调用方处理完响应再释放，
    /// 已下载、未处理的页面数因此不会超过全局并发。名额在主机名额之后才占用，
    /// 等某个主机的请求不会占着全局名额，挡住其他主机。
    pub async fn fetch_held(
        &self,
        req: FetchRequest,
        mode: RobotsMode,
    ) -> Result<(FetchResponse, Permit), Error> {
        self.fetch_held_via(req, mode, None).await
    }

    /// 同 [`Policy::fetch_held`]，`egress` 指定走哪个命名出口（`None` 为默认出口）。
    /// 限速、冷却和 robots 按主机算，和出口无关。
    pub async fn fetch_held_via(
        &self,
        req: FetchRequest,
        mode: RobotsMode,
        egress: Option<&str>,
    ) -> Result<(FetchResponse, Permit), Error> {
        let dynamic_fetcher;
        let fetcher: &Fetcher = match egress {
            None => &self.fetcher,
            Some(name) => match self.egress.get(name) {
                Some(f) => f.as_ref(),
                None if is_proxy_url(name) => {
                    let mut fc = self.fetcher.config().clone();
                    fc.proxy = Some(name.to_string());
                    dynamic_fetcher = Arc::new(Fetcher::new_egress(fc)?);
                    dynamic_fetcher.as_ref()
                }
                None => return Err(Error::InvalidRequest(format!("unknown egress `{name}`"))),
            },
        };
        let url = fetcher.check_url(&req.url)?;
        let host = url.host_str().unwrap_or_default().to_string();
        self.check_cooldown(&host)?;
        let crawl_delay = match mode {
            RobotsMode::Respect => self.check_robots(&url).await?,
            RobotsMode::Ignore => None,
        };
        let permit = self.limiter.acquire(&host, crawl_delay).await;
        // 等待名额期间主机可能进入了冷却。
        self.check_cooldown(&host)?;
        let guard = HopGuard { policy: self, mode };
        let resp = match fetcher.fetch_with(req, &guard).await {
            Ok(r) => r,
            Err(e) => {
                let e = Error::from(e);
                // 区分两类：DNS/上游错误说明站点本身不可达，按主机级退避（可指数增长）；
                // 单个 URL 的超时/网络错误只按"不同 URL"计数，避免一个永久慢页把整站冻住。
                match e {
                    Error::Dns(_) | Error::Upstream(_) => self.backoff.on_failure(&host),
                    Error::Timeout(_) | Error::Network(_) => {
                        self.backoff.on_url_failure(&host, url.as_str())
                    }
                    _ => {}
                }
                return Err(e);
            }
        };
        self.record_status(&resp.final_url, resp.status, resp.header("retry-after"))?;
        Ok((resp, permit))
    }

    /// Raw byte stream with robots, cooldown and per-host/global admission.
    /// Retain the returned permit until the body and disk writes are complete.
    pub async fn stream_held(
        &self,
        req: FetchRequest,
        mode: RobotsMode,
    ) -> Result<(FetchStream, Permit), Error> {
        let url = self.fetcher.check_url(&req.url)?;
        let host = url.host_str().unwrap_or_default();
        self.check_cooldown(host)?;
        let delay = match mode {
            RobotsMode::Respect => self.check_robots(&url).await?,
            RobotsMode::Ignore => None,
        };
        let permit = self.limiter.acquire(host, delay).await;
        self.check_cooldown(host)?;
        let guard = HopGuard { policy: self, mode };
        let stream = match self.fetcher.fetch_stream_with(req, &guard).await {
            Ok(stream) => stream,
            Err(e) => {
                let e = Error::from(e);
                match e {
                    Error::Dns(_) | Error::Upstream(_) => self.backoff.on_failure(host),
                    Error::Timeout(_) | Error::Network(_) => {
                        self.backoff.on_url_failure(host, url.as_str())
                    }
                    _ => {}
                }
                return Err(e);
            }
        };
        self.record_status(
            &stream.final_url,
            stream.status,
            stream.header("retry-after"),
        )?;
        Ok((stream, permit))
    }

    fn record_status(
        &self,
        url: &Url,
        status: u16,
        retry_after: Option<&str>,
    ) -> Result<(), Error> {
        let host = url.host_str().unwrap_or_default();
        let retry_after = retry_after.and_then(|v| parse_retry_after(v, SystemTime::now()));
        match status {
            429 => {
                self.backoff.on_throttled(host, retry_after);
                // 服务端给了 Retry-After 就表示"按这个节奏来"，我们照做即可；
                // 再永久加大整站的请求间隔会把站点越压越慢。只有没给指引时才收紧。
                if retry_after.is_none() && self.backoff.enabled() {
                    self.limiter.throttled(host);
                }
                return Err(Error::RateLimited { retry_after });
            }
            503 if retry_after.is_some() => {
                self.backoff.on_throttled(host, retry_after);
            }
            500..=599 => self.backoff.on_failure(host),
            _ => {
                self.backoff.on_success(host);
                self.limiter.succeeded(host);
            }
        }
        Ok(())
    }

    fn check_cooldown(&self, host: &str) -> Result<(), Error> {
        self.backoff
            .check(host)
            .map_err(|retry_after| Error::HostCooldown {
                host: host.to_string(),
                retry_after,
            })
    }
}

/// 请求的 `egress` 直接写了代理地址（不是配置里的名字）。
fn is_proxy_url(name: &str) -> bool {
    ["http://", "https://", "socks5://", "socks5h://"]
        .iter()
        .any(|p| name.starts_with(p))
}

/// 每一跳重定向：目标主机冷却检查和 robots 校验（SSRF 由 Fetcher 做）。
struct HopGuard<'p> {
    policy: &'p Policy,
    mode: RobotsMode,
}

impl RedirectGuard for HopGuard<'_> {
    fn check<'a>(&'a self, _from: &'a Url, to: &'a Url) -> BoxFuture<'a, Result<(), BoxError>> {
        Box::pin(async move {
            let host = to.host_str().unwrap_or_default();
            self.policy.check_cooldown(host)?;
            if self.mode == RobotsMode::Respect {
                self.policy.check_robots(to).await?;
            }
            Ok(())
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn f_2_4_robots_modes() {
        let strict = PolicyConfig::default();
        let open = PolicyConfig {
            allow_crawl_ignore_robots: true,
            ..Default::default()
        };
        use RequestKind::*;
        assert_eq!(
            robots_mode(Scrape, false, &strict).unwrap(),
            RobotsMode::Respect
        );
        assert_eq!(
            robots_mode(Scrape, true, &strict).unwrap(),
            RobotsMode::Ignore
        );
        for kind in [Crawl, Map] {
            assert_eq!(
                robots_mode(kind, false, &strict).unwrap(),
                RobotsMode::Respect
            );
            let err = robots_mode(kind, true, &strict).unwrap_err();
            assert_eq!(err.code().as_str(), "UNSUPPORTED_OPTION");
            assert_eq!(robots_mode(kind, true, &open).unwrap(), RobotsMode::Ignore);
        }
    }

    #[test]
    fn robots_switch_off_ignores_robots_for_every_request() {
        let off = PolicyConfig {
            respect_robots: false,
            ..Default::default()
        };
        for kind in [RequestKind::Scrape, RequestKind::Crawl, RequestKind::Map] {
            for asked in [false, true] {
                assert_eq!(robots_mode(kind, asked, &off).unwrap(), RobotsMode::Ignore);
            }
        }
    }
}
