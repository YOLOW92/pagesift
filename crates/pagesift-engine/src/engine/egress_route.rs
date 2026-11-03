//! 出口路由：请求走哪个出口，被拦截后换不换。
//!
//! - 请求指定了 `egress`（或 `proxy` 写了出口名、代理地址）：照用，用户策略优先；结果仍记进出口池的统计，不换。
//! - 没指定、配置了两个及以上命名出口：由出口池（[`pagesift_policy::EgressPool`]）选；
//!   按拦截分类决定退避、换一次出口重试或如实报告。换出口的重试照样经过站点限速器（礼貌预算按站点共享）。
//! - 没有出口池（零个或一个命名出口）：走默认出口，和以前完全一样。

use pagesift_policy::{EgressAction, EgressOutcome, EgressPool, Site};

use super::*;

impl Scraper {
    pub(super) async fn fetch_and_extract(
        &self,
        url: &str,
        opts: &ScrapeOptions,
        mode: RobotsMode,
    ) -> Result<Document, ApiError> {
        let user = opts.egress.as_deref().or_else(|| {
            opts.proxy
                .as_deref()
                .filter(|p| !matches!(*p, "basic" | "auto"))
        });
        // discoverScripts 不允许指定出口，也不走出口池（依赖脚本走默认出口）
        let pool = self.policy.egress_pool().filter(|_| !opts.discover_scripts);
        let (Some(pool), Ok(target)) = (pool, Url::parse(url)) else {
            return self.fetch_and_extract_via(url, opts, mode, user).await;
        };
        let host = target.host_str().unwrap_or_default().to_ascii_lowercase();
        let domain = pagesift_content::site_key(&host);
        let site = Site {
            host: &host,
            domain: &domain,
        };
        if let Some(name) = user {
            let result = self
                .fetch_and_extract_via(url, opts, mode, Some(name))
                .await;
            if let Some(id) = pool.position(name) {
                let pick = pool.pin(id);
                pool.report(site, &pick, outcome(&result));
            }
            return result;
        }
        self.fetch_pooled(pool, site, url, opts, mode).await
    }

    async fn fetch_pooled(
        &self,
        pool: &EgressPool,
        site: Site<'_>,
        url: &str,
        opts: &ScrapeOptions,
        mode: RobotsMode,
    ) -> Result<Document, ApiError> {
        let started = Instant::now();
        // 带 Cookie 的请求（自定义 Cookie 头或管理员配置的站点 Cookie）固定在同一出口
        let cookies = opts
            .headers
            .keys()
            .any(|k| k.eq_ignore_ascii_case("cookie"))
            || self.policy.has_configured_cookies(site.host);
        let capacity = self.concurrency();
        let pick = pool.choose(site, cookies, capacity);
        let first = self
            .fetch_and_extract_via(url, opts, mode, Some(pick.name()))
            .await;
        let other_class = match pool.report(site, &pick, outcome(&first)) {
            EgressAction::Switch { other_class } => other_class,
            EgressAction::SiteWide => return site_wide(first, pool.len()),
            EgressAction::Keep | EgressAction::Backoff => return first,
        };
        // 剩余时限：调用方给的 `timeout` 是整个请求的总时限，重试从里面扣
        let remaining = opts
            .timeout
            .map(|t| t.saturating_sub(started.elapsed().as_millis() as u64));
        if remaining == Some(0) {
            return first;
        }
        let Some(retry) = pool.choose_retry(site, &pick, other_class, capacity) else {
            return first;
        };
        let from = pick.name().to_string();
        drop(pick);
        let retry_opts;
        let o = match remaining {
            Some(t) => {
                retry_opts = ScrapeOptions {
                    timeout: Some(t),
                    ..opts.clone()
                };
                &retry_opts
            }
            None => opts,
        };
        let mut second = self
            .fetch_and_extract_via(url, o, mode, Some(retry.name()))
            .await;
        let note = format!("egress_switched: {from} -> {}", retry.name());
        if pool.report(site, &retry, outcome(&second)) == EgressAction::SiteWide {
            second = site_wide(second, pool.len());
        }
        match &mut second {
            Ok(doc) => {
                if let Some(meta) = doc.metadata.pagesift.as_mut() {
                    meta.warnings.push(note);
                }
            }
            Err(e) => add_detail(e, "egressSwitched", serde_json::json!(note)),
        }
        second
    }
}

/// 把一次结果按拦截分类归类（拦截页、状态码、网络错误）。
fn outcome(result: &Result<Document, ApiError>) -> EgressOutcome {
    match result {
        Ok(doc) => EgressOutcome::from_status(doc.metadata.status_code, false),
        Err(e) => match e.code {
            ErrorCode::AccessBarrier => {
                let field = |k: &str| {
                    e.details
                        .as_ref()
                        .and_then(|d| d.get(k))
                        .and_then(|v| v.as_str())
                        .unwrap_or_default()
                        .to_string()
                };
                EgressOutcome::from_barrier(&field("kind"), &field("ruleId"))
            }
            ErrorCode::RateLimited => EgressOutcome::Throttled,
            ErrorCode::NetworkError | ErrorCode::UpstreamError | ErrorCode::Timeout => {
                EgressOutcome::Unreachable
            }
            _ => EgressOutcome::Neutral,
        },
    }
}

/// 试过的出口都被拒：错误里注明，结果照原样返回。
fn site_wide(result: Result<Document, ApiError>, egresses: usize) -> Result<Document, ApiError> {
    result.map_err(|mut e| {
        add_detail(
            &mut e,
            "egressPool",
            serde_json::json!({ "verdict": "denied_on_every_egress_tried", "egresses": egresses }),
        );
        e
    })
}

fn add_detail(e: &mut ApiError, key: &str, value: serde_json::Value) {
    match e.details.as_mut().and_then(|d| d.as_object_mut()) {
        Some(map) => {
            map.insert(key.into(), value);
        }
        None => {
            let mut map = serde_json::Map::new();
            map.insert(key.into(), value);
            e.details = Some(serde_json::Value::Object(map));
        }
    }
}
