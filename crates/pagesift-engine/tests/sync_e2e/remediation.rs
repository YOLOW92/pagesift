use std::collections::BTreeMap;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use axum::Router;
use axum::http::{HeaderMap, HeaderValue, StatusCode, header};
use axum::response::IntoResponse;
use axum::routing::get;
use pagesift_engine::sync::SyncRunOptions;
use pagesift_graph::{ClaimStatus, ConflictKind, parse_timestamp_ms};
use pagesift_plan::experience::{FamilyExperienceTable, FamilyObservation};
use pagesift_plan::render::family;
use pagesift_store::{FreshnessPolicy, ResourceLifecycleState, SyncBudget};
use serde_json::json;

use super::common::{
    arc_state, product_html, run_opts, run_opts_forced, sample_spec, spawn_server, test_scraper,
    tick_and_get,
};

#[tokio::test]
async fn t26_observe_anchors_uncertain_coverage_and_independent_reliability() {
    let dir = tempfile::tempdir().unwrap();
    let store = pagesift_store::Store::open_dir(dir.path()).unwrap();
    let scraper = test_scraper(Some(store.clone()));

    let p1_phase = arc_state(AtomicUsize::new(0));
    let p1 = p1_phase.clone();
    let app = Router::new()
        .route(
            "/index",
            get(|| async move {
                (
                    [(header::CONTENT_TYPE, "text/html; charset=utf-8")],
                    product_html(
                        "Catalog Root",
                        "SKU-ROOT",
                        "10",
                        &[("/fragile", "Fragile"), ("/healthy", "Healthy")],
                    ),
                )
            }),
        )
        .route(
            "/healthy",
            get(|| async move {
                (
                    [(header::CONTENT_TYPE, "text/html; charset=utf-8")],
                    product_html("Healthy Item", "SKU-OK", "50", &[]),
                )
            }),
        )
        .route(
            "/fragile",
            get(move || {
                let ph = p1.load(Ordering::SeqCst);
                async move {
                    if ph == 1 {
                        let mut bad = b"<html><body>Corrupted ".to_vec();
                        bad.extend_from_slice(&[0xFF, 0xFE, 0xFD, 0x80, 0x81, 0xFF, 0xFE]);
                        bad.extend_from_slice(b"</body></html>");
                        (
                            StatusCode::OK,
                            [(header::CONTENT_TYPE, "text/html; charset=utf-8")],
                            bad,
                        )
                            .into_response()
                    } else {
                        let price = if ph >= 2 { "55" } else { "45" };
                        let html = product_html("Fragile Item", "SKU-FRAG", price, &[]);
                        (
                            StatusCode::OK,
                            [(header::CONTENT_TYPE, "text/html; charset=utf-8")],
                            html.into_bytes(),
                        )
                            .into_response()
                    }
                }
            }),
        );
    let (base, shutdown) = spawn_server(app).await;

    let mut spec = sample_spec("s-t26", format!("{base}/index"));
    spec.freshness = FreshnessPolicy {
        min_interval_secs: 1,
        default_interval_secs: 600,
        max_interval_secs: 3600,
        confirm_gone_after_404s: 2,
    };
    spec.required_fields = Vec::new();
    spec.extract_schema = None;
    store.sync().upsert_source(spec).unwrap();

    // Tick 1: 全部健康建立有效观测锚点
    let (d1, inv1) = tick_and_get(&scraper, &store, "s-t26", run_opts()).await;
    assert_eq!(d1.coverage_status, "complete_within_scope");
    let rec1 = inv1
        .iter()
        .find(|r| r.url.ends_with("/fragile"))
        .expect("fragile record");
    let valid_t1 = rec1
        .last_valid_observed_at_ms
        .expect("t1 valid observation anchor set");

    tokio::time::sleep(Duration::from_millis(60)).await;

    // Tick 2: /fragile 发生不确定解码；即使 /index 和 /healthy 成功，
    // 未解决的 uncertain_observations 绝不能报告 complete_within_scope，且有效时间锚点不可被 t2 覆盖！
    p1_phase.store(1, Ordering::SeqCst);
    let (d2, mut inv2) = tick_and_get(&scraper, &store, "s-t26", run_opts_forced()).await;
    assert_eq!(d2.stats.uncertain_observations, 1);
    assert_ne!(
        d2.coverage_status, "complete_within_scope",
        "tick with unresolved uncertain_observations must not claim complete_within_scope"
    );
    let rec2 = inv2
        .iter()
        .find(|r| r.url.ends_with("/fragile"))
        .expect("fragile record t2");
    assert_eq!(
        rec2.last_valid_observed_at_ms,
        Some(valid_t1),
        "unreliable attempt must not overwrite last_valid_observed_at_ms"
    );
    assert!(
        rec2.last_attempted_at_ms.unwrap_or(0) > valid_t1,
        "last_attempted_at_ms should track attempt time independently"
    );

    // Tick 3: 若不强制重抓且 /fragile 仍在退避期（未重新核验），只要存在未恢复的不可靠缺口，下一轮依然不可误报 complete_within_scope
    let now_after = pagesift_store::now_store_ms();
    for r in &mut inv2 {
        if r.url.ends_with("/fragile") {
            r.next_due_at_ms = now_after + 600_000;
        } else {
            r.next_due_at_ms = now_after.saturating_sub(1_000);
        }
    }
    store
        .sync()
        .put_inventory("s-t26", &inv2, now_after)
        .unwrap();
    let (d3, _) = tick_and_get(&scraper, &store, "s-t26", run_opts()).await;
    assert_ne!(
        d3.coverage_status, "complete_within_scope",
        "persisted unresolved unreliable gap must block complete_within_scope on subsequent ticks"
    );

    // Tick 4: /fragile 恢复正常并发生内容实质变化；学习窗口应跨越 [t1..t4] 而不是被 t2 失败缩短
    tokio::time::sleep(Duration::from_millis(60)).await;
    p1_phase.store(2, Ordering::SeqCst);
    let (d4, inv4) = tick_and_get(&scraper, &store, "s-t26", run_opts_forced()).await;
    assert_eq!(d4.coverage_status, "complete_within_scope");
    let rec4 = inv4
        .iter()
        .find(|r| r.url.ends_with("/fragile"))
        .expect("fragile record t4");
    assert!(rec4.last_valid_observed_at_ms.unwrap() > valid_t1);
    assert!(!rec4.learn.last_observation_unreliable);

    let _ = shutdown.send(());
}

fn format_http_gmt_now() -> String {
    let secs = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_secs();
    let days = (secs / 86_400) as i64;
    let rem = secs % 86_400;
    let hour = rem / 3600;
    let min = (rem % 3600) / 60;
    let sec = rem % 60;
    // Howard Hinnant civil_from_days
    let z = days + 719_468;
    let era = if z >= 0 { z } else { z - 146_096 } / 146_097;
    let doe = (z - era * 146_097) as u64;
    let yoe = (doe - doe / 1460 + doe / 36_524 - doe / 146_096) / 365;
    let y = (yoe as i64) + era * 400;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let d = doy - (153 * mp + 2) / 5 + 1;
    let m = if mp < 10 { mp + 3 } else { mp - 9 };
    let year = y + if m <= 2 { 1 } else { 0 };
    let months = [
        "Jan", "Feb", "Mar", "Apr", "May", "Jun", "Jul", "Aug", "Sep", "Oct", "Nov", "Dec",
    ];
    let wdays = ["Sun", "Mon", "Tue", "Wed", "Thu", "Fri", "Sat"];
    let wday = wdays[((days + 4).rem_euclid(7)) as usize];
    format!(
        "{wday}, {d:02} {} {year:04} {hour:02}:{min:02}:{sec:02} GMT",
        months[(m - 1) as usize]
    )
}

#[tokio::test]
async fn t27_schedule_last_modified_validation_waterfill_intervals_and_multiwave_due() {
    let dir = tempfile::tempdir().unwrap();
    let store = pagesift_store::Store::open_dir(dir.path()).unwrap();
    let scraper = test_scraper(Some(store.clone()));

    let credible_lm = "Sun, 15 Nov 2026 12:00:00 GMT";
    let expected_lm_ms = parse_timestamp_ms(credible_lm).expect("valid HTTP-date");

    let app = Router::new()
        .route(
            "/root",
            get(|| async move {
                (
                    [(header::CONTENT_TYPE, "text/html; charset=utf-8")],
                    product_html(
                        "Root Index",
                        "SKU-IDX",
                        "10",
                        &[
                            ("/credible", "Credible"),
                            ("/dynamic", "Dynamic"),
                            ("/cold", "Cold"),
                        ],
                    ),
                )
            }),
        )
        .route(
            "/credible",
            get(move || async move {
                let mut h = HeaderMap::new();
                h.insert(
                    header::CONTENT_TYPE,
                    HeaderValue::from_static("text/html; charset=utf-8"),
                );
                h.insert(
                    header::LAST_MODIFIED,
                    HeaderValue::from_static("Sun, 15 Nov 2026 12:00:00 GMT"),
                );
                (h, product_html("Credible Item", "SKU-CRED", "40", &[]))
            }),
        )
        .route(
            "/dynamic",
            get(|| async move {
                let mut h = HeaderMap::new();
                h.insert(
                    header::CONTENT_TYPE,
                    HeaderValue::from_static("text/html; charset=utf-8"),
                );
                h.insert(
                    header::LAST_MODIFIED,
                    HeaderValue::from_str(&format_http_gmt_now()).unwrap(),
                );
                (h, product_html("Dynamic Header Item", "SKU-DYN", "40", &[]))
            }),
        )
        .route(
            "/cold",
            get(|| async move {
                (
                    [(header::CONTENT_TYPE, "text/html; charset=utf-8")],
                    product_html("Cold Catalog Item", "SKU-COLD", "90", &[]),
                )
            }),
        );
    let (base, shutdown) = spawn_server(app).await;

    // 1. 验证 HTTP Last-Modified 打通真实观测链与动态伪造头拒止
    let mut spec = sample_spec("s-t27-lm", format!("{base}/root"));
    spec.freshness = FreshnessPolicy {
        min_interval_secs: 1,
        default_interval_secs: 60,
        max_interval_secs: 3600,
        confirm_gone_after_404s: 2,
    };
    spec.required_fields = Vec::new();
    spec.extract_schema = None;
    store.sync().upsert_source(spec).unwrap();

    let (d1, _) = tick_and_get(&scraper, &store, "s-t27-lm", run_opts()).await;
    assert_eq!(d1.stats.candidates_due, 4);
    assert_eq!(d1.stats.requests_executed, 4);

    // Tick 2: 内容均未发生实质改变；/dynamic 仍不断把 Last-Modified 设为当前时刻，应被识别为动态虚假头拒止
    let (_, inv2) = tick_and_get(&scraper, &store, "s-t27-lm", run_opts_forced()).await;
    let cred_rec = inv2.iter().find(|r| r.url.ends_with("/credible")).unwrap();
    let dyn_rec = inv2.iter().find(|r| r.url.ends_with("/dynamic")).unwrap();
    assert_eq!(cred_rec.last_modified_at_ms, Some(expected_lm_ms));
    assert!(!cred_rec.last_modified_dynamic);
    assert!(
        dyn_rec.last_modified_dynamic,
        "server advancing Last-Modified on unchanged body must be flagged dynamic"
    );
    assert!(dyn_rec.last_modified_at_ms.is_none());

    // 2. 验证多波次中未消费完的到期项不会重复计入 candidates_due：
    // 先将 4 个资源全部设为到期，max_requests=2（首波命中 root 与 1 个子页时不能重复累计未访到期项）
    let mut inv_due = store.sync().get_inventory("s-t27-lm").unwrap();
    let past = 1_000u64;
    for r in &mut inv_due {
        r.next_due_at_ms = past;
    }
    store
        .sync()
        .put_inventory("s-t27-lm", &inv_due, pagesift_store::now_store_ms())
        .unwrap();
    let mut spec_edit = store.sync().get_source("s-t27-lm").unwrap().unwrap();
    spec_edit.budget.max_requests_per_tick = 2;
    spec_edit.revision += 1;
    store.sync().upsert_source(spec_edit).unwrap();
    let (d_cap, _) = tick_and_get(&scraper, &store, "s-t27-lm", run_opts()).await;
    assert_eq!(d_cap.stats.requests_executed, 2);
    assert_eq!(
        d_cap.stats.candidates_due, 4,
        "candidates_due across waves must count unique resources (4), never double counting unvisited items"
    );

    // 3. 验证 Waterfill 在真实紧预算 Sync Tick 下拉开冷/高成本与热/高权重资源的真实 next_due_at_ms
    let mut spec_wf = sample_spec("s-t27-wf", format!("{base}/credible"));
    spec_wf.seeds = vec![format!("{base}/credible"), format!("{base}/cold")];
    spec_wf.freshness = FreshnessPolicy {
        min_interval_secs: 30,
        default_interval_secs: 120,
        max_interval_secs: 7200,
        confirm_gone_after_404s: 2,
    };
    spec_wf.budget = SyncBudget {
        max_requests_per_tick: 1,
        max_browser_renders_per_tick: 1,
        max_wall_clock_ms: 30_000,
        target_items: None,
    };
    spec_wf.required_fields = Vec::new();
    spec_wf.extract_schema = None;
    store.sync().upsert_source(spec_wf).unwrap();
    let _ = tick_and_get(&scraper, &store, "s-t27-wf", run_opts_forced()).await;
    // 把两个资源拉入高热（weight=3.0）与低冷（weight=0.3）并重置同时到期再跑一轮强制对比 Waterfill 真实输出间隔
    let now_wf = pagesift_store::now_store_ms();
    let mut wf_inv = store.sync().get_inventory("s-t27-wf").unwrap();
    for r in &mut wf_inv {
        r.next_due_at_ms = now_wf.saturating_sub(1_000);
        if r.url.ends_with("/credible") {
            r.importance_weight = 3.0;
            r.learn.change_count = 8;
            r.learn.exposure_secs = 480.0;
        } else {
            r.importance_weight = 0.3;
            r.learn.unchanged_count = 10;
            r.learn.exposure_secs = 50_000.0;
        }
    }
    store
        .sync()
        .put_inventory("s-t27-wf", &wf_inv, now_wf)
        .unwrap();
    let mut spec_wf2 = store.sync().get_source("s-t27-wf").unwrap().unwrap();
    spec_wf2.budget.max_requests_per_tick = 2; // 本轮检查2个同时到期页（预算仍远低于 cadence 下 30s 紧刷新期望）
    spec_wf2.freshness.default_interval_secs = 30;
    spec_wf2.revision += 1;
    store.sync().upsert_source(spec_wf2).unwrap();
    let (_, wf_after) = tick_and_get(&scraper, &store, "s-t27-wf", run_opts()).await;
    let hot_due = wf_after
        .iter()
        .find(|r| r.url.ends_with("/credible"))
        .unwrap()
        .next_due_at_ms;
    let cold_due = wf_after
        .iter()
        .find(|r| r.url.ends_with("/cold"))
        .unwrap()
        .next_due_at_ms;
    assert!(
        hot_due < cold_due,
        "Waterfill on real sync tick must schedule cold low-rate page further than hot page: hot={hot_due}, cold={cold_due}"
    );

    let _ = shutdown.send(());
}

#[tokio::test]
async fn t28_adapt_shadow_price_real_execution_and_direct_browser_recovery() {
    let dir = tempfile::tempdir().unwrap();
    let store = pagesift_store::Store::open_dir(dir.path()).unwrap();
    let scraper = test_scraper(Some(store.clone()));

    let app = Router::new()
        .route(
            "/spa/shell/{id}",
            get(|| async move {
                // JS 空壳（携带 ETag + __NEXT_DATA__ 无关空值），按 HTTP 提取不充分且需升阶决策
                let mut h = HeaderMap::new();
                h.insert(header::ETAG, HeaderValue::from_static("\"shell-etag\""));
                h.insert(
                    header::CONTENT_TYPE,
                    HeaderValue::from_static("text/html; charset=utf-8"),
                );
                (
                    h,
                    r#"<!doctype html><html><body><div id="app">Loading...</div><script>window.__APP__=1;</script></body></html>"#,
                )
            }),
        )
        .route(
            "/spa/ssr/{id}",
            get(|| async move {
                // 已从 SPA 回迁为完整 SSR：带完整正文与 JSON-LD，且即便保留旧 ETag 也不应被 ConditionalHttp 锁死
                let mut h = HeaderMap::new();
                h.insert(header::ETAG, HeaderValue::from_static("\"shell-etag\""));
                h.insert(
                    header::CONTENT_TYPE,
                    HeaderValue::from_static("text/html; charset=utf-8"),
                );
                (
                    h,
                    product_html("Migrated Back To SSR", "SKU-SSR-REC", "129", &[]),
                )
            }),
        );
    let (base, shutdown) = spawn_server(app).await;

    // 1. 影子价格真实影响执行控制：
    // 对 4 个待抓路径赋予“边缘浏览器增益（marginal browser_gain=0.25） + embedded_json 经验 + 高压浏览器预算（remaining=1）”，
    // 验证候选在 Tick 执行过程中一旦达到高影子价格，后续候选立即切换为 `browserEscalationPermitted = false`，
    // 不阻塞剩余 HTTP 请求也不无条件启动多余浏览器。
    let tbl = FamilyExperienceTable::new();
    let shell_fam = family(&format!("{base}/spa/shell/1"));
    for k in 0..8 {
        tbl.record_observation(
            &shell_fam,
            &FamilyObservation {
                observed_at_ms: 1_000 + k * 100,
                http_sufficient: Some(k % 4 != 0),
                browser_gain: Some(k % 4 == 0),
                embedded_json_observed: Some(true),
                http_cost_ms: Some(30.0),
                browser_cost_ms: Some(1_800.0),
                cost_ms: 80.0,
                ..Default::default()
            },
        );
    }

    let mut sp_spec = sample_spec("s-t28-sp", format!("{base}/spa/shell/1"));
    sp_spec.seeds = (1..=4).map(|i| format!("{base}/spa/shell/{i}")).collect();
    sp_spec.allow_browser = true;
    sp_spec.required_fields = Vec::new();
    sp_spec.extract_schema = None;
    sp_spec.budget = SyncBudget {
        max_requests_per_tick: 12,
        max_browser_renders_per_tick: 2, // 第 1 次若占用或起始 fraction 压力较低，耗完即触发高影子价格卸载且不中止 HTTP
        max_wall_clock_ms: 30_000,
        target_items: None,
    };
    store.sync().upsert_source(sp_spec).unwrap();
    store
        .sync()
        .put_experience_bytes("s-t28-sp", &tbl.encode_json(), 2_000)
        .unwrap();

    let (d_sp, _) = tick_and_get(&scraper, &store, "s-t28-sp", run_opts()).await;
    assert!(
        d_sp.stats.browser_renders_used <= 2,
        "shadow price & budget must strictly bound real browser launches: {:?}",
        d_sp.stats
    );
    assert!(
        d_sp.stats.attempted_total == 4,
        "all 4 resources must complete execution rather than aborting mid-tick on browser pressure: {:?}",
        d_sp.stats
    );
    let has_offloaded = d_sp.adapt_decisions.iter().any(|v| {
        v.get("browserEscalationPermitted")
            .and_then(|b| b.as_bool())
            == Some(false)
    });
    assert!(
        has_offloaded,
        "live decision under browser pressure must set browserEscalationPermitted=false: {:?}",
        d_sp.adapt_decisions
    );

    // 2. DirectBrowser -> HttpVerifyBrowser 真实恢复探测链路验证：
    // 预设为确认 SPA（http_sufficient < 0.25, browser_gain > 0.60, consecutive_direct_browser = 6）且旧记录带有 stale shell ETag
    let tbl_rec = FamilyExperienceTable::new();
    let ssr_fam = family(&format!("{base}/spa/ssr/1"));
    for k in 0..8 {
        tbl_rec.record_observation(
            &ssr_fam,
            &FamilyObservation {
                observed_at_ms: 1_000 + k * 100,
                http_sufficient: Some(false),
                browser_gain: Some(true),
                used_browser: k > 0,
                browser_cost_ms: Some(900.0),
                cost_ms: 900.0,
                ..Default::default()
            },
        );
    }
    let encoded_bytes = {
        // 用新表吸收足够 consecutive_direct_browser 触发 HttpVerifyBrowser 探测窗
        let s = tbl_rec;
        for j in 0..6 {
            s.record_observation(
                &ssr_fam,
                &FamilyObservation {
                    observed_at_ms: 5_000 + j * 100,
                    used_browser: true,
                    browser_gain: Some(true),
                    browser_cost_ms: Some(900.0),
                    cost_ms: 900.0,
                    ..Default::default()
                },
            );
        }
        s.encode_json()
    };

    let mut rec_spec = sample_spec("s-t28-rec", format!("{base}/spa/ssr/1"));
    rec_spec.allow_browser = true;
    rec_spec.required_fields = Vec::new();
    rec_spec.extract_schema = None;
    store.sync().upsert_source(rec_spec).unwrap();
    store
        .sync()
        .put_experience_bytes("s-t28-rec", &encoded_bytes, 6_000)
        .unwrap();

    // Inject existing baseline with shell ETag to ensure ConditionalHttp does not hijack SPA->SSR recovery
    let now_rec = pagesift_store::now_store_ms();
    let mut seed_inv = store.sync().get_inventory("s-t28-rec").unwrap_or_default();
    if seed_inv.is_empty() {
        let key = pagesift_graph::normalize_resource_key(&format!("{base}/spa/ssr/1"));
        let mut rr = pagesift_store::ResourceRecord {
            resource_key: key,
            url: format!("{base}/spa/ssr/1"),
            canonical_url: None,
            migrated_to: None,
            parent_url: None,
            depth: 0,
            role: pagesift_store::ResourceRole::Seed,
            state: ResourceLifecycleState::Active,
            discovered_at_ms: 1_000,
            last_checked_at_ms: Some(1_000),
            last_attempted_at_ms: Some(1_000),
            last_valid_observed_at_ms: Some(1_000),
            last_substantive_change_at_ms: Some(1_000),
            next_due_at_ms: now_rec.saturating_sub(1_000),
            etag: Some("\"shell-etag\"".into()),
            last_modified: None,
            last_modified_at_ms: None,
            last_modified_dynamic: false,
            content_hash: None,
            latest_doc_id: Some("old-shell".into()),
            last_status_code: Some(200),
            consecutive_failures: 0,
            consecutive_404s: 0,
            importance_weight: 1.0,
            last_strategy: Some("direct_browser".into()),
            title: None,
            claim_count: 0,
            last_validator_contract_id: Some(
                store
                    .sync()
                    .get_source("s-t28-rec")
                    .unwrap()
                    .unwrap()
                    .contract_id(),
            ),
            last_authoritative_status: Some(200),
            baseline_sufficient: true,
            has_embedded_json: false,
            text_signature: None,
            confirmed_gone_at_ms: None,
            learn: Default::default(),
        };
        rr.next_due_at_ms = now_rec.saturating_sub(1_000);
        seed_inv.push(rr);
        store
            .sync()
            .put_inventory("s-t28-rec", &seed_inv, now_rec)
            .unwrap();
    }

    // Tick 1 & Tick 2: 必须优先执行 HttpVerifyBrowser 经 HTTP 成功抽取 SSR 内容而不动用浏览器，且打破锁定
    let (dr1, _) = tick_and_get(&scraper, &store, "s-t28-rec", run_opts_forced()).await;
    assert_eq!(dr1.stats.browser_renders_used, 0);
    assert_eq!(dr1.stats.probe_recoveries, 1);
    let (dr2, _) = tick_and_get(&scraper, &store, "s-t28-rec", run_opts_forced()).await;
    assert_eq!(
        dr2.stats.browser_renders_used, 0,
        "positive probe streak must keep testing HTTP on tick 2 rather than relocking DirectBrowser"
    );
    let post_exp = FamilyExperienceTable::new();
    post_exp.decode_merge_json(
        &store
            .sync()
            .get_experience_bytes("s-t28-rec")
            .unwrap()
            .unwrap(),
    );
    let post_st = post_exp.get_family(&ssr_fam).unwrap();
    assert!(
        post_st.http_sufficient.mean() >= 0.45,
        "real HttpVerifyBrowser probe ticks must lift http_sufficient out of SPA lock, got {}",
        post_st.http_sufficient.mean()
    );

    let _ = shutdown.send(());
}

#[tokio::test]
async fn t29_negative_transfer_single_consume_and_barrier_cleanup_e2e() {
    let dir = tempfile::tempdir().unwrap();
    let store = pagesift_store::Store::open_dir(dir.path()).unwrap();
    let scraper = test_scraper(Some(store.clone()));

    let app = Router::new().route(
        "/cat/v2/items/clean",
        get(|| async move {
            (
                [(header::CONTENT_TYPE, "text/html; charset=utf-8")],
                product_html("Clean SSR Target", "SKU-NEG-CLEAN", "85", &[]),
            )
        }),
    );
    let (base, shutdown) = spawn_server(app).await;

    // Seed sibling source in tenant-alpha with high barrier_rate + http_sufficient=false
    let donor_tbl = FamilyExperienceTable::new();
    let sibling_fam = family(&format!("{base}/cat/v2/items/old"));
    for i in 0..6 {
        donor_tbl.record_observation_for_tenant(
            &sibling_fam,
            Some("tenant-alpha"),
            &FamilyObservation {
                observed_at_ms: 1_000 + i * 100,
                http_sufficient: Some(false),
                browser_gain: Some(true),
                embedded_json_observed: Some(false),
                encountered_barrier: Some(true),
                cost_ms: 600.0,
                ..Default::default()
            },
        );
    }
    let mut s_donor = sample_spec("donor-alpha", format!("{base}/cat/v2/items/clean"));
    s_donor.allow_browser = false;
    store
        .sync_for(Some("tenant-alpha"))
        .upsert_source(s_donor)
        .unwrap();
    store
        .sync_for(Some("tenant-alpha"))
        .put_experience_bytes("donor-alpha", &donor_tbl.encode_json(), 2_000)
        .unwrap();

    let mut s_target = sample_spec("target-alpha", format!("{base}/cat/v2/items/clean"));
    s_target.allow_browser = false;
    s_target.required_fields = Vec::new();
    s_target.extract_schema = None;
    store
        .sync_for(Some("tenant-alpha"))
        .upsert_source(s_target)
        .unwrap();

    for _ in 0..2 {
        scraper
            .run_sync_tick(
                "target-alpha",
                SyncRunOptions {
                    tenant: Some("tenant-alpha".into()),
                    force_all: true,
                    ..Default::default()
                },
            )
            .await
            .unwrap();
    }

    let bytes = store
        .sync_for(Some("tenant-alpha"))
        .get_experience_bytes("target-alpha")
        .unwrap()
        .unwrap();
    let tbl = FamilyExperienceTable::new();
    tbl.decode_merge_json(&bytes);
    let st = tbl
        .get_family(&family(&format!("{base}/cat/v2/items/clean")))
        .expect("target stats");
    assert!(st.transfer.rejected);
    assert!(
        (st.http_sufficient.success - 2.0).abs() < 1e-9 && st.http_sufficient.failure.abs() < 1e-9,
        "triggering observation must be consumed exactly once, got {:?}",
        st.http_sufficient
    );
    assert!(
        st.barrier_rate.success.abs() < 1e-9 && (st.barrier_rate.failure - 2.0).abs() < 1e-9,
        "foreign donor barrier_rate prior must be erased on rejection, got {:?}",
        st.barrier_rate
    );

    let _ = shutdown.send(());
}

#[tokio::test]
async fn t30_evidence_active_unverified_clusters_truncation_and_unknown_units() {
    let dir = tempfile::tempdir().unwrap();
    let store = pagesift_store::Store::open_dir(dir.path()).unwrap();
    let scraper = test_scraper(Some(store.clone()));

    let wire_bad = arc_state(AtomicUsize::new(0));
    let mirror_bad = arc_state(AtomicUsize::new(0));
    let wb = wire_bad.clone();
    let mb = mirror_bad.clone();

    let app = Router::new()
        .route(
            "/ev/index",
            get(|| async move {
                let html = r#"<!doctype html><html><body>
                    <a href="/ev/wire">Wire</a>
                    <a href="/ev/mirror">Mirror</a>
                    <a href="/ev/usd">USD</a>
                    <a href="/ev/unk">UNK</a>
                </body></html>"#;
                ([(header::CONTENT_TYPE, "text/html; charset=utf-8")], html)
            }),
        )
        .route(
            "/ev/wire",
            get(move || {
                let flag = wb.load(Ordering::SeqCst);
                async move {
                    if flag == 1 {
                        let mut bad = b"<html><body>".to_vec();
                        bad.extend_from_slice(&[0xFF, 0xFE, 0xFD, 0x80, 0xFF]);
                        bad.extend_from_slice(b"</body></html>");
                        (
                            [(header::CONTENT_TYPE, "text/html; charset=utf-8")],
                            bad,
                        )
                            .into_response()
                    } else {
                        (
                            [(header::CONTENT_TYPE, "text/html; charset=utf-8")],
                            product_html("Wire SKU", "SKU-EV-1", "99", &[]).into_bytes(),
                        )
                            .into_response()
                    }
                }
            }),
        )
        .route(
            "/ev/mirror",
            get(move || {
                let flag = mb.load(Ordering::SeqCst);
                async move {
                    if flag == 1 {
                        let mut bad = b"<html><body>".to_vec();
                        bad.extend_from_slice(&[0xFF, 0xFE, 0xFD, 0x80, 0xFF]);
                        bad.extend_from_slice(b"</body></html>");
                        (
                            [(header::CONTENT_TYPE, "text/html; charset=utf-8")],
                            bad,
                        )
                            .into_response()
                    } else {
                        let mut html = product_html("Mirror SKU", "SKU-EV-1", "99", &[]);
                        html = html.replace(
                            "</main>",
                            "<p>Citation: <a href=\"/ev/wire\">Wire</a></p></main>",
                        );
                        (
                            [(header::CONTENT_TYPE, "text/html; charset=utf-8")],
                            html.into_bytes(),
                        )
                            .into_response()
                    }
                }
            }),
        )
        .route(
            "/ev/usd",
            get(|| async move {
                let ld = json!({
                    "@context": "https://schema.org",
                    "@type": "Product",
                    "sku": "SKU-AMB-ONLY",
                    "offers": { "price": "$66" }
                });
                (
                    [(header::CONTENT_TYPE, "text/html; charset=utf-8")],
                    format!(
                        r#"<!doctype html><html><head><script type="application/ld+json">{ld}</script></head><body><p>US SKU-AMB-ONLY price $66 USD catalog.</p></body></html>"#
                    ),
                )
            }),
        )
        .route(
            "/ev/unk",
            get(|| async move {
                let ld = json!({
                    "@context": "https://schema.org",
                    "@type": "Product",
                    "sku": "SKU-AMB-ONLY",
                    "offers": { "price": 66 }
                });
                (
                    [(header::CONTENT_TYPE, "text/html; charset=utf-8")],
                    format!(
                        r#"<!doctype html><html><head><script type="application/ld+json">{ld}</script></head><body><p>SKU-AMB-ONLY price 66 without currency symbol.</p></body></html>"#
                    ),
                )
            }),
        )
        .route(
            "/trunc/index",
            get(|| async move {
                let mut links = String::new();
                for i in 0..66 {
                    links.push_str(&format!(r#"<a href="/trunc/{i}">P{i}</a> "#));
                }
                (
                    [(header::CONTENT_TYPE, "text/html; charset=utf-8")],
                    format!("<!doctype html><html><body><main>{links}</main></body></html>"),
                )
            }),
        )
        .route(
            "/trunc/{idx}",
            get(|| async move {
                (
                    [(header::CONTENT_TYPE, "text/html; charset=utf-8")],
                    product_html("Bucket Overflow", "SKU-TRUNC", "10", &[]),
                )
            }),
        );
    let (base, shutdown) = spawn_server(app).await;

    let mut spec = sample_spec("s-t30", format!("{base}/ev/index"));
    spec.required_fields = Vec::new();
    spec.extract_schema = None;
    store.sync().upsert_source(spec).unwrap();

    // Tick 1: 两个来源均为 Active，且 UNKNOWN 币种与 USD 不得误报为 confirmed UnitOrCurrencyMismatch
    tick_and_get(&scraper, &store, "s-t30", run_opts()).await;
    let st1 = store
        .sync()
        .build_state_view("s-t30", None)
        .unwrap()
        .unwrap();
    let cl1 = st1
        .fact_clusters
        .iter()
        .find(|c| c.normalized_field == "price" && c.subject_key.contains("SKU-EV-1"))
        .expect("SKU-EV-1 cluster");
    assert_eq!(cl1.verified_support_count, 2);
    assert_eq!(cl1.support_count, 2);
    assert_eq!(cl1.unverified_support_count, 0);
    assert_eq!(cl1.independent_sources, 1);
    assert_eq!(cl1.total_independent_sources, 1);

    assert!(
        !st1.conflicts
            .iter()
            .any(|c| c.subject_key.contains("SKU-AMB-ONLY")
                && c.kind == ConflictKind::UnitOrCurrencyMismatch),
        "UNKNOWN currency + USD with same amount must NOT emit UnitOrCurrencyMismatch: {:?}",
        st1.conflicts
    );

    // Tick 2: /mirror 出现不可靠解码转为 Unverified；fact_cluster 仅 1 个已验证，1 个待验证
    mirror_bad.store(1, Ordering::SeqCst);
    tick_and_get(&scraper, &store, "s-t30", run_opts_forced()).await;
    let st2 = store
        .sync()
        .build_state_view("s-t30", None)
        .unwrap()
        .unwrap();
    let cl2 = st2
        .fact_clusters
        .iter()
        .find(|c| c.normalized_field == "price" && c.subject_key.contains("SKU-EV-1"))
        .expect("SKU-EV-1 cluster tick 2");
    assert_eq!(cl2.verified_support_count, 1);
    assert_eq!(cl2.support_count, 1);
    assert_eq!(cl2.unverified_support_count, 1);
    assert_eq!(cl2.independent_sources, 1);
    assert_eq!(cl2.total_independent_sources, 1);

    // Tick 3: 连带 /wire 也降为 Unverified：support_count / independent_sources 必须回落为 0（绝不假称当前已证实）
    wire_bad.store(1, Ordering::SeqCst);
    tick_and_get(&scraper, &store, "s-t30", run_opts_forced()).await;
    let st3 = store
        .sync()
        .build_state_view("s-t30", None)
        .unwrap()
        .unwrap();
    let cl3 = st3
        .fact_clusters
        .iter()
        .find(|c| c.normalized_field == "price" && c.subject_key.contains("SKU-EV-1"))
        .expect("SKU-EV-1 cluster tick 3");
    assert_eq!(cl3.verified_support_count, 0);
    assert_eq!(cl3.support_count, 0);
    assert_eq!(cl3.independent_sources, 0);
    assert_eq!(cl3.unverified_support_count, 2);
    assert_eq!(cl3.total_independent_sources, 1);
    assert!(
        !st3.fact_clusters_truncated,
        "small bucket should report fact_clusters_truncated=false"
    );

    // 验证 fact_clusters_truncated=true 在分桶超上限（> 64）时从 Store 查询直达 SourceStateView 对外暴露
    let mut t_spec = sample_spec("s-t30-trunc", format!("{base}/trunc/index"));
    t_spec.required_fields = Vec::new();
    t_spec.extract_schema = None;
    t_spec.budget.max_requests_per_tick = 100;
    store.sync().upsert_source(t_spec).unwrap();
    tick_and_get(&scraper, &store, "s-t30-trunc", run_opts()).await;
    let st_trunc = store
        .sync()
        .build_state_view("s-t30-trunc", None)
        .unwrap()
        .unwrap();
    assert!(
        st_trunc.fact_clusters_truncated,
        "exceeding 64-claim bucket limit must propagate fact_clusters_truncated=true into SourceStateView"
    );
    let _check_json = serde_json::to_value(&st_trunc).unwrap();
    assert_eq!(
        _check_json
            .get("factClustersTruncated")
            .and_then(|v| v.as_bool()),
        Some(true)
    );

    // 确认 Active_claims 保留两条 Unverified 记录而非丢失
    let unv_ev = st3
        .active_claims
        .iter()
        .filter(|c| {
            c.claim.normalized_field == "price"
                && c.claim.subject_key.contains("SKU-EV-1")
                && matches!(c.status, ClaimStatus::Unverified { .. })
        })
        .count();
    assert_eq!(unv_ev, 2);
    assert_eq!(
        BTreeMap::from([("x", st3.conflicts.len())])
            .get("x")
            .copied(),
        Some(st3.conflicts.len())
    );

    let _ = shutdown.send(());
}
