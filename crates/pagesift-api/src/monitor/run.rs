//! 监控的调度和一次检查：到期的网址抓一遍（带条件请求），和上次的内容做块级 diff，
//! 更新变化率后验、算下次到期时间，记检查结果，发 webhook，落盘。

use std::collections::HashSet;
use std::sync::Arc;
use std::sync::atomic::Ordering;
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use pagesift_stat::poisson::ChangeRate;
use tokio::task::JoinSet;

use super::crawl::check_crawl;
use super::snapshot_ref::persist_blocking;
use super::{
    Check, MonitorLife, MonitorState, PAUSED_REASON_ACCOUNT_REVOKED, PAUSED_REASON_CHECK_PANICKED,
    PageResult, PageState, WARNING_REVOKED_PREFIX,
};
use crate::error::ErrorCode;
use crate::limits::Caller;
use crate::server::AppState;
use crate::types::ScrapeOptions;

/// [δ] 全局风险水平：下次检查放在“漏掉变化的后验概率”到 δ 的时刻。
pub(super) use super::diff::{compare, notify};

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

/// 没有在跑的调度任务时启动一个。监控全部删掉后它自己退出（释放它持有的服务状态和存储）。
pub(super) fn ensure_running(s: &AppState) {
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
                let has_active = map
                    .values()
                    .any(|e| !e.life.is_deleted() && e.state.status == "active");
                if !has_active && checks.is_empty() {
                    s.monitors.running.store(false, Ordering::SeqCst);
                    return;
                }
                map.values()
                    .filter(|e| {
                        !e.life.is_deleted()
                            && e.state.status == "active"
                            && !busy.contains(&e.state.id)
                    })
                    .filter_map(|e| e.state.next_due())
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
                // 不是调度者：过一个最短间隔再看租约，同时响应唤醒和已发检查收尾（删空监控时及时退出）
                tokio::select! {
                    () = tokio::time::sleep(Duration::from_secs(super::schedule::MIN_INTERVAL_SECS)) => {}
                    () = s.monitors.wake.notified() => {}
                    Some(done) = checks.join_next(), if !checks.is_empty() => {
                        settle(&s, &mut busy, done).await;
                    }
                }
                continue;
            }
            let ids: Vec<String> = {
                let map = s.monitors.map.lock().await;
                let now = now_secs();
                map.values()
                    .filter(|e| {
                        !e.life.is_deleted()
                            && e.state.status == "active"
                            && !busy.contains(&e.state.id)
                    })
                    .filter(|e| e.state.next_due().is_some_and(|d| d <= now))
                    .map(|e| e.state.id.clone())
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
    let life = {
        let map = s.monitors.map.lock().await;
        match map.get(&id) {
            Some(e) if !e.life.is_deleted() => e.life.clone(),
            _ => return,
        }
    };
    let persist_guard = life.persist_lock.clone().lock_owned().await;
    if life.is_deleted() {
        return;
    }
    let saved = {
        let mut map = s.monitors.map.lock().await;
        let Some(entry) = map.get_mut(&id) else {
            return;
        };
        if !Arc::ptr_eq(&entry.life, &life) || life.is_deleted() {
            return;
        }
        entry.state.status = "paused".into();
        entry.state.paused_reason = Some(PAUSED_REASON_CHECK_PANICKED.into());
        let warning = "monitor_paused: a check crashed, so this monitor stopped running";
        if !entry.state.warnings.iter().any(|w| w == warning) {
            entry.state.warnings.push(warning.into());
        }
        entry.state.clone()
    };
    let writer = s.clone();
    let res = tokio::task::spawn_blocking(move || {
        let _guard = persist_guard;
        persist_blocking(&writer, &saved)
    })
    .await;
    match res {
        Ok(Ok(())) => {}
        Ok(Err(error)) => {
            tracing::error!(monitor = %id, %error, "could not persist panicked monitor pause")
        }
        Err(error) => tracing::error!(monitor = %id, %error, "monitor panic persist task failed"),
    }
}

async fn pause_revoked_monitor(s: &AppState, id: &str, life: &Arc<MonitorLife>) {
    let persist_guard = life.persist_lock.clone().lock_owned().await;
    if life.is_deleted() {
        return;
    }
    let saved = {
        let mut map = s.monitors.map.lock().await;
        let Some(entry) = map.get_mut(id) else {
            return;
        };
        if !Arc::ptr_eq(&entry.life, life) || life.is_deleted() {
            return;
        }
        let warning = format!(
            "{WARNING_REVOKED_PREFIX} owner account {:?} is no longer configured; monitor paused without running under default limits",
            entry.state.owner
        );
        entry.state.status = "paused".into();
        entry.state.paused_reason = Some(PAUSED_REASON_ACCOUNT_REVOKED.into());
        if !entry.state.warnings.iter().any(|w| w == &warning) {
            entry.state.warnings.push(warning);
        }
        entry.state.clone()
    };
    s.metrics
        .monitor_revoked_paused
        .fetch_add(1, Ordering::Relaxed);
    tracing::warn!(
        monitor = %id,
        owner = %saved.owner,
        "monitor owner account is no longer configured during check; pausing monitor instead of falling back to default_limits"
    );
    let writer = s.clone();
    let res = tokio::task::spawn_blocking(move || {
        let _guard = persist_guard;
        persist_blocking(&writer, &saved)
    })
    .await;
    if let Ok(Err(error)) = res {
        tracing::error!(monitor = %id, %error, "could not persist revoked-account pause state");
    }
}

/// 一次检查：只查到期的网址（自适应时各网址的到期时间不同）。
/// 通过条目自带的唯一 `life.run_lock` 与手动 `/run` 串行执行，避免并发覆盖 `next_due`。
pub(crate) async fn run_check(s: &AppState, id: &str) {
    let life = {
        let map = s.monitors.map.lock().await;
        let Some(entry) = map.get(id) else {
            return;
        };
        if entry.life.is_deleted() || entry.state.status != "active" {
            return;
        }
        entry.life.clone()
    };
    let _guard = life.run_lock.lock().await;
    let _ = run_check_locked(s, id, &life).await;
}

/// 在调用方已持有该监控 `life.run_lock` 的前提下执行一次检查。
pub(crate) async fn run_check_locked(
    s: &AppState,
    id: &str,
    life: &Arc<MonitorLife>,
) -> Option<Check> {
    if life.is_deleted() {
        return None;
    }
    let snapshot = {
        let map = s.monitors.map.lock().await;
        let entry = map.get(id)?;
        if !Arc::ptr_eq(&entry.life, life) || life.is_deleted() || entry.state.status == "deleting"
        {
            return None;
        }
        entry.state.clone()
    };
    let Some(caller) = s.monitors.resolve_caller(&snapshot.owner) else {
        pause_revoked_monitor(s, id, life).await;
        return None;
    };
    let started = now_secs();
    let due: Vec<(String, PageState)> = snapshot
        .pages
        .iter()
        .filter(|(_, p)| p.next_due <= started)
        .map(|(u, p)| (u.clone(), p.clone()))
        .collect();
    if due.is_empty() {
        return None;
    }
    let mut results = Vec::new();
    let mut updates: Vec<(String, PageState)> = Vec::new();
    let substantive_only = snapshot.req.notify.as_deref() != Some("any");
    let mut cancel_rx = life.subscribe_cancel();
    for (url, prev) in due {
        if life.is_deleted() {
            return None;
        }
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
            let (pages, observed) = tokio::select! {
                biased;
                () = MonitorLife::wait_cancelled(&mut cancel_rx) => return None,
                res = check_crawl(
                    s,
                    &snapshot,
                    &seed,
                    target,
                    &opts,
                    substantive_only,
                    &caller,
                    life,
                ) => res,
            };
            if life.is_deleted() {
                return None;
            }
            let mut next = prev.clone();
            observe(&snapshot, &mut next, started, observed);
            updates.push((url, next));
            for (u, st, r) in pages {
                updates.push((u, st));
                results.push(r);
            }
            continue;
        }
        let (result, mut next) = tokio::select! {
            biased;
            () = MonitorLife::wait_cancelled(&mut cancel_rx) => return None,
            res = check_page(s, &url, &prev, &opts, substantive_only, &caller) => res,
        };
        if life.is_deleted() {
            return None;
        }
        observe(&snapshot, &mut next, started, observation(&result));
        updates.push((url, next));
        results.push(result);
    }
    if life.is_deleted() {
        return None;
    }
    #[cfg(test)]
    {
        let before_persist = s
            .monitors
            .hooks
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .as_ref()
            .and_then(|h| h.before_persist.clone());
        if let Some(hook) = before_persist {
            hook(id).await;
        }
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
    let persist_guard = life.persist_lock.clone().lock_owned().await;
    if life.is_deleted() {
        return None;
    }
    let saved = {
        let mut map = s.monitors.map.lock().await;
        let entry = map.get_mut(id)?;
        if !Arc::ptr_eq(&entry.life, life) || life.is_deleted() {
            return None;
        }
        let m = &mut entry.state;
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
    // 序列化和写盘可能很久（状态里有页面全文），不占异步线程；
    // 将 OwnedMutexGuard 移入 blocking 任务确保即使外层 future 被取消，物理写盘完成前 DELETE 也会等待。
    let writer = s.clone();
    let persist_state = saved.clone();
    let persist_res = tokio::task::spawn_blocking(move || {
        let _guard = persist_guard;
        persist_blocking(&writer, &persist_state)
    })
    .await;
    match persist_res {
        Ok(Ok(())) => {}
        Ok(Err(error)) => {
            tracing::error!(monitor = %id, %error, "could not persist monitor check result");
        }
        Err(error) => {
            tracing::error!(monitor = %id, %error, "monitor persist task failed");
        }
    }
    if !life.is_deleted() {
        notify(s, &saved, &check, life).await;
    }
    Some(check)
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
    crate::limits::apply_http_caller_scope(&mut o, Some(caller), None);
    o.max_age = Some(0);
    if prev.markdown.is_some() {
        o.if_none_match = prev.etag.clone();
        o.if_modified_since = prev.last_modified.clone();
    }
    let mut next = prev.clone();
    let hold = match s.limiter.reserve_page(caller).await {
        Ok(hold) => Some(hold),
        Err(e) => return (failure(url, &e), next),
    };
    match s.engine.scrape(url, &o).await {
        Ok(doc) => {
            if let Some(hold) = hold {
                let outcome = hold.commit().await;
                if outcome.is_unrecoverable() {
                    let err = crate::ApiError::new(
                        ErrorCode::Internal,
                        outcome
                            .warning()
                            .unwrap_or_else(|| "quota settlement failed".into()),
                    );
                    return (failure(url, &err), next);
                }
            }
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
    } else if prev.markdown.is_some() || prev.snapshot_ref.is_some() {
        r.status = "removed".into();
        next.removed = true;
        next.markdown = None;
        next.snapshot_ref = None;
        next.extract = None;
        next.etag = None;
        next.last_modified = None;
    } else {
        r.status = "error".into();
    }
}
