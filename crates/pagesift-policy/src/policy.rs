use std::collections::HashMap;
use std::sync::{Arc, Mutex};
use std::time::{Duration, SystemTime};

use pagesift_net::{
    BoxError, BoxFuture, FetchRequest, FetchResponse, Fetcher, RedirectGuard, Url, product_token,
};
use tokio::time::Instant;

use crate::backoff::{Backoff, BackoffConfig, parse_retry_after};
use crate::error::Error;
use crate::limiter::{Permit, RateLimitConfig, RateLimiter};
use crate::robots::{MAX_ROBOTS_BYTES, Robots};

#[derive(Debug, Clone)]
pub struct PolicyConfig {
    pub rate: RateLimitConfig,
    pub backoff: BackoffConfig,
    pub robots_ttl: Duration,
    /// robots.txt 返回 5xx（按全部禁止处理）时的缓存时间，比正常结果短，方便站点恢复后尽快重试。
    pub robots_error_ttl: Duration,
    /// 管理员开关：是否允许 crawl 和 map 忽略 robots.txt（F-2.4）。
    pub allow_crawl_ignore_robots: bool,
}

impl Default for PolicyConfig {
    fn default() -> Self {
        Self {
            rate: RateLimitConfig::default(),
            backoff: BackoffConfig::default(),
            robots_ttl: Duration::from_secs(24 * 3600),
            robots_error_ttl: Duration::from_secs(600),
            allow_crawl_ignore_robots: false,
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
    entries: Mutex<HashMap<String, Slot>>,
}

type Slot = Arc<tokio::sync::Mutex<Option<(Instant, Arc<Robots>)>>>;

const ROBOTS_CACHE_PRUNE_AT: usize = 4096;

impl RobotsCache {
    pub fn new(fetcher: Arc<Fetcher>, ttl: Duration, error_ttl: Duration) -> Self {
        Self {
            fetcher,
            ttl,
            error_ttl,
            entries: Mutex::new(HashMap::new()),
        }
    }

    /// 取 `url` 所在源的 robots.txt。网络错误和 SSRF 拦截原样返回，不缓存。
    pub async fn get(&self, url: &Url) -> Result<Arc<Robots>, Error> {
        let origin = url.origin().ascii_serialization();
        let slot = {
            let mut entries = self.entries.lock().expect("lock poisoned");
            if entries.len() >= ROBOTS_CACHE_PRUNE_AT && !entries.contains_key(&origin) {
                let now = Instant::now();
                entries.retain(|_, s| {
                    Arc::strong_count(s) > 1
                        || s.try_lock()
                            .map_or(true, |g| g.as_ref().is_some_and(|(t, _)| *t > now))
                });
            }
            entries.entry(origin.clone()).or_default().clone()
        };
        let mut guard = slot.lock().await;
        if let Some((expires, robots)) = guard.as_ref()
            && *expires > Instant::now()
        {
            return Ok(robots.clone());
        }
        let (robots, ttl) = self.fetch(&origin).await?;
        let robots = Arc::new(robots);
        *guard = Some((Instant::now() + ttl, robots.clone()));
        Ok(robots)
    }

    async fn fetch(&self, origin: &str) -> Result<(Robots, Duration), Error> {
        let mut req = FetchRequest::new(format!("{origin}/robots.txt"));
        req.max_body_bytes = Some(MAX_ROBOTS_BYTES);
        match self.fetcher.fetch(req).await {
            Ok(r) if (200..300).contains(&r.status) => {
                Ok((Robots::parse(&String::from_utf8_lossy(&r.body)), self.ttl))
            }
            // 4xx（含 429 以外的客户端错误）：没有可用的 robots.txt，全部允许。
            Ok(r) if (400..500).contains(&r.status) && r.status != 429 => {
                Ok((Robots::allow_all(), self.ttl))
            }
            // 5xx、429 以及其他异常状态：按全部禁止处理。
            Ok(_) => Ok((Robots::disallow_all(), self.error_ttl)),
            // RFC 9309：重定向超过 5 次可以视为不可用，按全部允许处理。
            Err(pagesift_net::Error::TooManyRedirects(_)) => Ok((Robots::allow_all(), self.ttl)),
            Err(e) => Err(e.into()),
        }
    }
}

/// 礼貌抓取的入口：冷却检查 → robots → 限速 → 抓取 → 记录主机健康状况。
pub struct Policy {
    fetcher: Arc<Fetcher>,
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
            backoff: Backoff::new(config.backoff.clone()),
            token: product_token(fetcher.user_agent()).to_string(),
            fetcher,
            config,
        }
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
        let url = self.fetcher.check_url(&req.url)?;
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
        let resp = match self.fetcher.fetch_with(req, &guard).await {
            Ok(r) => r,
            Err(e) => {
                let e = Error::from(e);
                if matches!(e, Error::Timeout(_) | Error::Upstream(_)) {
                    self.backoff.on_failure(&host);
                }
                return Err(e);
            }
        };
        let final_host = resp.final_url.host_str().unwrap_or_default().to_string();
        let retry_after = resp
            .header("retry-after")
            .and_then(|v| parse_retry_after(v, SystemTime::now()));
        match resp.status {
            429 => {
                self.backoff.on_throttled(&final_host, retry_after);
                Err(Error::RateLimited { retry_after })
            }
            503 => {
                self.backoff.on_throttled(&final_host, retry_after);
                Ok((resp, permit))
            }
            500..=599 => {
                self.backoff.on_failure(&final_host);
                Ok((resp, permit))
            }
            _ => {
                self.backoff.on_success(&final_host);
                Ok((resp, permit))
            }
        }
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
}
