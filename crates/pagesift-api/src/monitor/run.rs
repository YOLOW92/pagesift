//! 监控的调度和一次检查：到期的网址抓一遍（带条件请求），和上次的内容做块级 diff，
//! 更新变化率后验、算下次到期时间，记检查结果，发 webhook，落盘。

use std::collections::HashSet;
use std::sync::atomic::Ordering;
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use pagesift_stat::poisson::ChangeRate;
use serde_json::{Value, json};
use tokio::task::JoinSet;

use super::crawl::check_crawl;
use super::{Check, Leader, MonitorState, PageResult, PageState};
use crate::error::ErrorCode;
use crate::limits::Caller;
use crate::server::AppState;
use crate::types::{Document, ScrapeOptions};

/// [δ] 全局风险水平：下次检查放在“漏掉变化的后验概率”到 δ 的时刻。
const DELTA: f64 = 0.05;
/// [策略] 每个网址记多少次“变了”的区间：够后验收敛，计算量也不会一直涨
const MAX_CHANGE_HISTORY: usize = 1024;

pub(crate) fn now_secs() -> f64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_or(0.0, |d| d.as_secs_f64())
}

pub(crate) fn rfc3339_secs(t: f64) -> String {
    let at = Duration::try_from_secs_f64(t.max(0.0))
        .ok()
        .and_then(|d| UNIX_EPOCH.checked_add(d))
        .unwrap_or(UNIX_EPOCH);
    crate::jobs::rfc3339(at)
}

fn key(id: &str) -> String {
    format!("monitor/{id}")
}

pub(super) fn persist(s: &AppState, m: &MonitorState) {
    if let (Some(store), Ok(v)) = (s.jobs.store(), serde_json::to_vec(m))
        && let Err(e) = store.tasks().kv_put(key(&m.id), &v)
    {
        tracing::warn!(monitor = %m.id, error = %e, "could not save the monitor");
    }
}

pub(super) fn forget(s: &AppState, id: &str) {
    if let Some(store) = s.jobs.store() {
        let _ = store.tasks().kv_delete(key(id));
    }
}

/// 读回落盘的监控；有监控时启动调度。`leader` 为假时（多服务器、本机不持有网关租约）不检查，只等。
pub(crate) fn start_scheduler(s: AppState, leader: Option<Leader>) {
    *s.monitors.leader.lock().unwrap_or_else(|e| e.into_inner()) = leader;
    if let Some(store) = s.jobs.store()
        && let Ok(keys) = store.tasks().kv_keys("monitor/")
    {
        let mut map = s
            .monitors
            .map
            .try_lock()
            .expect("monitors are not shared yet");
        for k in keys {
            let parsed = match store.tasks().kv_get(&k) {
                Ok(Some(v)) => {
                    serde_json::from_slice::<MonitorState>(&v).map_err(|e| e.to_string())
                }
                Ok(None) => continue,
                Err(e) => Err(e.to_string()),
            };
            match parsed {
                Ok(m) => {
                    map.insert(m.id.clone(), m);
                }
                // 记录留在存储里，不删：修好之后还能读回来
                Err(error) => {
                    tracing::error!(record = %String::from_utf8_lossy(&k), %error, "could not read a saved monitor; it is not scheduled");
                    s.metrics
                        .monitor_load_failures
                        .fetch_add(1, std::sync::atomic::Ordering::Relaxed);
                }
            }
        }
        if map.is_empty() {
            return;
        }
    } else {
        return;
    }
    ensure_running(&s);
}

/// 没有在跑的调度任务时启动一个。监控全部删掉后它自己退出（释放它持有的服务状态和存储）。
pub(crate) fn ensure_running(s: &AppState) {
    if s.monitors.running.swap(true, Ordering::SeqCst) {
        return;
    }
    let s = s.clone();
    let leader = s
        .monitors
        .leader
        .lock()
        .unwrap_or_else(|e| e.into_inner())
        .clone();
    tokio::spawn(async move {
        let _flag = RunningFlag(s.monitors.running.clone());
        // 每个监控的检查各跑各的：一个爬得久、webhook 不通的监控不耽误别人到期
        let mut checks: JoinSet<(String, bool)> = JoinSet::new();
        let mut busy: HashSet<String> = HashSet::new();
        loop {
            while let Some(done) = checks.try_join_next() {
                settle(&s, &mut busy, done).await;
            }
            let due = {
                let map = s.monitors.map.lock().await;
                if map.is_empty() && checks.is_empty() {
                    s.monitors.running.store(false, Ordering::SeqCst);
                    return;
                }
                map.values()
                    .filter(|m| m.status == "active" && !busy.contains(&m.id))
                    .filter_map(MonitorState::next_due)
                    .fold(f64::INFINITY, f64::min)
            };
            let wait = (due - now_secs()).max(0.0);
            if wait > 0.0 {
                let sleep = Duration::try_from_secs_f64(wait).unwrap_or(Duration::MAX);
                tokio::select! {
                    () = tokio::time::sleep(sleep.min(Duration::from_secs(86_400))) => {}
                    () = s.monitors.wake.notified() => continue,
                    Some(done) = checks.join_next(), if !checks.is_empty() => {
                        settle(&s, &mut busy, done).await;
                        continue;
                    }
                }
            }
            if leader.as_ref().is_some_and(|l| !l()) {
                // 不是调度者：过一个最短间隔再看租约（没有哪个网址会比最短间隔更早到期两次）
                tokio::time::sleep(Duration::from_secs(super::schedule::MIN_INTERVAL_SECS)).await;
                continue;
            }
            let ids: Vec<String> = {
                let map = s.monitors.map.lock().await;
                let now = now_secs();
                map.values()
                    .filter(|m| m.status == "active" && !busy.contains(&m.id))
                    .filter(|m| m.next_due().is_some_and(|d| d <= now))
                    .map(|m| m.id.clone())
                    .collect()
            };
            for id in ids {
                busy.insert(id.clone());
                let s = s.clone();
                checks.spawn(async move {
                    let task = id.clone();
                    let panicked = tokio::spawn(async move { run_check(&s, &task).await })
                        .await
                        .is_err();
                    (id, panicked)
                });
            }
        }
    });
}

/// 调度任务万一 panic 也要放开 `running`，下一个请求才能再启动它。正常退出时已经放开了，不再动。
struct RunningFlag(std::sync::Arc<std::sync::atomic::AtomicBool>);

impl Drop for RunningFlag {
    fn drop(&mut self) {
        if std::thread::panicking() {
            self.0.store(false, Ordering::SeqCst);
        }
    }
}

/// 一次检查结束。panic 的监控停下来：它的到期时间没动，不停就会立刻再 panic 一次，重启后也一样。
async fn settle(
    s: &AppState,
    busy: &mut HashSet<String>,
    done: Result<(String, bool), tokio::task::JoinError>,
) {
    let Ok((id, panicked)) = done else { return };
    busy.remove(&id);
    if !panicked {
        return;
    }
    tracing::error!(monitor = %id, "a monitor check panicked; the monitor is paused");
    let mut map = s.monitors.map.lock().await;
    if let Some(m) = map.get_mut(&id) {
        m.status = "paused".into();
        m.warnings
            .push("monitor_paused: a check crashed, so this monitor stopped running".into());
        persist(s, m);
    }
}

/// 一次检查：只查到期的网址（自适应时各网址的到期时间不同）。
pub(crate) async fn run_check(s: &AppState, id: &str) {
    let Some(snapshot) = s.monitors.map.lock().await.get(id).cloned() else {
        return;
    };
    let started = now_secs();
    let due: Vec<(String, PageState)> = snapshot
        .pages
        .iter()
        .filter(|(_, p)| p.next_due <= started)
        .map(|(u, p)| (u.clone(), p.clone()))
        .collect();
    let mut results = Vec::new();
    let mut updates: Vec<(String, PageState)> = Vec::new();
    let substantive_only = snapshot.req.notify.as_deref() != Some("any");
    let caller = s.monitors.caller(&snapshot.owner);
    for (url, prev) in due {
        let target = snapshot
            .req
            .targets
            .iter()
            .find(|t| t.id.as_deref() == Some(prev.target.as_str()));
        let Some(target) = target else { continue };
        let opts = target.scrape_options.clone().unwrap_or_default();
        // 配额用完就不抓：这次没有观测，到下一个间隔再试
        if let Err(e) = s.limiter.check_quota(&caller) {
            let mut next = prev.clone();
            observe(&snapshot, &mut next, started, None);
            results.push(failure(&url, &e));
            updates.push((url, next));
            continue;
        }
        if let Some(seed) = url.strip_prefix("crawl:").and(target.url.clone()) {
            let (pages, observed) = check_crawl(
                s,
                &snapshot,
                &seed,
                target,
                &opts,
                substantive_only,
                &caller,
            )
            .await;
            let mut next = prev.clone();
            observe(&snapshot, &mut next, started, observed);
            updates.push((url, next));
            for (u, st, r) in pages {
                updates.push((u, st));
                results.push(r);
            }
            continue;
        }
        let (result, next) = check_page(s, &url, &prev, &opts, substantive_only, &caller).await;
        let mut next = next;
        observe(&snapshot, &mut next, started, observation(&result));
        updates.push((url, next));
        results.push(result);
    }
    let check = Check {
        id: uuid::Uuid::new_v4().to_string(),
        started_at: rfc3339_secs(started),
        finished_at: rfc3339_secs(now_secs()),
        status: match results.iter().filter(|r| r.status == "error").count() {
            0 => "completed".into(),
            n if n == results.len() => "failed".into(),
            _ => "partial".into(),
        },
        pages: results,
    };
    let saved = {
        let mut map = s.monitors.map.lock().await;
        let Some(m) = map.get_mut(id) else { return };
        for (u, p) in updates {
            m.pages.insert(u, p);
        }
        allocate(m);
        m.last_run_at = Some(check.finished_at.clone());
        m.checks.push_back(check.clone());
        let keep_secs = f64::from(
            m.req
                .retention_days
                .unwrap_or(super::DEFAULT_RETENTION_DAYS),
        ) * 86_400.0;
        while m.checks.len() > 1
            && m.checks.front().is_some_and(|c| {
                parse_rfc3339(&c.started_at).is_some_and(|t| started - t > keep_secs)
            })
        {
            m.checks.pop_front();
        }
        while m.checks.len() > super::MAX_CHECKS_KEPT {
            m.checks.pop_front();
        }
        m.clone()
    };
    // 序列化和写盘可能很久（状态里有页面全文），不占异步线程
    let writer = s.clone();
    let saved = tokio::task::spawn_blocking(move || {
        persist(&writer, &saved);
        saved
    })
    .await;
    let Ok(saved) = saved else { return };
    notify(s, &saved, &check).await;
}

fn parse_rfc3339(s: &str) -> Option<f64> {
    // 只解析本模块写的 `YYYY-MM-DDTHH:MM:SS(.fff)Z`
    // [协议] 下面的下标是 RFC 3339 各字段的位置和宽度；日期换算的系数来自 Howard Hinnant,
    // "chrono-Compatible Low-Level Date Algorithms"（days_from_civil）
    let b = s.as_bytes();
    let num = |a: usize, n: usize| -> Option<i64> {
        std::str::from_utf8(b.get(a..a + n)?).ok()?.parse().ok()
    };
    let (y, mo, d) = (num(0, 4)?, num(5, 2)?, num(8, 2)?); // [协议] RFC 3339 §5.6
    let (h, mi, se) = (num(11, 2)?, num(14, 2)?, num(17, 2)?); // [协议] RFC 3339 §5.6
    // 公历日期到 Unix 天数（Howard Hinnant, "chrono-Compatible Low-Level Date Algorithms"）
    let y = if mo <= 2 { y - 1 } else { y }; // [推导] days_from_civil
    let era = y.div_euclid(400); // [推导] days_from_civil
    let yoe = y - era * 400; // [推导] days_from_civil
    let doy = (153 * ((mo + 9) % 12) + 2) / 5 + d - 1; // [推导] days_from_civil
    let doe = yoe * 365 + yoe / 4 - yoe / 100 + doy; // [推导] days_from_civil
    let days = era * 146_097 + doe - 719_468; // [推导] days_from_civil
    Some((days * 86_400 + h * 3600 + mi * 60 + se) as f64)
}

/// 自适应监控：δ 规则给出每个网址的间隔 t_i（漏掉变化的后验概率到 δ），总请求率 R = Σ 1/t_i 就是
/// 这个监控的预算；在同一预算下让平均发现延迟最小的分配是 T_i = (Σ√λ_j) / (R·√λ_i)
/// （最小化 Σ λ_i T_i / 2，约束 Σ 1/T_i = R，拉格朗日乘子法；由 Cauchy–Schwarz 不比均匀轮询差）。
/// 只有一个网址时 T = t，和 δ 规则相同。结果夹在最短、最长间隔之间。
pub(super) fn allocate(m: &mut MonitorState) {
    if m.period.is_some() {
        return;
    }
    let live: Vec<(f64, f64)> = m
        .pages
        .values()
        .filter(|p| p.next_due.is_finite())
        .filter_map(|p| Some((p.rate_mean?, p.delta_interval?)))
        .collect();
    let min = m.min_interval as f64;
    let budget: f64 = live.iter().map(|(_, t)| 1.0 / t.max(min)).sum();
    let s: f64 = live.iter().map(|(l, _)| l.sqrt()).sum();
    if budget <= 0.0 || s <= 0.0 {
        return;
    }
    for p in m.pages.values_mut().filter(|p| p.next_due.is_finite()) {
        let (Some(l), Some(last)) = (p.rate_mean, p.last_attempt.or(p.last_check)) else {
            continue;
        };
        let t = (s / (budget * l.sqrt())).max(min);
        let t = m.max_interval.map_or(t, |mx| t.min(mx as f64));
        p.next_due = last + t.min(super::schedule::MAX_INTERVAL_SECS as f64);
    }
}

/// 这次检查对变化率的观测：抓取失败什么也没看到（`None`），不算“没变”。
fn observation(r: &PageResult) -> Option<bool> {
    match r.status.as_str() {
        "error" => None,
        "same" => Some(false),
        _ => Some(true),
    }
}

/// 一个网址这次没有结果。
pub(super) fn failure(url: &str, e: &crate::ApiError) -> PageResult {
    PageResult {
        url: url.into(),
        status: "error".into(),
        error: Some(format!("{}: {}", e.code.as_str(), e.message)),
        noise_only: false,
        diff: None,
        field_changes: Vec::new(),
        not_modified: false,
    }
}

/// 记一次观测（`None`：没有结果，变化率不动），算下次到期时间。
pub(super) fn observe(m: &MonitorState, p: &mut PageState, now: f64, changed: Option<bool>) {
    let mut rate = ChangeRate::default();
    if let Some(e) = p.exposure {
        rate.exposure = e;
    }
    rate.changed = p.changed_intervals.clone();
    if let Some(changed) = changed {
        if let Some(last) = p.last_check {
            rate.observe(now - last, changed);
        }
        p.last_check = Some(now);
    }
    // 后验的计算量随变化次数线性增长。太长时忘掉较早的一半，变和没变按同样比例忘，估计不偏
    if rate.changed.len() > MAX_CHANGE_HISTORY {
        let half = rate.changed.len() / 2;
        rate.changed.drain(..half);
        rate.exposure /= 2.0;
    }
    p.exposure = Some(rate.exposure);
    p.changed_intervals = rate.changed.clone();
    p.rate_mean = Some(rate.mean());
    p.last_attempt = Some(now);
    let interval = match m.period {
        Some(period) => period as f64,
        None => {
            let t = rate.next_check(DELTA);
            p.delta_interval = Some(t);
            let t = t.max(m.min_interval as f64);
            m.max_interval.map_or(t, |mx| t.min(mx as f64))
        }
    };
    p.next_due = now + interval.min(super::schedule::MAX_INTERVAL_SECS as f64);
}

async fn check_page(
    s: &AppState,
    url: &str,
    prev: &PageState,
    opts: &ScrapeOptions,
    substantive_only: bool,
    caller: &Caller,
) -> (PageResult, PageState) {
    let mut o = opts.clone();
    o.owner = Some(caller.account.to_string()).filter(|owner| owner != "anonymous");
    o.max_age = Some(0);
    if prev.markdown.is_some() {
        o.if_none_match = prev.etag.clone();
        o.if_modified_since = prev.last_modified.clone();
    }
    let mut next = prev.clone();
    match s.engine.scrape(url, &o).await {
        Ok(doc) => {
            s.limiter.consume_pages(caller, 1).await;
            let r = compare(url, prev, &doc, substantive_only, &mut next);
            (r, next)
        }
        Err(e) if matches!(e.code, ErrorCode::NotFound) => {
            let mut r = failure(url, &e);
            mark_gone(prev, &mut next, &mut r);
            (r, next)
        }
        Err(e) => (failure(url, &e), next),
    }
}

/// 页面不在了：第一次报 removed 并清掉旧内容，之后是 same；从来没拿到过内容的还是 error。
pub(super) fn mark_gone(prev: &PageState, next: &mut PageState, r: &mut PageResult) {
    if prev.removed {
        r.status = "same".into();
        r.error = None;
    } else if prev.markdown.is_some() {
        r.status = "removed".into();
        next.removed = true;
        next.markdown = None;
        next.extract = None;
        next.etag = None;
        next.last_modified = None;
    } else {
        r.status = "error".into();
    }
}

/// 和上次的内容比较，更新 `next` 里的内容和校验值。
pub(super) fn compare(
    url: &str,
    prev: &PageState,
    doc: &Document,
    substantive_only: bool,
    next: &mut PageState,
) -> PageResult {
    let meta = doc.metadata.pagesift.as_ref();
    let not_modified = meta.is_some_and(|m| m.not_modified);
    let status_code = doc.metadata.status_code;
    let mut r = PageResult {
        url: url.into(),
        status: "same".into(),
        error: None,
        noise_only: false,
        diff: None,
        field_changes: Vec::new(),
        not_modified,
    };
    // [协议] 404 Not Found、410 Gone（RFC 9110 §15.5.5、§15.5.11）
    if status_code == 404 || status_code == 410 {
        r.error = Some(format!("HTTP {status_code}"));
        mark_gone(prev, next, &mut r);
        return r;
    }
    // [协议] 4xx、5xx 是错误响应（RFC 9110 §15）：这是抓取失败，不是页面的新内容
    if status_code >= 400 {
        r.status = "error".into();
        r.error = Some(format!("HTTP {status_code}"));
        return r;
    }
    next.removed = false;
    if not_modified {
        return r;
    }
    next.etag = meta.and_then(|m| m.etag.clone());
    next.last_modified = meta.and_then(|m| m.last_modified.clone());
    let new_md = doc.markdown.clone().unwrap_or_default();
    let Some(old_md) = prev.markdown.as_deref() else {
        r.status = "new".into();
        next.markdown = Some(new_md);
        next.extract = doc.extract.clone();
        return r;
    };
    let report = pagesift_diff::diff_with(old_md, &new_md, &monitor_noise());
    let fields = field_changes(prev.extract.as_ref(), doc.extract.as_ref());
    let substantive = report.substantive || !fields.is_empty();
    if report.changed || !fields.is_empty() {
        if substantive || !substantive_only {
            r.status = "changed".into();
            let blocks: Vec<&pagesift_diff::BlockChange> = report
                .blocks
                .iter()
                .filter(|b| {
                    !substantive_only || b.significance == pagesift_diff::Significance::Substantive
                })
                .collect();
            r.diff = Some(json!({
                "text": pagesift_diff::unified_diff(old_md, &new_md, DIFF_CONTEXT).text(),
                "blocks": blocks,
            }));
            r.field_changes = fields;
        } else {
            r.noise_only = true;
        }
    }
    next.markdown = Some(new_md);
    next.extract = doc.extract.clone();
    r
}

/// [协议] 统一 diff 的上下文行数：GNU diff `-u` 的默认值 3。
const DIFF_CONTEXT: usize = 3;

/// `extract` 结果的字段级变化：`{item, field, before, after}`；列表项增减各记一条。
fn field_changes(old: Option<&Value>, new: Option<&Value>) -> Vec<Value> {
    let (Some(Value::Array(a)), Some(Value::Array(b))) = (old, new) else {
        return Vec::new();
    };
    let mut out = Vec::new();
    for i in 0..a.len().max(b.len()) {
        match (a.get(i), b.get(i)) {
            (Some(Value::Object(x)), Some(Value::Object(y))) => {
                let mut keys: Vec<&String> = x.keys().chain(y.keys()).collect();
                keys.sort();
                keys.dedup();
                for k in keys {
                    if x.get(k) != y.get(k) {
                        out.push(
                            json!({ "item": i, "field": k, "before": x.get(k), "after": y.get(k) }),
                        );
                    }
                }
            }
            (Some(x), None) => out.push(json!({ "item": i, "type": "removed", "before": x })),
            (None, Some(y)) => out.push(json!({ "item": i, "type": "added", "after": y })),
            (Some(x), Some(y)) if x != y => out.push(json!({ "item": i, "before": x, "after": y })),
            _ => {}
        }
    }
    out
}

/// webhook：每个有变化的页面一条 `monitor.page`（`same` 不发），每次检查一条 `monitor.check.completed`。
async fn notify(s: &AppState, m: &MonitorState, c: &Check) {
    let Some(cfg) = &m.webhook else { return };
    let wants = |e: &str| cfg.events.iter().any(|x| x == e);
    if wants("monitor.page") {
        for p in c.pages.iter().filter(|p| p.status != "same") {
            let mut item = serde_json::to_value(p).unwrap_or(Value::Null);
            item["monitorId"] = json!(m.id);
            item["checkId"] = json!(c.id);
            let body = json!({
                "success": p.status != "error", "type": "monitor.page", "id": c.id,
                "data": [item], "metadata": cfg.metadata,
            });
            let _ = s.webhooks.deliver(cfg, &body).await;
        }
    }
    if wants("monitor.check.completed") {
        let body = json!({
            "success": c.status == "completed", "type": "monitor.check.completed", "id": c.id,
            "data": [{ "monitorId": m.id, "checkId": c.id, "status": c.status, "summary": c.summary() }],
            "metadata": cfg.metadata,
        });
        let _ = s.webhooks.deliver(cfg, &body).await;
    }
}

/// 监控用的噪声类别：日期、相对时间、计数、广告位。价格和其他数字的变化按实质变化报
/// （监控价格是常见用途；`pagesift_diff` 的默认值把价格也当噪声，适合内容去重，不适合监控）。
fn monitor_noise() -> pagesift_diff::DiffOptions {
    use pagesift_diff::NoiseCategory::*;
    pagesift_diff::DiffOptions {
        noise_categories: vec![Date, RelativeTime, Count, Advertisement],
        ..Default::default()
    }
}
