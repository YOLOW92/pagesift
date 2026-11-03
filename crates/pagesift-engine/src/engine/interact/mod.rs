//! `/v2/scrape/{id}/interact`：保留的浏览器页面上继续执行动作。
//!
//! - `keepSession: true` 的抓取走完整的抓取流程，只是渲染完不关页面（[`KEEP`] 把页面带出来）；
//! - 没有要求保留的抓取只记下网址和选项，第一次 interact 时重新打开（Firecrawl 的回放），警告 `session_replayed`；
//! - 只用 Chrome：无障碍树、鼠标键盘事件只在 Chrome 上验证过；
//! - 点击可能跳到别的页面：动作前查当前页的 robots 并按主机限速，动作后跳走了再查一次。
//!
//! 回收的规则见 [`registry`]，`prompt` 的流程见 [`act`]。

use std::collections::BTreeSet;
use std::sync::Mutex;

use pagesift_api::types::{
    InteractRequest, InteractResponse, InteractStopped, SESSION_REPLAYED, session_gone,
};
use pagesift_cdp::KeptPage;
use serde_json::Value;

use super::*;

mod act;
mod plan;
mod registry;

pub use registry::InteractLimits;
pub(crate) use registry::Sessions;
use registry::{OpenPage, PageState, Record};

tokio::task_local! {
    /// 这次抓取要保留页面：渲染成功后页面放进这里，交回给 [`Scraper::open_page`]。
    /// 用 task-local 是因为页面要穿过整条抓取流程（钩子、缓存、审计），它不是抓取选项的一部分；
    /// 抓取流程到 `render` 之间没有 spawn。
    pub(super) static KEEP: Arc<KeepSlot>;
}

#[derive(Default)]
pub(super) struct KeepSlot(Mutex<Option<(KeptPage, Arc<BrowserPool>)>>);

impl KeepSlot {
    /// 换内核或换出口重试时后来的覆盖先来的（旧页面丢弃时自己关）。
    pub(super) fn put(&self, page: KeptPage, pool: Arc<BrowserPool>) {
        *self.0.lock().unwrap_or_else(|e| e.into_inner()) = Some((page, pool));
    }

    fn take(&self) -> Option<(KeptPage, Arc<BrowserPool>)> {
        self.0.lock().unwrap_or_else(|e| e.into_inner()).take()
    }
}

/// 这个内核能不能做 interact。
pub(super) fn interact_capable(cfg: &PoolConfig) -> bool {
    cfg.engine.unwrap_or(pagesift_cdp::EngineKind::Chrome) == pagesift_cdp::EngineKind::Chrome
}

/// 记录表的字节上限：内核链里最大的“一个浏览器加一个标签”的内存（[`registry`]）。
pub(super) fn record_budget(chain: &[PoolConfig]) -> usize {
    let mb = chain
        .iter()
        .map(|c| {
            let f = c
                .engine
                .unwrap_or(pagesift_cdp::EngineKind::Chrome)
                .footprint();
            f.browser_mb + f.tab_mb
        })
        .max()
        .unwrap_or(0) as usize;
    mb * 1024 * 1024
}

/// 把用过的秘密的值换回占位符。
fn redact(text: &str, secrets: &[(&str, &str)]) -> String {
    let mut out = text.to_string();
    for (name, value) in secrets {
        if !value.is_empty() && out.contains(value) {
            out = out.replace(value, &format!("{{{{secret:{name}}}}}"));
        }
    }
    out
}

fn redact_json(v: &mut Value, secrets: &[(&str, &str)]) {
    match v {
        Value::String(s) => *s = redact(s, secrets),
        Value::Array(a) => a.iter_mut().for_each(|x| redact_json(x, secrets)),
        Value::Object(m) => m.values_mut().for_each(|x| redact_json(x, secrets)),
        _ => {}
    }
}

/// 文字里出现的 `{{secret:NAME}}` 的名字。
fn secret_names(text: &str) -> impl Iterator<Item = &str> {
    text.split("{{secret:")
        .skip(1)
        .filter_map(|rest| rest.split_once("}}").map(|(name, _)| name))
}

impl Scraper {
    /// 能不能做 interact：内核链里有 Chrome。
    pub(super) fn interact_available(&self) -> bool {
        self.browsers.iter().any(|(c, _)| interact_capable(c))
    }

    fn interact_unavailable(&self) -> ApiError {
        ApiError::unsupported(
            "interact needs Chrome: no Chrome or Chromium was found on this machine (run `pagesift doctor`)",
        )
    }

    /// 内核链里第一个能做 interact 的池。
    async fn interact_pool(&self) -> Result<Arc<BrowserPool>, ApiError> {
        for i in 0..self.browsers.len() {
            if interact_capable(&self.browsers[i].0)
                && let Some(pool) = self.browser(i).await
            {
                return Ok(pool);
            }
        }
        Err(self.interact_unavailable())
    }

    /// 按内存和标签名额腾出一个保留页面的位置；腾不出来时报 429。
    async fn admit_page(&self) -> Result<(), ApiError> {
        let pool = self.interact_pool().await?;
        let cap = self.sessions.sweep(Some(pool.session_room()), 1).await;
        let open = self.sessions.open_count();
        if open >= cap {
            return Err(ApiError::new(
                ErrorCode::ConcurrencyLimitExceeded,
                format!(
                    "cannot keep another browser session: {open} are open and in use, and this machine's memory and browser tabs admit {cap}"
                ),
            ));
        }
        Ok(())
    }

    /// 走完整的抓取流程打开页面，不关。
    async fn open_page(
        &self,
        url: &str,
        opts: &ScrapeOptions,
        mode: RobotsMode,
    ) -> Result<(Document, KeptPage, Arc<BrowserPool>), ApiError> {
        let mut inner = opts.clone();
        inner.engine = Some("browser".into());
        // 会话要的是一个活的页面，缓存里没有
        inner.max_age = Some(0);
        inner.store_in_cache = false;
        let slot = Arc::new(KeepSlot::default());
        let doc = KEEP
            .scope(slot.clone(), self.scrape_with(url, &inner, mode))
            .await?;
        let (page, pool) = slot.take().ok_or_else(|| {
            ApiError::unsupported(
                "this page was answered without a browser (a rule chose the engine, or the content is not a web page), so there is no session to keep",
            )
        })?;
        Ok((doc, page, pool))
    }

    fn opened(
        &self,
        page: KeptPage,
        pool: Arc<BrowserPool>,
        doc: &Document,
        url: &str,
    ) -> Box<OpenPage> {
        self.sessions.start_reaper();
        let now = Instant::now();
        Box::new(OpenPage {
            page,
            pool,
            opened: now,
            last_used: now,
            url: doc.metadata.url.clone().unwrap_or_else(|| url.to_string()),
            secrets: BTreeSet::new(),
        })
    }

    /// `/v2/scrape`：`keep` 时保留页面；否则只记下回放信息。结果带 `metadata.scrapeId`。
    pub async fn scrape_session(
        &self,
        url: &str,
        opts: &ScrapeOptions,
        keep: bool,
        owner: Option<&str>,
    ) -> Result<Document, ApiError> {
        if !self.interact_available() {
            if keep {
                return Err(self.interact_unavailable());
            }
            return self.scrape(url, opts).await;
        }
        let mode = self
            .policy
            .robots_mode(RequestKind::Scrape, opts.ignore_robots_txt)
            .map_err(policy_error)?;
        if !keep {
            let mut doc = self.scrape_with(url, opts, mode).await?;
            let id = self
                .sessions
                .insert(None, owner, url, opts, mode, PageState::NotOpened);
            doc.metadata.scrape_id = Some(id);
            return Ok(doc);
        }
        if opts.engine.as_deref() == Some("http") {
            return Err(ApiError::invalid(
                "keepSession keeps a browser page and cannot be combined with engine \"http\"",
            ));
        }
        self.admit_page().await?;
        let (mut doc, page, pool) = self.open_page(url, opts, mode).await?;
        let open = self.opened(page, pool, &doc, url);
        let id = self
            .sessions
            .insert(None, owner, url, opts, mode, PageState::Open(open));
        doc.metadata.scrape_id = Some(id);
        Ok(doc)
    }

    /// 没有这个 id 的记录、但请求带了 `url`：Firecrawl 的“原来的上下文没有了，在这个网址开新会话”。
    fn record_for(
        &self,
        id: &str,
        req: &InteractRequest,
        owner: Option<&str>,
    ) -> Result<Arc<Record>, ApiError> {
        match (self.sessions.get(id, owner), req.url.as_deref()) {
            (Ok(r), _) => Ok(r),
            (Err(e), Some(url))
                if matches!(e.code, ErrorCode::SessionGone | ErrorCode::NotFound)
                    && pagesift_api::types::is_scrape_id(id) =>
            {
                let opts = ScrapeOptions::default();
                let mode = self
                    .policy
                    .robots_mode(RequestKind::Scrape, opts.ignore_robots_txt)
                    .map_err(policy_error)?;
                self.sessions
                    .insert(Some(id), owner, url, &opts, mode, PageState::NotOpened);
                self.sessions.get(id, owner)
            }
            (Err(e), _) => Err(e),
        }
    }

    pub async fn interact(
        &self,
        id: &str,
        req: &InteractRequest,
        owner: Option<&str>,
    ) -> Result<InteractResponse, ApiError> {
        req.check()?;
        if !self.interact_available() {
            return Err(self.interact_unavailable());
        }
        if req.prompt.is_some() && self.llm.is_none() {
            return Err(ApiError::unsupported(
                "prompt needs an LLM: set [llm] in pagesift.toml, or send `actions`",
            ));
        }
        let actions = req.actions.clone().unwrap_or_default();
        let record = self.record_for(id, req, owner)?;
        let mut resp = InteractResponse::default();
        let limit = req.time_limit();
        let deadline = tokio::time::Instant::now() + limit;
        let timed_out = |resp: &mut InteractResponse| {
            resp.killed = true;
            resp.fail(format!(
                "the call did not finish within {} s; the session is still open",
                limit.as_secs_f64()
            ));
        };
        // 同一个会话上的调用排队执行：页面只有一个
        let Ok(mut state) = tokio::time::timeout_at(deadline, record.state.lock()).await else {
            timed_out(&mut resp);
            return Ok(resp);
        };
        if let PageState::Gone(why) = &*state {
            return Err(session_gone(format!(
                "the browser session of scrape {id} has ended: {why}"
            )));
        }
        if matches!(&*state, PageState::NotOpened) {
            let opts = replay_options(&record.opts, limit);
            let opening = async {
                self.admit_page().await?;
                self.open_page(&record.url, &opts, record.mode).await
            };
            let (doc, page, pool) = match tokio::time::timeout_at(deadline, opening).await {
                Ok(r) => r?,
                Err(_) => {
                    timed_out(&mut resp);
                    return Ok(resp);
                }
            };
            *state = PageState::Open(self.opened(page, pool, &doc, &record.url));
            self.sessions.mark_open(id);
            resp.warnings.push(format!(
                "{SESSION_REPLAYED}: the scrape did not keep its page (keepSession), so {} was opened again; the page may differ from the scrape result",
                record.url
            ));
        }
        let PageState::Open(open) = &mut *state else {
            return Err(session_gone(format!(
                "the browser session of scrape {id} has ended"
            )));
        };
        let run = async {
            match &req.prompt {
                Some(prompt) => self.run_prompt(&record, open, prompt, &mut resp).await,
                None => self.run_actions(&record, open, actions, &mut resp).await,
            }
        };
        let outcome = tokio::time::timeout_at(deadline, run).await;
        open.last_used = Instant::now();
        let alive = open.page.is_alive();
        let redacting: Vec<(String, String)> = open
            .secrets
            .iter()
            .filter_map(|n| self.secrets.get(n).map(|v| (n.clone(), v.clone())))
            .collect();
        match outcome {
            Err(_) => timed_out(&mut resp),
            Ok(Ok(())) => {}
            Ok(Err(e)) => {
                let closes = e.code == ErrorCode::RobotsDisallowed || !alive;
                if closes {
                    let why = if alive {
                        "an action navigated to a page robots.txt disallows".to_string()
                    } else {
                        "the browser exited".to_string()
                    };
                    let old = std::mem::replace(&mut *state, PageState::Gone(why.clone()));
                    drop(state);
                    if let PageState::Open(o) = old {
                        o.page.close().await;
                    }
                    self.sessions.mark_closed(id);
                    return Err(if alive {
                        e
                    } else {
                        session_gone(format!(
                            "the browser session of scrape {id} has ended: {why}"
                        ))
                    });
                }
                return Err(e);
            }
        }
        drop(state);
        finish_response(resp, &redacting)
    }

    pub async fn stop_interact(
        &self,
        id: &str,
        owner: Option<&str>,
    ) -> Result<InteractStopped, ApiError> {
        let not_found = |detail: &str| {
            ApiError::new(
                ErrorCode::NotFound,
                format!("no browser session for scrape {id}: {detail}"),
            )
        };
        let record = self.sessions.get(id, owner).map_err(|e| match e.code {
            // Firecrawl 的 DELETE 只有 403、404：查不到就是 404，原因写在消息里
            ErrorCode::SessionGone => ApiError::new(ErrorCode::NotFound, e.message),
            _ => e,
        })?;
        let mut state = record.state.lock().await;
        match &*state {
            PageState::NotOpened => return Err(not_found("no session was started")),
            PageState::Gone(why) => return Err(not_found(&format!("it has ended: {why}"))),
            PageState::Open(_) => {}
        }
        let old = std::mem::replace(&mut *state, PageState::Gone("it was stopped".into()));
        drop(state);
        let PageState::Open(open) = old else {
            return Err(not_found("no session was started"));
        };
        let held = open.opened.elapsed();
        open.page.close().await;
        self.sessions.mark_closed(id);
        Ok(InteractStopped {
            success: true,
            session_duration_ms: held.as_millis() as u64,
            credits_billed: 0,
        })
    }

    /// 动作前：查当前页的 robots，按主机限速；限速名额拿到动作结束。
    async fn polite_permit(
        &self,
        record: &Record,
        current: &str,
    ) -> Result<pagesift_policy::Permit, ApiError> {
        let target = Url::parse(current).map_err(|e| ApiError::invalid(e.to_string()))?;
        let delay = match record.mode {
            RobotsMode::Respect => self
                .policy
                .check_robots(&target)
                .await
                .map_err(policy_error)?,
            RobotsMode::Ignore => None,
        };
        let host = target.host_str().unwrap_or_default().to_string();
        Ok(self.policy.limiter().acquire(&host, delay).await)
    }

    async fn run_actions(
        &self,
        record: &Record,
        open: &mut OpenPage,
        mut actions: Vec<Value>,
        resp: &mut InteractResponse,
    ) -> Result<(), ApiError> {
        for a in &actions {
            if let Some(text) = a.get("text").and_then(Value::as_str) {
                open.secrets.extend(secret_names(text).map(str::to_string));
            }
        }
        super::autoschema::fill_secrets(&mut actions, &self.secrets)?;
        let parsed = pagesift_cdp::parse_actions(&Value::Array(actions))
            .map_err(|e| ApiError::invalid(e.to_string()))?;
        let permit = self.polite_permit(record, &open.url.clone()).await?;
        let started = Instant::now();
        match open.page.run_actions(&parsed).await {
            Ok(out) => {
                resp.result = out.javascript_returns.last().map(|j| j.value.to_string());
                resp.actions = Some(out.to_firecrawl_json());
            }
            Err(e) => action_failure(e, resp)?,
        }
        drop(permit);
        self.fill_page(record, open, resp, started).await
    }

    /// 动作之后的页面：跳走了再查一次 robots，然后转成文档放进响应。
    async fn fill_page(
        &self,
        record: &Record,
        open: &mut OpenPage,
        resp: &mut InteractResponse,
        started: Instant,
    ) -> Result<(), ApiError> {
        let mut page = match open.page.capture().await {
            Ok(p) => p,
            Err(e) => {
                // 这次的动作结果比取不到页面更重要：错误如实写上，已有的失败原因不覆盖
                if open.page.is_alive() {
                    if resp.error.is_none() {
                        resp.fail(format!("could not read the page after the actions: {e}"));
                    }
                    return Ok(());
                }
                return Err(ApiError::new(render_error_code(&e), e.to_string()));
            }
        };
        if page.final_url != open.url {
            if record.mode == RobotsMode::Respect
                && let Ok(u) = Url::parse(&page.final_url)
            {
                self.policy.check_robots(&u).await.map_err(policy_error)?;
            }
            open.url = page.final_url.clone();
        }
        let secrets = self.named_secrets(&open.secrets);
        if !secrets.is_empty() {
            // 转 Markdown 前就换掉：转义之后可能对不上
            page.html = redact(&page.html, &secrets);
        }
        let opts = page_options(&record.opts);
        let doc = self
            .live_document(page, &record.url, &opts, Vec::new(), started)
            .await?;
        resp.data = Some(doc);
        Ok(())
    }

    fn named_secrets<'a>(&'a self, names: &'a BTreeSet<String>) -> Vec<(&'a str, &'a str)> {
        names
            .iter()
            .filter_map(|n| self.secrets.get(n).map(|v| (n.as_str(), v.as_str())))
            .collect()
    }
}

/// 动作失败：选择器找不到之类写进响应（会话还在），其余按错误类别报。
fn action_failure(e: pagesift_cdp::Error, resp: &mut InteractResponse) -> Result<(), ApiError> {
    use pagesift_cdp::Error as E;
    match e {
        E::Action { .. } | E::Protocol { .. } => {
            resp.fail(e.to_string());
            Ok(())
        }
        E::InvalidOptions(m) => Err(ApiError::invalid(m)),
        E::ExecuteJavascriptDisabled => Err(ApiError::unsupported(e.to_string())),
        other => Err(ApiError::new(render_error_code(&other), other.to_string())),
    }
}

/// 回放用的选项：时限不超过这次调用的时限。
fn replay_options(opts: &ScrapeOptions, limit: Duration) -> ScrapeOptions {
    let mut o = opts.clone();
    let ms = limit.as_millis() as u64;
    o.timeout = Some(o.timeout.map_or(ms, |t| t.min(ms)));
    o
}

/// 动作之后取页面用的选项：原来的格式里不调模型、不靠缓存的那些；一个都没有时给 Markdown。
fn page_options(opts: &ScrapeOptions) -> ScrapeOptions {
    let mut o = opts.clone();
    o.formats.retain(|f| {
        matches!(
            f.kind.as_str(),
            "markdown" | "html" | "rawHtml" | "links" | "images"
        )
    });
    if o.formats.is_empty() {
        o.formats.push(pagesift_api::types::Format::new("markdown"));
    }
    o
}

/// 响应里用过的秘密换回占位符：整页、动作产出、错误信息和网址都可能带上（输入框的内容、表单 GET 的查询串）。
fn finish_response(
    resp: InteractResponse,
    secrets: &[(String, String)],
) -> Result<InteractResponse, ApiError> {
    if secrets.is_empty() {
        return Ok(resp);
    }
    let pairs: Vec<(&str, &str)> = secrets
        .iter()
        .map(|(n, v)| (n.as_str(), v.as_str()))
        .collect();
    let mut v = serde_json::to_value(&resp)
        .map_err(|e| ApiError::new(ErrorCode::Internal, e.to_string()))?;
    redact_json(&mut v, &pairs);
    serde_json::from_value(v).map_err(|e| ApiError::new(ErrorCode::Internal, e.to_string()))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn secrets_are_named_and_redacted() {
        let names: Vec<&str> = secret_names("log in as {{secret:USER}} / {{secret:PW}}").collect();
        assert_eq!(names, ["USER", "PW"]);
        assert_eq!(secret_names("nothing here").count(), 0);
        let pairs = [("PW", "hunter2"), ("EMPTY", "")];
        assert_eq!(redact("value=hunter2;", &pairs), "value={{secret:PW}};");
        let mut v = serde_json::json!({"a": ["hunter2"], "b": {"c": "x hunter2"}, "n": 1});
        redact_json(&mut v, &pairs);
        assert_eq!(
            v,
            serde_json::json!({"a": ["{{secret:PW}}"], "b": {"c": "x {{secret:PW}}"}, "n": 1})
        );
    }

    #[test]
    fn page_options_drop_llm_formats() {
        let o: ScrapeOptions =
            serde_json::from_value(serde_json::json!({"formats": ["summary", "links"]})).unwrap();
        let kinds: Vec<_> = page_options(&o)
            .formats
            .iter()
            .map(|f| f.kind.clone())
            .collect();
        assert_eq!(kinds, ["links"]);
        let o: ScrapeOptions =
            serde_json::from_value(serde_json::json!({"formats": ["summary"]})).unwrap();
        assert!(page_options(&o).wants("markdown"));
    }
}
