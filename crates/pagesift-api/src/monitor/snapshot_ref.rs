//! 监控持久化与 Markdown 快照外置（`1.5` 存储收敛）：
//!
//! 落盘 `monitor/{id}` 时将每个 `PageState` 的 `markdown` 写入 `Store::snapshots()`
//! 并仅在状态 JSON 中保存内容寻址引用 `snap:<blake3>`；启动恢复时按需水合，
//! 并清除失效记录中的悬空引用。

use std::sync::atomic::Ordering;

use pagesift_store::{ContentHash, Store};

use super::{
    MonitorEntry, MonitorLife, MonitorState, PAUSED_REASON_ACCOUNT_REVOKED, WARNING_REVOKED_PREFIX,
};
use crate::error::{ApiError, ErrorCode};
use crate::server::AppState;

const SNAP_PREFIX: &str = "snap:";

fn key(id: &str) -> String {
    format!("monitor/{id}")
}

fn parse_snapshot_ref(raw: &str) -> Option<ContentHash> {
    let hex = raw.strip_prefix(SNAP_PREFIX)?;
    if hex.len() != 64 || !hex.is_ascii() {
        return None;
    }
    let mut bytes = [0u8; 32];
    for (i, slot) in bytes.iter_mut().enumerate() {
        let idx = i * 2;
        *slot = u8::from_str_radix(&hex[idx..idx + 2], 16).ok()?;
    }
    Some(ContentHash(bytes))
}

/// 将单份监控状态中的所有页正文外置到 `Store::snapshots()`，并返回供序列化的精简副本。
pub(super) fn offload_monitor_snapshots(store: &Store, m: &MonitorState) -> MonitorState {
    let mut slim = m.clone();
    for (url, page) in &mut slim.pages {
        let Some(md) = page.markdown.take() else {
            continue;
        };
        let hash = ContentHash::of(md.as_bytes());
        let expected_ref = format!("{SNAP_PREFIX}{hash}");
        if page.snapshot_ref.as_deref() == Some(expected_ref.as_str())
            && store.snapshots().get(&hash).ok().flatten().is_some()
        {
            continue;
        }
        match store.snapshots().put(url, md.as_bytes()) {
            Ok(put) => {
                page.snapshot_ref = Some(format!("{SNAP_PREFIX}{}", put.hash));
            }
            Err(err) => {
                tracing::warn!(
                    monitor = %m.id,
                    url = %url,
                    error = %err,
                    "failed to offload monitor snapshot; keeping inline markdown"
                );
                page.markdown = Some(md);
            }
        }
    }
    slim
}

/// 从 `Store::snapshots()` 按 `snapshot_ref` 水合页面正文，保持重启后 diff 语义不变；
/// 若快照已被淘汰或损坏，则追加明确 `baseline_unavailable` 告警（R08）。
pub(super) fn hydrate_monitor_snapshots(store: &Store, m: &mut MonitorState) {
    let mut push_warn = |msg: String| {
        if !m.warnings.iter().any(|existing| existing == &msg) {
            m.warnings.push(msg);
        }
    };
    for (url, page) in &mut m.pages {
        if page.markdown.is_some() {
            continue;
        }
        let Some(ref_str) = page.snapshot_ref.as_deref() else {
            continue;
        };
        let Some(hash) = parse_snapshot_ref(ref_str) else {
            tracing::warn!(monitor = %m.id, url = %url, snapshot_ref = %ref_str, "invalid snapshotRef");
            push_warn(format!(
                "baseline_unavailable: invalid snapshotRef for {url}"
            ));
            continue;
        };
        match store.snapshots().get(&hash) {
            Ok(Some(bytes)) => match String::from_utf8(bytes) {
                Ok(md) => page.markdown = Some(md),
                Err(err) => {
                    tracing::warn!(
                        monitor = %m.id,
                        url = %url,
                        error = %err,
                        "non-UTF-8 monitor snapshot payload"
                    );
                    push_warn(format!(
                        "baseline_unavailable: non-UTF-8 snapshot payload for {url}"
                    ));
                }
            },
            Ok(None) => {
                tracing::warn!(monitor = %m.id, url = %url, snapshot_ref = %ref_str, "snapshot evicted or missing");
                push_warn(format!(
                    "baseline_unavailable: snapshot evicted or missing for {url}"
                ));
            }
            Err(err) => {
                tracing::warn!(monitor = %m.id, url = %url, error = %err, "failed to load monitor snapshot");
                push_warn(format!(
                    "baseline_unavailable: failed to load snapshot for {url}"
                ));
            }
        }
    }
}

pub(super) fn persist_checked(store: &Store, m: &MonitorState) -> Result<(), ApiError> {
    let slim = offload_monitor_snapshots(store, m);
    let v = serde_json::to_vec(&slim).map_err(|e| {
        ApiError::new(
            ErrorCode::Internal,
            format!("could not encode monitor {}: {e}", m.id),
        )
    })?;
    store.tasks().kv_put(key(&m.id), &v).map_err(|e| {
        ApiError::new(
            ErrorCode::Internal,
            format!("could not persist monitor {}: {e}", m.id),
        )
    })
}

pub(super) fn persist_blocking(s: &AppState, m: &MonitorState) -> Result<(), ApiError> {
    #[cfg(test)]
    {
        let hook = s
            .monitors
            .hooks
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .as_ref()
            .and_then(|h| h.during_persist.clone());
        if let Some(hook) = hook {
            hook(&m.id);
        }
    }
    let Some(store) = s.jobs.store() else {
        return Ok(());
    };
    persist_checked(store, m)
}

pub(super) async fn forget_async(s: &AppState, id: &str) -> Result<(), ApiError> {
    #[cfg(test)]
    {
        let should_fail = s
            .monitors
            .hooks
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .as_ref()
            .and_then(|h| h.fail_delete.clone())
            .is_some_and(|f| f(id));
        if should_fail {
            return Err(ApiError::new(
                ErrorCode::Internal,
                format!("could not delete monitor {id}: injected storage delete failure"),
            ));
        }
    }
    let Some(store) = s.jobs.store().cloned() else {
        return Ok(());
    };
    let id = id.to_string();
    let k = key(&id);
    store
        .blocking(move |st| st.tasks().kv_delete(&k))
        .await
        .map_err(|e| {
            ApiError::new(
                ErrorCode::Internal,
                format!("could not delete monitor {id}: {e}"),
            )
        })?;
    Ok(())
}

/// 读回落盘的监控；有活跃监控时启动调度。`leader` 为假时（多服务器、本机不持有网关租约）不检查，只等。
pub(crate) fn start_scheduler(s: AppState, leader: Option<super::Leader>) {
    *s.monitors.leader.lock().unwrap_or_else(|e| e.into_inner()) = leader;
    let Some(store) = s.jobs.store() else {
        return;
    };
    let keys = match store.tasks().kv_keys("monitor/") {
        Ok(keys) => keys,
        Err(error) => {
            tracing::error!(%error, "could not list saved monitors; monitor scheduler not started");
            return;
        }
    };
    let any_active = {
        let mut map = s
            .monitors
            .map
            .try_lock()
            .expect("monitors are not shared yet");
        let mut active = false;
        for k in keys {
            let parsed = match store.tasks().kv_get(&k) {
                Ok(Some(v)) => {
                    serde_json::from_slice::<MonitorState>(&v).map_err(|e| e.to_string())
                }
                Ok(None) => continue,
                Err(e) => Err(e.to_string()),
            };
            match parsed {
                Ok(mut m) => {
                    hydrate_monitor_snapshots(store, &mut m);
                    let caller = s.monitors.resolve_caller(&m.owner);
                    match caller {
                        None => {
                            // 保留原本即因崩溃或其他非撤销原因暂停（含旧版无 pausedReason 的 paused）的状态，
                            // 防止账号先撤销再恢复时将其误自动切回 active。
                            let already_other_paused = m.status == "paused"
                                && m.paused_reason.as_deref()
                                    != Some(PAUSED_REASON_ACCOUNT_REVOKED);
                            if !already_other_paused {
                                let warning = format!(
                                    "{WARNING_REVOKED_PREFIX} owner account {:?} is no longer configured; monitor paused without running under default limits",
                                    m.owner
                                );
                                let changed = m.status != "paused"
                                    || m.paused_reason.as_deref()
                                        != Some(PAUSED_REASON_ACCOUNT_REVOKED)
                                    || !m.warnings.iter().any(|w| w == &warning);
                                m.status = "paused".into();
                                m.paused_reason = Some(PAUSED_REASON_ACCOUNT_REVOKED.into());
                                if !m.warnings.iter().any(|w| w == &warning) {
                                    m.warnings.push(warning);
                                }
                                if changed && let Err(e) = persist_checked(store, &m) {
                                    tracing::warn!(
                                        monitor = %m.id,
                                        owner = %m.owner,
                                        error = %e,
                                        "failed to persist revoked-account pause state for monitor"
                                    );
                                }
                                s.metrics
                                    .monitor_revoked_paused
                                    .fetch_add(1, Ordering::Relaxed);
                                tracing::warn!(
                                    monitor = %m.id,
                                    owner = %m.owner,
                                    "monitor owner account is no longer configured; pausing monitor instead of falling back to default_limits"
                                );
                            }
                        }
                        Some(_) => {
                            if m.status == "paused"
                                && m.paused_reason.as_deref() == Some(PAUSED_REASON_ACCOUNT_REVOKED)
                            {
                                m.status = "active".into();
                                m.paused_reason = None;
                                m.warnings
                                    .retain(|w| !w.starts_with(WARNING_REVOKED_PREFIX));
                                if let Err(e) = persist_checked(store, &m) {
                                    tracing::warn!(
                                        monitor = %m.id,
                                        owner = %m.owner,
                                        error = %e,
                                        "failed to persist auto-resumed monitor state"
                                    );
                                }
                                tracing::info!(
                                    monitor = %m.id,
                                    owner = %m.owner,
                                    "monitor owner account restored; resuming previously account_revoked monitor"
                                );
                            }
                        }
                    }
                    if m.status == "active" {
                        active = true;
                    }
                    map.insert(
                        m.id.clone(),
                        MonitorEntry {
                            state: m,
                            life: MonitorLife::new(),
                        },
                    );
                }
                // 记录留在存储里，不删：修好之后还能读回来
                Err(error) => {
                    tracing::error!(
                        record = %String::from_utf8_lossy(&k),
                        %error,
                        "could not read a saved monitor; it is not scheduled"
                    );
                    s.metrics
                        .monitor_load_failures
                        .fetch_add(1, Ordering::Relaxed);
                }
            }
        }
        active
    };
    if any_active {
        super::run::ensure_running(&s);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use axum::body::Body;
    use axum::http::{Request, StatusCode};
    use http_body_util::BodyExt;
    use std::sync::Arc;
    use tower::ServiceExt;

    use crate::engine::{BoxFuture, Engine};
    use crate::error::ApiError;
    use crate::server::{Config, router};
    use crate::types::{Document, Metadata, ScrapeOptions};

    struct StaticEngine(String);

    impl Engine for StaticEngine {
        fn scrape<'a>(
            &'a self,
            url: &'a str,
            _opts: &'a ScrapeOptions,
        ) -> BoxFuture<'a, Result<Document, ApiError>> {
            let md = self.0.clone();
            Box::pin(async move {
                Ok(Document {
                    markdown: Some(md),
                    metadata: Metadata {
                        source_url: Some(url.into()),
                        status_code: 200,
                        ..Default::default()
                    },
                    ..Default::default()
                })
            })
        }
    }

    fn monitor_rt() -> tokio::runtime::Runtime {
        tokio::runtime::Builder::new_multi_thread()
            .worker_threads(2)
            .enable_all()
            .build()
            .expect("tokio runtime")
    }

    #[test]
    fn persist_offloads_markdown_to_snapshot_store_and_rehydrates_on_restart() {
        let dir = tempfile::tempdir().expect("tempdir");
        let unique_body = "# Product\n\nUNIQUE_MONITOR_BODY_MARKER_987654\n\nPrice: $49";

        let rt1 = monitor_rt();
        let id = rt1.block_on(async {
            let store = Store::open(pagesift_store::Config::new(dir.path())).expect("store");
            let app1 = router(
                Arc::new(StaticEngine(unique_body.into())),
                Config {
                    store: Some(store),
                    ..Default::default()
                },
            );
            let create_req = Request::builder()
                .method("POST")
                .uri("/v2/monitor")
                .header("content-type", "application/json")
                .body(Body::from(
                    serde_json::json!({
                        "targets": [{ "type": "scrape", "urls": ["https://example.com/p"] }]
                    })
                    .to_string(),
                ))
                .unwrap();
            let resp = app1.clone().oneshot(create_req).await.unwrap();
            assert_eq!(resp.status(), StatusCode::OK);
            let body: serde_json::Value =
                serde_json::from_slice(&resp.into_body().collect().await.unwrap().to_bytes())
                    .unwrap();
            let id = body["data"]["id"].as_str().unwrap().to_string();

            let run_req = Request::builder()
                .method("POST")
                .uri(format!("/v2/monitor/{id}/run"))
                .body(Body::empty())
                .unwrap();
            let resp = app1.oneshot(run_req).await.unwrap();
            assert_eq!(resp.status(), StatusCode::OK);
            id
        });
        drop(rt1);

        let rt2 = monitor_rt();
        rt2.block_on(async {
            let store = Store::open(pagesift_store::Config::new(dir.path())).expect("store");
            let raw_bytes = store
                .tasks()
                .kv_get(key(&id))
                .expect("kv_get")
                .expect("persisted monitor");
            let raw_str = std::str::from_utf8(&raw_bytes).expect("utf8 json");
            assert!(
                !raw_str.contains("UNIQUE_MONITOR_BODY_MARKER_987654"),
                "monitor metadata must not inline page markdown: {raw_str}"
            );
            let persisted: MonitorState = serde_json::from_slice(&raw_bytes).expect("json");
            let page = persisted.pages.get("https://example.com/p").expect("page");
            assert!(page.markdown.is_none());
            assert!(
                page.snapshot_ref
                    .as_deref()
                    .is_some_and(|r| r.starts_with("snap:"))
            );

            // Simulated restart with identical body: check must recognize `same` using hydrated baseline
            let app2 = router(
                Arc::new(StaticEngine(unique_body.into())),
                Config {
                    store: Some(store),
                    ..Default::default()
                },
            );
            let run2 = Request::builder()
                .method("POST")
                .uri(format!("/v2/monitor/{id}/run"))
                .body(Body::empty())
                .unwrap();
            let resp2 = app2.oneshot(run2).await.unwrap();
            assert_eq!(resp2.status(), StatusCode::OK);
            let body2: serde_json::Value =
                serde_json::from_slice(&resp2.into_body().collect().await.unwrap().to_bytes())
                    .unwrap();
            assert_eq!(body2["data"]["pages"][0]["status"], "same");
        });
    }

    #[test]
    fn parse_snapshot_ref_rejects_multibyte_utf8_64_byte_payload_without_panic() {
        let payload = format!("{SNAP_PREFIX}{}", "é".repeat(32));
        assert_eq!(payload.len(), SNAP_PREFIX.len() + 64);
        assert_eq!(parse_snapshot_ref(&payload), None);
    }

    #[test]
    fn evicted_snapshot_never_falsifies_new_or_same_baseline_r08() {
        let dir = tempfile::tempdir().expect("tempdir");
        let body = "# Product\n\nPrice: $49";

        let rt1 = monitor_rt();
        let id = rt1.block_on(async {
            let store = Store::open(pagesift_store::Config::new(dir.path())).expect("store");
            let app1 = router(
                Arc::new(StaticEngine(body.into())),
                Config {
                    store: Some(store),
                    ..Default::default()
                },
            );
            let create_req = Request::builder()
                .method("POST")
                .uri("/v2/monitor")
                .header("content-type", "application/json")
                .body(Body::from(
                    serde_json::json!({
                        "targets": [{ "type": "scrape", "urls": ["https://example.com/evict"] }]
                    })
                    .to_string(),
                ))
                .unwrap();
            let resp = app1.clone().oneshot(create_req).await.unwrap();
            let created: serde_json::Value =
                serde_json::from_slice(&resp.into_body().collect().await.unwrap().to_bytes())
                    .unwrap();
            let id = created["data"]["id"].as_str().unwrap().to_string();

            let run1 = Request::builder()
                .method("POST")
                .uri(format!("/v2/monitor/{id}/run"))
                .body(Body::empty())
                .unwrap();
            assert_eq!(app1.oneshot(run1).await.unwrap().status(), StatusCode::OK);
            id
        });
        drop(rt1);

        let rt2 = monitor_rt();
        rt2.block_on(async {
            let store = Store::open(pagesift_store::Config::new(dir.path())).expect("store");
            // Corrupt the stored snapshot ref after app1 shutdown to simulate snapshot eviction / missing payload.
            let mut raw_mon: MonitorState =
                serde_json::from_slice(&store.tasks().kv_get(key(&id)).unwrap().expect("monitor"))
                    .unwrap();
            raw_mon
                .pages
                .get_mut("https://example.com/evict")
                .unwrap()
                .snapshot_ref = Some(format!("{SNAP_PREFIX}{}", "f".repeat(64)));
            store
                .tasks()
                .kv_put(key(&id), &serde_json::to_vec(&raw_mon).unwrap())
                .unwrap();

            let app2 = router(
                Arc::new(StaticEngine(body.into())),
                Config {
                    store: Some(store),
                    ..Default::default()
                },
            );
            let get_req = Request::builder()
                .method("GET")
                .uri(format!("/v2/monitor/{id}"))
                .body(Body::empty())
                .unwrap();
            let get_resp = app2.clone().oneshot(get_req).await.unwrap();
            let get_body: serde_json::Value =
                serde_json::from_slice(&get_resp.into_body().collect().await.unwrap().to_bytes())
                    .unwrap();
            assert!(
                get_body["data"]["warnings"].as_array().is_some_and(|w| w
                    .iter()
                    .any(|x| x.as_str().unwrap_or("").contains("baseline_unavailable"))),
                "expected baseline_unavailable warning: {get_body}"
            );

            let run2 = Request::builder()
                .method("POST")
                .uri(format!("/v2/monitor/{id}/run"))
                .body(Body::empty())
                .unwrap();
            let resp2 = app2.clone().oneshot(run2).await.unwrap();
            let body2: serde_json::Value =
                serde_json::from_slice(&resp2.into_body().collect().await.unwrap().to_bytes())
                    .unwrap();
            assert_eq!(body2["data"]["pages"][0]["status"], "error", "{body2}");
            assert!(
                body2["data"]["pages"][0]["error"]
                    .as_str()
                    .is_some_and(|e| e.contains("baseline_snapshot_unavailable")),
                "{body2}"
            );

            // Next check heals baseline cleanly back to `same`.
            let run3 = Request::builder()
                .method("POST")
                .uri(format!("/v2/monitor/{id}/run"))
                .body(Body::empty())
                .unwrap();
            let resp3 = app2.oneshot(run3).await.unwrap();
            let body3: serde_json::Value =
                serde_json::from_slice(&resp3.into_body().collect().await.unwrap().to_bytes())
                    .unwrap();
            assert_eq!(body3["data"]["pages"][0]["status"], "same", "{body3}");
        });
    }
}
