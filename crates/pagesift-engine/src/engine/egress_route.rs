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
        if self.itna {
            return self.fetch_itna(url, opts, mode).await;
        }
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

    async fn fetch_itna(
        &self,
        url: &str,
        opts: &ScrapeOptions,
        mode: RobotsMode,
    ) -> Result<Document, ApiError> {
        let timeout = Duration::from_millis(opts.timeout.unwrap_or(30_000));
        let budget = Arc::new(pagesift_itna::NetworkBudget::new(timeout, 16, 128));
        ITNA_NETWORK_BUDGET
            .scope(budget, self.fetch_itna_inner(url, opts, mode))
            .await
    }

    async fn fetch_itna_inner(
        &self,
        url: &str,
        opts: &ScrapeOptions,
        mode: RobotsMode,
    ) -> Result<Document, ApiError> {
        use pagesift_itna::{Attempts, Decision, Executor};

        // Keep the selected egress for both attempts; the legacy pooled route is
        // deliberately bypassed so it cannot perform an unbudgeted retry.
        let target = Url::parse(url).map_err(|e| ApiError::invalid(e.to_string()))?;
        let host = target.host_str().unwrap_or_default().to_ascii_lowercase();
        let domain = pagesift_content::site_key(&host);
        let site = Site {
            host: &host,
            domain: &domain,
        };
        let explicit = opts.egress.as_deref().or_else(|| {
            opts.proxy
                .as_deref()
                .filter(|p| !matches!(*p, "basic" | "auto"))
        });
        let pool = self.policy.egress_pool().filter(|_| !opts.discover_scripts);
        let sensitive = opts.headers.keys().any(|key| {
            key.eq_ignore_ascii_case("cookie")
                || key.eq_ignore_ascii_case("authorization")
                || key.eq_ignore_ascii_case("proxy-authorization")
        }) || !self.policy.fetcher().config().credentials.is_empty();
        let pick = if explicit.is_none() {
            pool.map(|p| p.choose(site, sensitive, self.concurrency()))
        } else {
            None
        };
        let selected = explicit.or_else(|| pick.as_ref().map(|p| p.name()));
        let default_proxy = if selected.is_none() {
            self.policy
                .fetcher()
                .effective_proxy(url)
                .await
                .map_err(|e| policy_error(e.into()))?
        } else {
            None
        };
        let egress = selected;
        let expected_direct = selected.is_none().then_some(default_proxy.is_none());
        // Identity reuse requires a stable caller. Without one, a private
        // session still carries cookies from HTTP to the browser retry but is
        // discarded afterwards and never shared.
        let session_config = pagesift_net::SessionConfig {
            exclusive: true,
            neutral_on_cancel: true,
            ..Default::default()
        };
        let mut _lease = None;
        let session = if !sensitive && opts.headers.is_empty() && !opts.discover_scripts {
            match opts.owner.as_deref() {
                Some(owner) => {
                    let key = pagesift_policy::identity_pool::IdentityKey {
                        owner: owner.to_owned(),
                        origin: target.origin().ascii_serialization(),
                        egress: egress.or(default_proxy.as_deref()).map(str::to_owned),
                        fingerprint: self.policy.fetcher().config().browser_fingerprint,
                    };
                    match self
                        .identities
                        .try_acquire(key, &target)
                        .map_err(|e| ApiError::invalid(e.to_string()))?
                    {
                        Some(lease) => {
                            let cookies = lease.cookies();
                            _lease = Some(lease);
                            Some(cookies)
                        }
                        None => Some(std::sync::Arc::new(
                            pagesift_net::CookieSession::new(&target, session_config)
                                .map_err(|e| ApiError::invalid(e.to_string()))?,
                        )),
                    }
                }
                None => Some(std::sync::Arc::new(
                    pagesift_net::CookieSession::new(&target, session_config)
                        .map_err(|e| ApiError::invalid(e.to_string()))?,
                )),
            }
        } else {
            None
        };
        let first_executor =
            if opts.engine.as_deref() == Some("browser") || !opts.actions.is_empty() {
                Executor::Browser
            } else {
                Executor::Http
            };
        // Apply one deadline even when the caller did not provide one.
        let mut attempts =
            Attempts::new(Duration::from_millis(opts.timeout.unwrap_or(30_000)), 2, 1);
        if !attempts.reserve(first_executor) {
            return Err(ApiError::new(
                ErrorCode::Timeout,
                "itna attempt budget exhausted",
            ));
        }
        let mut first_opts = opts.clone();
        first_opts.timeout = Some(attempts.remaining().as_millis().min(u64::MAX as u128) as u64);
        let mut first = ITNA_DEFAULT_PROXY
            .scope(
                default_proxy.clone(),
                ITNA_EXPECTED_DIRECT.scope(
                    expected_direct,
                    ITNA_COOKIE_SESSION.scope(
                        session.clone(),
                        self.fetch_and_extract_via(url, &first_opts, mode, egress),
                    ),
                ),
            )
            .await;
        let assessment = itna_outcome(&first);
        if let (Some(pool), Some(pick)) = (pool, pick.as_ref()) {
            pool.report(site, pick, outcome(&first));
        }
        let browser_allowed = opts.engine.as_deref() != Some("http")
            && opts.respond_timing.as_deref() != Some("html")
            && !opts.discover_scripts
            && !sensitive
            && opts.headers.is_empty();
        if attempts.finish(first_executor, assessment, browser_allowed) != Decision::TryBrowser
            || !attempts.reserve(Executor::Browser)
        {
            let engine = if opts.engine.as_deref() == Some("http") {
                "http"
            } else if first_executor == Executor::Browser {
                "browser"
            } else {
                "auto"
            };
            add_itna_trace(
                &mut first,
                serde_json::json!({
                    "attempts": [{"engine": engine, "outcome": assessment}],
                    "browserExecutions": ITNA_NETWORK_BUDGET.try_with(|b| b.browser_executions()).unwrap_or(0)
                }),
            );
            return first;
        }
        let mut browser_opts = opts.clone();
        browser_opts.engine = Some("browser".into());
        browser_opts.timeout = Some(attempts.remaining().as_millis().min(u64::MAX as u128) as u64);
        let mut result = ITNA_DEFAULT_PROXY
            .scope(
                default_proxy,
                ITNA_EXPECTED_DIRECT.scope(
                    expected_direct,
                    ITNA_COOKIE_SESSION.scope(
                        session,
                        self.fetch_and_extract_via(url, &browser_opts, mode, egress),
                    ),
                ),
            )
            .await;
        attempts.finish(Executor::Browser, itna_outcome(&result), false);
        let trace = serde_json::json!({
            "attempts": [
                {"engine": "http", "outcome": assessment},
                {"engine": "browser", "outcome": itna_outcome(&result)}
            ],
            "browserExecutions": ITNA_NETWORK_BUDGET.try_with(|b| b.browser_executions()).unwrap_or(0)
        });
        add_itna_trace(&mut result, trace);
        result
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

fn add_itna_trace(result: &mut Result<Document, ApiError>, trace: serde_json::Value) {
    match result {
        Ok(doc) => {
            doc.metadata
                .pagesift
                .get_or_insert_with(Default::default)
                .itna = Some(trace)
        }
        Err(e) => add_detail(e, "itna", trace),
    }
}

fn itna_outcome(result: &Result<Document, ApiError>) -> pagesift_itna::Outcome {
    use pagesift_itna::{Outcome, classify};
    match result {
        Ok(doc) => classify(
            Some(doc.metadata.status_code),
            doc.metadata.retry_after,
            None,
        ),
        Err(e) => match e.code {
            ErrorCode::AccessBarrier => {
                let field = |key| {
                    e.details
                        .as_ref()
                        .and_then(|d| d.get(key))
                        .and_then(|v| v.as_str())
                        .unwrap_or("")
                };
                classify(None, false, Some((field("kind"), field("ruleId"))))
            }
            ErrorCode::RateLimited | ErrorCode::HostCooldown => Outcome::Throttled,
            ErrorCode::NetworkError | ErrorCode::Timeout | ErrorCode::DnsFailure => {
                Outcome::Unreachable
            }
            _ => Outcome::Other,
        },
    }
}

/// 把一次结果按拦截分类归类（拦截页、状态码、网络错误）。
fn outcome(result: &Result<Document, ApiError>) -> EgressOutcome {
    match result {
        Ok(doc) => EgressOutcome::from_status(doc.metadata.status_code, doc.metadata.retry_after),
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

#[cfg(test)]
mod tests {
    use super::*;
    use axum::{Router, http::StatusCode, response::Html, routing::get};
    use std::sync::atomic::{AtomicUsize, Ordering};

    async fn fixture(status: StatusCode, body: &'static str) -> (String, Arc<AtomicUsize>) {
        let calls = Arc::new(AtomicUsize::new(0));
        let counter = calls.clone();
        let app = Router::new().route(
            "/",
            get(move || {
                let counter = counter.clone();
                async move {
                    counter.fetch_add(1, Ordering::SeqCst);
                    (status, Html(body))
                }
            }),
        );
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = listener.local_addr().unwrap();
        tokio::spawn(async move { axum::serve(listener, app).await.unwrap() });
        (format!("http://{addr}/"), calls)
    }

    fn scraper() -> Scraper {
        let mut cfg = ScraperConfig {
            itna: true,
            ..Default::default()
        };
        cfg.fetcher.env_proxy = false;
        cfg.fetcher.ssrf.allow_private_network = true;
        cfg.policy.rate.default_interval = Duration::ZERO;
        Scraper::new(cfg).unwrap()
    }

    #[tokio::test]
    async fn http_only_barrier_is_reported_without_browser_retry() {
        let (url, calls) = fixture(
            StatusCode::FORBIDDEN,
            "<html><body>Access denied</body></html>",
        )
        .await;
        let opts = ScrapeOptions {
            engine: Some("http".into()),
            ..Default::default()
        };
        let err = scraper()
            .fetch_and_extract(&url, &opts, RobotsMode::Ignore)
            .await
            .unwrap_err();
        assert_eq!(err.code, ErrorCode::AccessBarrier);
        assert_eq!(calls.load(Ordering::SeqCst), 1);
        assert_eq!(
            err.details.unwrap()["itna"]["attempts"]
                .as_array()
                .unwrap()
                .len(),
            1
        );
    }

    #[tokio::test]
    async fn browser_fallback_waits_for_a_non_interactive_challenge() {
        if pagesift_cdp::detect_browser().is_none() || pagesift_cdp::running_as_root() {
            eprintln!("skipping: Chrome unavailable or running as root");
            return;
        }
        let app = Router::new().route(
            "/",
            get(|headers: axum::http::HeaderMap| async move {
                let ua = headers
                    .get("user-agent")
                    .and_then(|v| v.to_str().ok())
                    .unwrap_or("");
                if ua.contains("pagesift/") {
                    (
                        StatusCode::FORBIDDEN,
                        Html("<html><body>Access denied</body></html>"),
                    )
                } else {
                    (
                        StatusCode::OK,
                        Html(r#"<html><head><title>Just a moment...</title></head><body><p>Checking your browser</p><script>setTimeout(() => {
                            document.title = 'Ready';
                            document.body.innerHTML = '<article><h1>Resolved challenge</h1><p>The browser completed the non-interactive check and returned the article.</p></article>';
                        }, 2500);</script></body></html>"#),
                    )
                }
            }),
        );
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = listener.local_addr().unwrap();
        tokio::spawn(async move { axum::serve(listener, app).await.unwrap() });
        let opts = ScrapeOptions {
            timeout: Some(20_000),
            ..Default::default()
        };
        let doc = scraper()
            .fetch_and_extract(&format!("http://{addr}/"), &opts, RobotsMode::Ignore)
            .await
            .unwrap();
        assert!(doc.markdown.unwrap().contains("Resolved challenge"));
        assert_eq!(
            doc.metadata.pagesift.unwrap().itna.unwrap()["attempts"]
                .as_array()
                .unwrap()
                .len(),
            2
        );
    }

    #[tokio::test]
    async fn interactive_captcha_is_reported_for_a_human() {
        if pagesift_cdp::detect_browser().is_none() || pagesift_cdp::running_as_root() {
            eprintln!("skipping: Chrome unavailable or running as root");
            return;
        }
        let app = Router::new().route(
            "/",
            get(|headers: axum::http::HeaderMap| async move {
                let ua = headers
                    .get("user-agent")
                    .and_then(|v| v.to_str().ok())
                    .unwrap_or("");
                if ua.contains("pagesift/") {
                    (
                        StatusCode::FORBIDDEN,
                        Html("<html><body>Access denied</body></html>"),
                    )
                } else {
                    (
                        StatusCode::OK,
                        Html(r#"<html><head><title>Just a moment...</title></head><body><p>Verifying you are human</p><div class="g-recaptcha"></div></body></html>"#),
                    )
                }
            }),
        );
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = listener.local_addr().unwrap();
        tokio::spawn(async move { axum::serve(listener, app).await.unwrap() });
        let opts = ScrapeOptions {
            timeout: Some(20_000),
            ..Default::default()
        };
        let err = scraper()
            .fetch_and_extract(&format!("http://{addr}/"), &opts, RobotsMode::Ignore)
            .await
            .unwrap_err();
        assert_eq!(err.code, ErrorCode::AccessBarrier);
        let details = err.details.unwrap();
        assert!(
            matches!(details["kind"].as_str(), Some("captcha" | "challenge")),
            "{details}"
        );
        assert_eq!(details["needsHuman"], true);
    }

    #[tokio::test]
    async fn identities_are_reused_per_owner_and_isolated() {
        use axum::response::IntoResponse;
        let app = Router::new().route(
            "/",
            get(|headers: axum::http::HeaderMap| async move {
                let seen = headers
                    .get("cookie")
                    .and_then(|v| v.to_str().ok())
                    .unwrap_or("")
                    .contains("seen=1");
                if seen {
                    Html("<html><body><article><h1>Reused identity</h1><p>This article was fetched with the cookie from the previous request.</p></article></body></html>")
                        .into_response()
                } else {
                    (
                        [("set-cookie", "seen=1; Path=/; HttpOnly")],
                        Html("<html><body><article><h1>First visit</h1><p>This is the first article for this caller identity.</p></article></body></html>"),
                    )
                        .into_response()
                }
            }),
        );
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = listener.local_addr().unwrap();
        tokio::spawn(async move { axum::serve(listener, app).await.unwrap() });
        let url = format!("http://{addr}/");
        let scraper = scraper();
        let mut opts = ScrapeOptions {
            owner: Some("tenant-a".into()),
            ..Default::default()
        };
        let first = scraper
            .fetch_and_extract(&url, &opts, RobotsMode::Ignore)
            .await
            .unwrap();
        assert!(first.markdown.unwrap().contains("First visit"));
        let second = scraper
            .fetch_and_extract(&url, &opts, RobotsMode::Ignore)
            .await
            .unwrap();
        assert!(second.markdown.unwrap().contains("Reused identity"));
        opts.owner = Some("tenant-b".into());
        let other = scraper
            .fetch_and_extract(&url, &opts, RobotsMode::Ignore)
            .await
            .unwrap();
        assert!(other.markdown.unwrap().contains("First visit"));
    }

    #[tokio::test]
    async fn rate_limit_never_triggers_retry() {
        let (url, calls) = fixture(StatusCode::TOO_MANY_REQUESTS, "limited").await;
        let err = scraper()
            .fetch_and_extract(&url, &ScrapeOptions::default(), RobotsMode::Ignore)
            .await
            .unwrap_err();
        assert_eq!(err.code, ErrorCode::RateLimited);
        assert_eq!(calls.load(Ordering::SeqCst), 1);
        assert_eq!(
            err.details.unwrap()["itna"]["attempts"][0]["outcome"],
            "throttled"
        );
    }

    #[tokio::test]
    async fn retry_after_on_503_is_classified_as_throttled() {
        let app = Router::new()
            .route(
                "/",
                get(|| async {
                    (
                        StatusCode::SERVICE_UNAVAILABLE,
                        [("retry-after", "5")],
                        Html("<html><body><article><h1>Busy</h1><p>The origin asked the caller to retry later.</p></article></body></html>"),
                    )
                }),
            )
            .route(
                "/plain",
                get(|| async {
                    (
                        StatusCode::SERVICE_UNAVAILABLE,
                        Html("<html><body><article><h1>Down</h1><p>The origin is unavailable without a retry hint.</p></article></body></html>"),
                    )
                }),
            );
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = listener.local_addr().unwrap();
        tokio::spawn(async move { axum::serve(listener, app).await.unwrap() });
        let opts = ScrapeOptions {
            engine: Some("http".into()),
            ..Default::default()
        };
        let doc = scraper()
            .fetch_and_extract(&format!("http://{addr}/"), &opts, RobotsMode::Ignore)
            .await
            .unwrap();
        assert_eq!(doc.metadata.status_code, 503);
        assert!(doc.metadata.retry_after);
        assert_eq!(
            doc.metadata.pagesift.clone().unwrap().itna.unwrap()["attempts"][0]["outcome"],
            "throttled"
        );
        let plain = scraper()
            .fetch_and_extract(&format!("http://{addr}/plain"), &opts, RobotsMode::Ignore)
            .await
            .unwrap();
        assert_eq!(plain.metadata.status_code, 503);
        assert!(!plain.metadata.retry_after);
        assert_eq!(
            plain.metadata.pagesift.unwrap().itna.unwrap()["attempts"][0]["outcome"],
            "other"
        );
    }

    #[tokio::test]
    async fn barrier_falls_back_to_real_browser() {
        if pagesift_cdp::detect_browser().is_none() || pagesift_cdp::running_as_root() {
            eprintln!("skipping: Chrome unavailable or running as root");
            return;
        }
        let calls = Arc::new(AtomicUsize::new(0));
        let counter = calls.clone();
        let app = Router::new().route("/", get(move |headers: axum::http::HeaderMap| {
            let counter = counter.clone();
            async move {
                counter.fetch_add(1, Ordering::SeqCst);
                let ua = headers.get("user-agent").and_then(|v| v.to_str().ok()).unwrap_or("");
                if ua.contains("pagesift/") {
                    (StatusCode::FORBIDDEN, [("set-cookie", "probe=ok; Path=/; HttpOnly")], Html("<html><body>Access denied</body></html>"))
                } else {
                    let cookie = headers.get("cookie").and_then(|v| v.to_str().ok()).unwrap_or("");
                    if cookie.contains("probe=ok") {
                        (StatusCode::OK, [("set-cookie", "unused=1")], Html("<html><body><article><h1>Browser success</h1><p>This is the accessible article content.</p></article></body></html>"))
                    } else {
                        (StatusCode::FORBIDDEN, [("set-cookie", "unused=1")], Html("<html><body>Access denied</body></html>"))
                    }
                }
            }
        }));
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = listener.local_addr().unwrap();
        tokio::spawn(async move { axum::serve(listener, app).await.unwrap() });
        let opts = ScrapeOptions {
            timeout: Some(20_000),
            ..Default::default()
        };
        let result = scraper()
            .fetch_and_extract(&format!("http://{addr}/"), &opts, RobotsMode::Ignore)
            .await
            .unwrap();
        assert!(
            result
                .markdown
                .as_deref()
                .unwrap_or("")
                .contains("Browser success")
        );
        let trace = result.metadata.pagesift.unwrap().itna.unwrap();
        assert_eq!(trace["attempts"].as_array().unwrap().len(), 2);
        assert_eq!(trace["attempts"][1]["outcome"], "served");
        assert_eq!(calls.load(Ordering::SeqCst), 2);
    }

    #[tokio::test]
    async fn browser_cookie_bridge_blocks_same_host_other_port() {
        if pagesift_cdp::detect_browser().is_none() || pagesift_cdp::running_as_root() {
            eprintln!("skipping: Chrome unavailable or running as root");
            return;
        }
        let leaked = Arc::new(AtomicUsize::new(0));
        let other_count = leaked.clone();
        let other = Router::new().route(
            "/script.js",
            get(move || {
                let other_count = other_count.clone();
                async move {
                    other_count.fetch_add(1, Ordering::SeqCst);
                    "window.loaded=true"
                }
            }),
        );
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let other_addr = listener.local_addr().unwrap();
        tokio::spawn(async move { axum::serve(listener, other).await.unwrap() });
        let app = Router::new().route("/", get(move |headers: axum::http::HeaderMap| async move {
            let ua = headers.get("user-agent").and_then(|v| v.to_str().ok()).unwrap_or("");
            if ua.contains("pagesift/") {
                (StatusCode::FORBIDDEN, [("set-cookie", "secret=value; Path=/; HttpOnly")], Html("<html><body>Access denied</body></html>".to_string()))
            } else {
                (StatusCode::OK, [("set-cookie", "unused=1")], Html(format!("<html><body><article><h1>Origin protected</h1><p>Some legitimate article text.</p></article><script src=\"http://{other_addr}/script.js\"></script></body></html>")))
            }
        }));
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = listener.local_addr().unwrap();
        tokio::spawn(async move { axum::serve(listener, app).await.unwrap() });
        let opts = ScrapeOptions {
            timeout: Some(20_000),
            ..Default::default()
        };
        let doc = scraper()
            .fetch_and_extract(&format!("http://{addr}/"), &opts, RobotsMode::Ignore)
            .await
            .unwrap();
        assert!(doc.markdown.unwrap().contains("Origin protected"));
        assert_eq!(leaked.load(Ordering::SeqCst), 0);
    }

    #[tokio::test]
    async fn default_proxy_remains_bound_during_browser_fallback() {
        use tokio::io::{AsyncReadExt, AsyncWriteExt};
        if pagesift_cdp::detect_browser().is_none() || pagesift_cdp::running_as_root() {
            eprintln!("skipping: Chrome unavailable or running as root");
            return;
        }
        let proxy_hits = Arc::new(AtomicUsize::new(0));
        let app = Router::new().route("/", get(|headers: axum::http::HeaderMap| async move {
            let ua = headers.get("user-agent").and_then(|v| v.to_str().ok()).unwrap_or("");
            if ua.contains("pagesift/") {
                (StatusCode::FORBIDDEN, [("set-cookie", "probe=ok; Path=/")], Html("<html><body>Access denied</body></html>"))
            } else if headers.get("cookie").and_then(|v| v.to_str().ok()).unwrap_or("").contains("probe=ok") {
                (StatusCode::OK, [("set-cookie", "unused=1")], Html("<html><body><article><h1>Proxy identity preserved</h1><p>Article content delivered through the same proxy.</p></article></body></html>"))
            } else {
                (StatusCode::FORBIDDEN, [("set-cookie", "unused=1")], Html("<html><body>Access denied</body></html>"))
            }
        }));
        let origin = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let origin_addr = origin.local_addr().unwrap();
        tokio::spawn(async move { axum::serve(origin, app).await.unwrap() });
        let proxy = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let proxy_addr = proxy.local_addr().unwrap();
        let hits = proxy_hits.clone();
        tokio::spawn(async move {
            while let Ok((mut client, _)) = proxy.accept().await {
                let hits = hits.clone();
                tokio::spawn(async move {
                    let mut head = Vec::new();
                    loop {
                        if head.len() >= 16_384 {
                            return;
                        }
                        match client.read_u8().await {
                            Ok(b) => head.push(b),
                            Err(_) => return,
                        }
                        if head.ends_with(b"\r\n\r\n") {
                            break;
                        }
                    }
                    let Some(end) = head.windows(2).position(|w| w == b"\r\n") else {
                        return;
                    };
                    let line = String::from_utf8_lossy(&head[..end]);
                    let mut words = line.split_whitespace();
                    let (Some("GET"), Some(target), Some(version)) =
                        (words.next(), words.next(), words.next())
                    else {
                        return;
                    };
                    let Ok(url) = Url::parse(target) else { return };
                    if url.host_str() != Some("93.184.216.34") {
                        return;
                    }
                    if url.path() == "/" {
                        hits.fetch_add(1, Ordering::SeqCst);
                    }
                    let Ok(mut upstream) = tokio::net::TcpStream::connect(origin_addr).await else {
                        return;
                    };
                    let mut path = url.path().to_owned();
                    if let Some(q) = url.query() {
                        path.push('?');
                        path.push_str(q);
                    }
                    let request = format!("GET {path} {version}\r\n");
                    if upstream.write_all(request.as_bytes()).await.is_err()
                        || upstream.write_all(&head[end + 2..]).await.is_err()
                    {
                        return;
                    }
                    let _ = tokio::io::copy_bidirectional(&mut client, &mut upstream).await;
                });
            }
        });
        let mut cfg = ScraperConfig {
            itna: true,
            ..Default::default()
        };
        cfg.fetcher.env_proxy = false;
        cfg.fetcher.proxy = Some(format!("http://{proxy_addr}"));
        cfg.policy.rate.default_interval = Duration::ZERO;
        let scraper = Scraper::new(cfg).unwrap();
        let opts = ScrapeOptions {
            timeout: Some(20_000),
            ..Default::default()
        };
        let doc = scraper
            .fetch_and_extract("http://93.184.216.34:8080/", &opts, RobotsMode::Ignore)
            .await
            .unwrap();
        assert!(doc.markdown.unwrap().contains("Proxy identity preserved"));
        assert_eq!(proxy_hits.load(Ordering::SeqCst), 2);
    }
}
