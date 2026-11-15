//! Page change comparison and webhook/email notification helpers for monitor checks.

use serde_json::{Value, json};

use super::run::mark_gone;
use super::{Check, MonitorState, PageResult, PageState};
use crate::server::AppState;
use crate::types::Document;

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
    if prev.markdown.is_none() && prev.snapshot_ref.is_some() {
        r.status = "error".into();
        r.error = Some(
            "baseline_snapshot_unavailable: previous snapshot was evicted or missing; diff comparison unavailable"
                .into(),
        );
        if !not_modified {
            next.etag = meta.and_then(|m| m.etag.clone());
            next.last_modified = meta.and_then(|m| m.last_modified.clone());
            next.markdown = Some(doc.markdown.clone().unwrap_or_default());
            next.snapshot_ref = None;
            next.extract = doc.extract.clone();
        }
        return r;
    }
    if not_modified {
        return r;
    }
    next.etag = meta.and_then(|m| m.etag.clone());
    next.last_modified = meta.and_then(|m| m.last_modified.clone());
    let new_md = doc.markdown.clone().unwrap_or_default();
    let Some(old_md) = prev.markdown.as_deref() else {
        r.status = "new".into();
        next.markdown = Some(new_md);
        next.snapshot_ref = None;
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
    next.snapshot_ref = None;
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
pub(super) async fn notify(s: &AppState, m: &MonitorState, c: &Check) {
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
