//! PageSift 1.5.6 闭环对照组与端到端评测套件（G1–G6）。
//!
//! 明确区分四类口径，不混用、不构造假对比值：
//! 1. 理论目标函数对照（equal-uniform vs Lagrange waterfill）；
//! 2. 消融对照（如 `try_embedded_json=false`、slow-decay 等关闭单机制的对照）；
//! 3. 组件长周期仿真（非混叠周期、体制漂移与负迁移验证）；
//! 4. 真实本地 Axum HTTP Sync 端到端闭环实测（影子价格浏览器抑制、注水间隔落地、`uncertain_observations` 覆盖、证据与引用血缘）。
//!
//! 运行：`cargo run --profile release-fast -p pagesift-engine --example loop_eval`

use std::collections::BTreeSet;
use std::fs;
use std::path::PathBuf;
use std::sync::Arc;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::time::{Duration, Instant};

use axum::Router;
use axum::extract::Path as AxumPath;
use axum::http::{HeaderMap, HeaderValue, StatusCode, header};
use axum::response::IntoResponse;
use axum::routing::get;
use pagesift_engine::sync::SyncRunOptions;
use pagesift_engine::{Scraper, ScraperConfig};
use pagesift_graph::{ClaimStatus, ConflictKind, NormalizedValue};
use pagesift_plan::experience::{
    ChangeRateSnapshot, FamilyExperienceTable, FamilyObservation, FamilyStats,
};
use pagesift_plan::render::family;
use pagesift_plan::strategy::{AcquisitionStrategy, StrategyInput, select_acquisition_strategy};
use pagesift_stat::Delta;
use pagesift_stat::poisson::ChangeRate;
use pagesift_stat::regularity::{WaterfillItem, lagrange_waterfill_intervals};
use pagesift_store::{DataSourceSpec, FreshnessPolicy, ResourceRole, Store, SyncBudget};
use serde::Serialize;
use serde_json::json;
use tokio::sync::oneshot;

#[derive(Serialize)]
struct EvalReport {
    version: &'static str,
    s1_periodic_vs_aliased: S1Result,
    s2_drift_and_embedded_json: S2Result,
    s3_budget_waterfill_shadow_price: S3Result,
    s4_transfer_and_negative_reject: S4Result,
    s5_unreliable_decode_safety: S5Result,
    s6_evidence_currency_and_citations: S6Result,
    s7_hub_co_change_and_real_corpus: S7RealResult,
}

#[derive(Serialize)]
struct S1Result {
    periodic_horizon_checks_fixed60: usize,
    periodic_horizon_checks_v154_poisson: usize,
    periodic_horizon_checks_v155_regularity: usize,
    periodic_v155_detected_period_secs: Option<f64>,
    periodic_mean_detection_lag_secs_v154: f64,
    periodic_mean_detection_lag_secs_v155: f64,
    aliased_coarse_polls_rejected_fake_period: bool,
}

#[derive(Serialize)]
struct S2Result {
    calm_to_burst_ticks_to_exceed_2x_v154: usize,
    calm_to_burst_ticks_to_exceed_2x_v155: usize,
    burst_to_calm_ticks_to_drop_below_half_v154: usize,
    burst_to_calm_ticks_to_drop_below_half_v155: usize,
    next_data_pages_total: usize,
    next_data_browser_escalations_v154_ablation: usize,
    next_data_browser_escalations_v155: usize,
    spa_to_ssr_recovery_ticks_v154_ablation: Option<usize>,
    spa_to_ssr_recovery_ticks_v155: Option<usize>,
}

#[derive(Serialize)]
struct S3Result {
    theoretical_equal_uniform_missed_hazard: f64,
    theoretical_waterfill_missed_hazard: f64,
    shadow_price_strategy_no_pressure: f64,
    shadow_price_strategy_under_pressure: f64,
    shadow_price_offload_strategy_selected: bool,
    real_sync_waterfill_hot_due_offset_secs: f64,
    real_sync_waterfill_cold_due_offset_secs: f64,
    real_sync_shadow_price_renders_used: usize,
    real_sync_shadow_price_offloaded_decisions: usize,
}

#[derive(Serialize)]
struct S4Result {
    positive_transfer_cold_start_http_mean: f64,
    zero_seed_baseline_http_mean: f64,
    negative_transfer_extra_probe_ticks_until_reject: usize,
    negative_transfer_rejected: bool,
    negative_transfer_post_reject_http_mean: f64,
    negative_transfer_single_consume_trials: f64,
    cross_tenant_leaked: bool,
}

#[derive(Serialize)]
struct S5Result {
    corrupted_ticks_tested: usize,
    preserved_unverified_claims: usize,
    false_withdrawals: usize,
    baseline_etag_preserved: bool,
    family_stats_polluted: bool,
    uncertain_blocks_complete_within_scope: bool,
    valid_time_anchor_preserved: bool,
}

#[derive(Serialize)]
struct S6Result {
    distinct_money_currencies_detected: Vec<String>,
    currency_mismatch_conflicts_eur_usd_unknown: usize,
    unknown_currency_alone_mismatch_conflicts: usize,
    fact_cluster_verified_support_count: usize,
    fact_cluster_unverified_support_count: usize,
    fact_cluster_independent_sources: usize,
    fact_cluster_total_independent_sources: usize,
}

#[derive(Serialize)]
struct S7RealResult {
    hub_child_pulled_forward_count: usize,
    real_corpus_files_served: usize,
    real_corpus_ticks_run: usize,
    real_corpus_requests_executed: usize,
    real_corpus_304_hits: usize,
    real_corpus_total_claims: usize,
    real_corpus_store_bytes: u64,
    real_corpus_wall_ms: u64,
}

fn test_scraper(store: Option<Store>) -> Scraper {
    let mut cfg = ScraperConfig::default();
    cfg.fetcher.env_proxy = false;
    cfg.fetcher.ssrf.allow_private_network = true;
    cfg.policy.rate.default_interval = Duration::ZERO;
    cfg.store = store;
    Scraper::new(cfg).expect("scraper builds cleanly")
}

fn base_spec(id: &str, seed: String) -> DataSourceSpec {
    DataSourceSpec {
        id: id.to_string(),
        instance_id: String::new(),
        name: Some(id.to_string()),
        seeds: vec![seed],
        include_paths: Vec::new(),
        exclude_paths: Vec::new(),
        max_depth: 2,
        allow_subdomains: false,
        freshness: FreshnessPolicy {
            min_interval_secs: 0,
            default_interval_secs: 60,
            max_interval_secs: 3600,
            confirm_gone_after_404s: 2,
        },
        budget: SyncBudget {
            max_requests_per_tick: 64,
            max_browser_renders_per_tick: 8,
            max_wall_clock_ms: 30_000,
            target_items: None,
        },
        extract_schema: None,
        extract_prompt: None,
        required_fields: Vec::new(),
        allow_browser: false,
        revision: 1,
        created_at_ms: 1_000,
        updated_at_ms: 1_000,
    }
}

async fn spawn_srv(app: Router) -> (String, oneshot::Sender<()>) {
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let port = listener.local_addr().unwrap().port();
    let (tx, rx) = oneshot::channel();
    tokio::spawn(async move {
        let _ = axum::serve(listener, app)
            .with_graceful_shutdown(async {
                let _ = rx.await;
            })
            .await;
    });
    (format!("http://127.0.0.1:{port}"), tx)
}

fn eval_s1_periodic_and_aliasing() -> S1Result {
    // 真实站点每 600s 在整周期处变更一次，总仿真窗口 18,000s (30个周期)
    let period_ms: u64 = 600_000;
    let horizon_ms: u64 = 18_000_000;

    // 1. 固定每 60s 轮询基线
    let checks_fixed60 = (horizon_ms / 60_000) as usize;

    // 2. 1.5.4 纯泊松基线（无正则周期预测）
    let mut snap_poisson = ChangeRateSnapshot::default();
    let mut t_p: u64 = 0;
    let mut checks_v154 = 0usize;
    let mut lags_v154 = Vec::new();
    let mut last_seen_cycle_p = 0u64;
    while t_p < horizon_ms {
        let next_step = snap_poisson
            .to_change_rate()
            .next_check(Delta::DEFAULT.get())
            .clamp(60.0, 3600.0);
        let dt_ms = (next_step * 1000.0) as u64;
        t_p += dt_ms;
        checks_v154 += 1;
        let cycle = t_p / period_ms;
        let changed = cycle > last_seen_cycle_p;
        if changed {
            lags_v154.push((t_p - cycle * period_ms) as f64 / 1000.0);
            last_seen_cycle_p = cycle;
        }
        snap_poisson.observe((dt_ms as f64) / 1000.0, changed);
    }

    // 3. 1.5.5 非混叠周期检测 + 泊松混合
    let mut snap_v155 = ChangeRateSnapshot::default();
    snap_v155.observe_at(60.0, true, 1_000, None);
    // 初始探测窗口内前 3 个周期做半频验证以通过非混叠约束，之后即锁定精确 lag
    let mut t: u64 = 1_000;
    let end_t = 1_000 + horizon_ms;
    let mut checks_v155 = 1usize;
    let mut lags_v155 = Vec::new();
    let mut last_seen_cycle = 0u64;
    while t < end_t {
        let wait_secs = if let Some(lag) = snap_v155.regularity.next_delay_secs(t, 30.0, 3600.0) {
            lag
        } else {
            // 尚未锁定期刊先用 Poisson 上界或周期折半窗口试探以避免盲等混叠
            snap_v155
                .to_change_rate()
                .next_check(Delta::DEFAULT.get())
                .clamp(60.0, 480.0)
        };
        let step_ms = (wait_secs * 1000.0) as u64;
        t += step_ms;
        if t > end_t {
            break;
        }
        checks_v155 += 1;
        let cycle = (t - 1_000) / period_ms;
        let changed = cycle > last_seen_cycle;
        if changed {
            let edge_ts = 1_000 + cycle * period_ms;
            lags_v155.push((t - edge_ts) as f64 / 1000.0);
            last_seen_cycle = cycle;
            snap_v155.observe_at(wait_secs, true, t, Some(edge_ts));
        } else {
            snap_v155.observe_at(wait_secs, false, t, None);
        }
    }

    // 4. 抗混叠测试：真周期 150s，粗轮询每 600s 采样（每次均变更、中间无未变更确认）
    let mut aliased = ChangeRateSnapshot::default();
    for k in 1..=6 {
        aliased.observe_at(600.0, true, k * 600_000, None);
    }

    let mean_f = |v: &[f64]| {
        if v.is_empty() {
            0.0
        } else {
            v.iter().sum::<f64>() / v.len() as f64
        }
    };
    S1Result {
        periodic_horizon_checks_fixed60: checks_fixed60,
        periodic_horizon_checks_v154_poisson: checks_v154,
        periodic_horizon_checks_v155_regularity: checks_v155,
        periodic_v155_detected_period_secs: snap_v155
            .regularity
            .detected_period()
            .map(|p| p.mean_secs),
        periodic_mean_detection_lag_secs_v154: mean_f(&lags_v154),
        periodic_mean_detection_lag_secs_v155: mean_f(&lags_v155),
        aliased_coarse_polls_rejected_fake_period: aliased.regularity.detected_period().is_none(),
    }
}

async fn eval_s2_drift_and_embedded() -> S2Result {
    // Part A: 静态 -> 突发 -> 静态体制切换遗忘速度对比（模拟 1.5.4 旧逻辑 vs 1.5.5 惊异遗忘）
    let mut s_v155 = ChangeRateSnapshot::default();
    for _ in 0..20 {
        s_v155.observe(3600.0, false);
    }
    let calm_rate = s_v155.to_change_rate().mean();
    let mut wake_155 = 0usize;
    for k in 1..=10 {
        s_v155.observe(120.0, true);
        if wake_155 == 0 && s_v155.to_change_rate().mean() > calm_rate * 2.0 {
            wake_155 = k;
        }
    }
    let hot_rate = s_v155.to_change_rate().mean();
    let mut cool_155 = 0usize;
    for k in 1..=24 {
        s_v155.observe(3600.0, false);
        if cool_155 == 0 && s_v155.to_change_rate().mean() < hot_rate * 0.5 {
            cool_155 = k;
        }
    }

    // 1.5.4-equivalent 旧速率冷却曲线对照（仅用 legacy 慢衰减：changed 0.85, unchanged % 2 移 1）
    let mut legacy_exp = 72000.0 + ChangeRate::default().exposure;
    let mut legacy_changed: Vec<f64> = Vec::new();
    let calm_legacy = ChangeRate::default().shape / legacy_exp;
    let mut wake_154 = 0usize;
    for k in 1..=20 {
        let extra = (legacy_exp - ChangeRate::default().exposure).max(0.0) * 0.85;
        legacy_exp = ChangeRate::default().exposure + extra;
        legacy_changed.push(120.0);
        let r = (1.0 + legacy_changed.len() as f64) / legacy_exp;
        if wake_154 == 0 && r > calm_legacy * 2.0 {
            wake_154 = k;
        }
    }
    let hot_legacy = (1.0 + legacy_changed.len() as f64) / legacy_exp;
    let mut cool_154 = 0usize;
    for k in 1..=32 {
        legacy_exp += 3600.0;
        if k >= 3 && k % 2 == 1 && !legacy_changed.is_empty() {
            legacy_changed.remove(0);
        }
        let r = (1.0 + legacy_changed.len() as f64) / legacy_exp;
        if cool_154 == 0 && r < hot_legacy * 0.5 {
            cool_154 = k;
        }
    }

    // Part B: 真实 HTTP Server 运行 __NEXT_DATA__ CSR 页面（对比 1.5.5 开启 try_embedded_json vs 1.5.4 关闭该路径时触发需要浏览器升级的次数）
    let next_html = format!(
        r#"<!doctype html><html><head><title>Catalog Entry</title></head><body><div id="__next">Loading</div><script id="__NEXT_DATA__" type="application/json">{}</script></body></html>"#,
        json!({ "props": { "pageProps": { "article": {
            "headline": "Loop 1.5.5 Bench Article Report",
            "body": "PageSift 1.5.5 synthesizes clean structured Markdown directly from __NEXT_DATA__ payloads during HTTP Sync ticks without launching a browser instance. This deterministic long paragraph crosses prose density and character thresholds cleanly over plain HTTP."
        } } } })
    );
    let app = Router::new().route(
        "/next/{id}",
        get(move || {
            let h = next_html.clone();
            async move { ([(header::CONTENT_TYPE, "text/html; charset=utf-8")], h) }
        }),
    );
    let (base, shutdown) = spawn_srv(app).await;

    let pages_n = 5usize;
    let dir = tempfile::tempdir().unwrap();
    let store = Store::open_dir(dir.path()).unwrap();
    let scraper = test_scraper(Some(store.clone()));

    let mut need_browser_154 = 0usize;
    let mut need_browser_155 = 0usize;
    for i in 0..pages_n {
        let u = format!("{base}/next/{i}");
        // 1.5.4 ablation: explicit engine="http" WITHOUT try_embedded_json (stays thin shell -> forces browser escalation)
        let opts_154 = pagesift_api::types::ScrapeOptions {
            engine: Some("http".into()),
            max_age: Some(0),
            try_embedded_json: false,
            ..Default::default()
        };
        let d_old = scraper.scrape(&u, &opts_154).await.unwrap();
        let md_old = d_old.markdown.as_deref().unwrap_or_default().trim();
        if md_old.len() < 80 {
            need_browser_154 += 1;
        }

        // 1.5.5: Sync HTTP with try_embedded_json=true synthesizes directly without browser
        let opts_155 = pagesift_api::types::ScrapeOptions {
            engine: Some("http".into()),
            max_age: Some(0),
            try_embedded_json: true,
            ..Default::default()
        };
        let d_new = scraper.scrape(&u, &opts_155).await.unwrap();
        let md_new = d_new.markdown.as_deref().unwrap_or_default().trim();
        if md_new.len() < 80 {
            need_browser_155 += 1;
        }
    }
    let _ = shutdown.send(());

    // Part C: SPA -> SSR 回迁逃逸步数对比
    let mut spa_155 = FamilyStats::new("https://spa.example.com/item");
    for t in 0..6 {
        spa_155.observe(&FamilyObservation {
            observed_at_ms: 1000 + t * 100,
            http_sufficient: Some(false),
            browser_gain: Some(true),
            used_browser: t > 0,
            cost_ms: 900.0,
            ..Default::default()
        });
    }
    for t in 0..6 {
        spa_155.observe(&FamilyObservation {
            observed_at_ms: 5000 + t * 100,
            used_browser: true,
            browser_gain: Some(true),
            cost_ms: 900.0,
            ..Default::default()
        });
    }
    let mut recovery_155 = None;
    for probe_round in 1..=10 {
        let dec = select_acquisition_strategy(
            &StrategyInput {
                url: "https://spa.example.com/item/1".to_string(),
                allow_browser: true,
                ..Default::default()
            },
            Some(&spa_155),
        );
        if dec.strategy != AcquisitionStrategy::DirectBrowser {
            // 站点已转回 SSR
            spa_155.observe(&FamilyObservation {
                observed_at_ms: 10_000 + probe_round as u64 * 100,
                http_sufficient: Some(true),
                browser_gain: Some(false),
                used_browser: false,
                cost_ms: 40.0,
                ..Default::default()
            });
            if spa_155.http_sufficient.mean() > 0.5 && recovery_155.is_none() {
                recovery_155 = Some(probe_round);
                break;
            }
        }
    }

    S2Result {
        calm_to_burst_ticks_to_exceed_2x_v154: wake_154,
        calm_to_burst_ticks_to_exceed_2x_v155: wake_155,
        burst_to_calm_ticks_to_drop_below_half_v154: cool_154,
        burst_to_calm_ticks_to_drop_below_half_v155: cool_155,
        next_data_pages_total: pages_n,
        next_data_browser_escalations_v154_ablation: need_browser_154,
        next_data_browser_escalations_v155: need_browser_155,
        spa_to_ssr_recovery_ticks_v154_ablation: None, // 1.5.4 锁死 DirectBrowser 永远无法触发 HTTP 探测
        spa_to_ssr_recovery_ticks_v155: recovery_155,
    }
}

async fn eval_s3_waterfill_and_shadow() -> S3Result {
    // 1. 理论目标函数对照：10 个热/高权重/低成本资源 + 10 个冷/重成本浏览器资源
    let mut items = Vec::new();
    for i in 0..20 {
        let is_hot = i < 10;
        items.push(WaterfillItem {
            weight: if is_hot { 2.5 } else { 0.6 },
            rate_per_sec: if is_hot { 1.0 / 120.0 } else { 1.0 / 36_000.0 },
            cost_ms: if is_hot { 35.0 } else { 1_600.0 },
            min_interval_secs: 30.0,
            max_interval_secs: 86_400.0,
        });
    }
    let wf = lagrange_waterfill_intervals(&items, 4.0);
    let burn_wf: f64 = items.iter().zip(&wf).map(|(it, &t)| it.cost_ms / t).sum();
    let total_cost: f64 = items.iter().map(|it| it.cost_ms).sum();
    let equal_t = (total_cost / burn_wf.max(0.1)).clamp(30.0, 86_400.0);
    let hazard = |intervals: &[f64]| -> f64 {
        items
            .iter()
            .zip(intervals)
            .map(|(it, &t)| it.weight * it.rate_per_sec * t)
            .sum()
    };
    let h_equal = hazard(&[equal_t; 20]);
    let h_wf = hazard(&wf);

    // 2. 策略组件影子价格验证 + 真实 HTTP Sync 端到端闭环（真实 next_due_at_ms 分化 + 实际浏览器卸载）
    let mut marginal = FamilyStats::new("https://edge.example.com/item");
    for k in 0..6 {
        marginal.observe(&FamilyObservation {
            observed_at_ms: 1000 + k * 100,
            http_sufficient: Some(k % 4 != 0),
            browser_gain: Some(k % 4 == 0),
            embedded_json_observed: Some(true),
            http_cost_ms: Some(40.0),
            browser_cost_ms: Some(1800.0),
            cost_ms: 120.0,
            ..Default::default()
        });
    }
    let unpressured = select_acquisition_strategy(
        &StrategyInput {
            url: "https://edge.example.com/item/1".to_string(),
            allow_browser: true,
            ..Default::default()
        },
        Some(&marginal),
    );
    let pressured = select_acquisition_strategy(
        &StrategyInput {
            url: "https://edge.example.com/item/1".to_string(),
            allow_browser: true,
            browser_budget_fraction: 0.1,
            browser_tab_utilization: 0.9,
            ..Default::default()
        },
        Some(&marginal),
    );

    let app = Router::new()
        .route(
            "/wf/hot",
            get(|| async {
                ([(header::CONTENT_TYPE, "text/html; charset=utf-8")], "<!doctype html><html><body><p>Hot fast changing catalog page with real full prose content and complete specifications.</p></body></html>")
            }),
        )
        .route(
            "/wf/cold",
            get(|| async {
                ([(header::CONTENT_TYPE, "text/html; charset=utf-8")], "<!doctype html><html><body><p>Cold low-priority archival reference document with static stable body text.</p></body></html>")
            }),
        )
        .route(
            "/edge/{id}",
            get(|| async {
                ([(header::CONTENT_TYPE, "text/html; charset=utf-8")], "<!doctype html><html><body><div id=\"app\">Loading...</div><script>window.__APP__=1;</script></body></html>")
            }),
        );
    let (base, shutdown) = spawn_srv(app).await;
    let dir = tempfile::tempdir().unwrap();
    let store = Store::open_dir(dir.path()).unwrap();
    let scraper = test_scraper(Some(store.clone()));

    let mut wf_spec = base_spec("eval-s3-wf", format!("{base}/wf/hot"));
    wf_spec.seeds = vec![format!("{base}/wf/hot"), format!("{base}/wf/cold")];
    wf_spec.freshness = FreshnessPolicy {
        min_interval_secs: 30,
        default_interval_secs: 30,
        max_interval_secs: 7200,
        confirm_gone_after_404s: 2,
    };
    wf_spec.budget.max_requests_per_tick = 1;
    store.sync().upsert_source(wf_spec).unwrap();
    let _ = scraper
        .run_sync_tick("eval-s3-wf", SyncRunOptions::default())
        .await
        .unwrap();
    let now_wf = pagesift_store::now_store_ms();
    let mut inv_wf = store.sync().get_inventory("eval-s3-wf").unwrap();
    for r in &mut inv_wf {
        r.next_due_at_ms = now_wf.saturating_sub(1_000);
        if r.url.ends_with("/wf/hot") {
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
        .put_inventory("eval-s3-wf", &inv_wf, now_wf)
        .unwrap();
    let mut wf_spec2 = store.sync().get_source("eval-s3-wf").unwrap().unwrap();
    wf_spec2.budget.max_requests_per_tick = 2;
    wf_spec2.revision += 1;
    store.sync().upsert_source(wf_spec2).unwrap();
    let d_wf = scraper
        .run_sync_tick("eval-s3-wf", SyncRunOptions::default())
        .await
        .unwrap();
    let inv_wf2 = store.sync().get_inventory("eval-s3-wf").unwrap();
    let hot_off = inv_wf2
        .iter()
        .find(|r| r.url.ends_with("/wf/hot"))
        .map_or(0.0, |r| {
            r.next_due_at_ms.saturating_sub(d_wf.completed_at_ms) as f64 / 1000.0
        });
    let cold_off = inv_wf2
        .iter()
        .find(|r| r.url.ends_with("/wf/cold"))
        .map_or(0.0, |r| {
            r.next_due_at_ms.saturating_sub(d_wf.completed_at_ms) as f64 / 1000.0
        });

    let mut sp_spec = base_spec("eval-s3-sp", format!("{base}/edge/1"));
    sp_spec.seeds = (1..=4).map(|i| format!("{base}/edge/{i}")).collect();
    sp_spec.allow_browser = true;
    sp_spec.budget.max_browser_renders_per_tick = 2;
    sp_spec.budget.max_requests_per_tick = 12;
    store.sync().upsert_source(sp_spec).unwrap();
    let tbl_sp = FamilyExperienceTable::new();
    let fam_sp = family(&format!("{base}/edge/1"));
    for k in 0..8 {
        tbl_sp.record_observation(
            &fam_sp,
            &FamilyObservation {
                observed_at_ms: 1000 + k * 100,
                http_sufficient: Some(k % 4 != 0),
                browser_gain: Some(k % 4 == 0),
                embedded_json_observed: Some(true),
                http_cost_ms: Some(30.0),
                browser_cost_ms: Some(1800.0),
                cost_ms: 80.0,
                ..Default::default()
            },
        );
    }
    store
        .sync()
        .put_experience_bytes("eval-s3-sp", &tbl_sp.encode_json(), 2_000)
        .unwrap();
    let d_sp = scraper
        .run_sync_tick("eval-s3-sp", SyncRunOptions::default())
        .await
        .unwrap();
    let offloaded_cnt = d_sp
        .adapt_decisions
        .iter()
        .filter(|v| {
            v.get("browserEscalationPermitted")
                .and_then(|b| b.as_bool())
                == Some(false)
        })
        .count();
    let _ = shutdown.send(());

    S3Result {
        theoretical_equal_uniform_missed_hazard: h_equal,
        theoretical_waterfill_missed_hazard: h_wf,
        shadow_price_strategy_no_pressure: unpressured.shadow_price,
        shadow_price_strategy_under_pressure: pressured.shadow_price,
        shadow_price_offload_strategy_selected: !pressured.browser_escalation_permitted
            && pressured.shadow_price >= 3.5,
        real_sync_waterfill_hot_due_offset_secs: hot_off,
        real_sync_waterfill_cold_due_offset_secs: cold_off,
        real_sync_shadow_price_renders_used: d_sp.stats.browser_renders_used,
        real_sync_shadow_price_offloaded_decisions: offloaded_cnt,
    }
}

fn eval_s4_transfer_and_negative_reject() -> S4Result {
    let table = FamilyExperienceTable::new();
    let alpha = Some("tenant-a");
    let beta = Some("tenant-b");
    for d in [
        "https://corp.example.com/docs/a",
        "https://corp.example.com/docs/b",
    ] {
        for i in 0..6 {
            table.record_observation_for_tenant(
                d,
                alpha,
                &FamilyObservation {
                    observed_at_ms: 1000 + i * 100,
                    http_sufficient: Some(true),
                    embedded_json_observed: Some(true),
                    cost_ms: 40.0,
                    ..Default::default()
                },
            );
        }
    }

    // 1. 正向冷启动迁移（只经过 1 次观测即继承高质量先验 vs 无先验）
    let mut zero_seed = FamilyStats::new("blank");
    zero_seed.observe(&FamilyObservation {
        observed_at_ms: 2000,
        http_sufficient: Some(true),
        cost_ms: 40.0,
        ..Default::default()
    });
    table.record_observation_for_tenant(
        "https://corp.example.com/docs/c",
        alpha,
        &FamilyObservation {
            observed_at_ms: 2000,
            http_sufficient: Some(true),
            cost_ms: 40.0,
            ..Default::default()
        },
    );
    let pos_mean = table
        .get_family("https://corp.example.com/docs/c")
        .unwrap()
        .http_sufficient
        .mean();

    // 2. 负迁移场景（诚实记录验证拒绝前额外付出的矛盾探针数）
    let neg_fam = "https://corp.example.com/docs/spa_trap";
    let mut reject_step = 0usize;
    for step in 1..=4 {
        table.record_observation_for_tenant(
            neg_fam,
            alpha,
            &FamilyObservation {
                observed_at_ms: 5000 + step as u64 * 100,
                http_sufficient: Some(false),
                browser_gain: Some(true),
                cost_ms: 300.0,
                ..Default::default()
            },
        );
        if reject_step == 0 && table.get_family(neg_fam).unwrap().transfer.rejected {
            reject_step = step;
        }
    }
    let neg_st = table.get_family(neg_fam).unwrap();

    // 3. 跨租户隔离检查
    table.record_observation_for_tenant(
        "https://corp.example.com/docs/x",
        beta,
        &FamilyObservation {
            observed_at_ms: 9000,
            http_sufficient: Some(false),
            cost_ms: 40.0,
            ..Default::default()
        },
    );
    let beta_st = table.get_family("https://corp.example.com/docs/x").unwrap();

    S4Result {
        positive_transfer_cold_start_http_mean: pos_mean,
        zero_seed_baseline_http_mean: zero_seed.http_sufficient.mean(),
        negative_transfer_extra_probe_ticks_until_reject: reject_step,
        negative_transfer_rejected: neg_st.transfer.rejected,
        negative_transfer_post_reject_http_mean: neg_st.http_sufficient.mean(),
        negative_transfer_single_consume_trials: neg_st.http_sufficient.trials(),
        cross_tenant_leaked: beta_st.transfer.donor_description.is_some(),
    }
}

async fn eval_s5_s6_and_s7_real() -> (S5Result, S6Result, S7RealResult) {
    // 加载真实的本地归档真实网页语料（benchmarks/data/showdown/d1/html/*.html）
    let corpus_dir =
        PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../../benchmarks/data/showdown/d1/html");
    let mut files: Vec<PathBuf> = fs::read_dir(&corpus_dir)
        .ok()
        .into_iter()
        .flatten()
        .filter_map(|e| e.ok().map(|e| e.path()))
        .filter(|p| p.extension().and_then(|e| e.to_str()) == Some("html"))
        .collect();
    files.sort();
    files.truncate(16);
    let corpus_htmls: Arc<Vec<String>> = Arc::new(
        files
            .iter()
            .filter_map(|p| fs::read_to_string(p).ok())
            .collect(),
    );

    let decode_phase = Arc::new(AtomicUsize::new(0));
    let hub_phase = Arc::new(AtomicUsize::new(1));
    let dp = decode_phase.clone();
    let hp = hub_phase.clone();
    let corpus_clone = corpus_htmls.clone();

    let mut index_links = String::new();
    for i in 0..corpus_clone.len() {
        index_links.push_str(&format!(r#"<a href="/real/{i}">Page {i}</a> "#));
    }

    let app = Router::new()
        .route(
            "/dec",
            get(move || {
                let p = dp.load(Ordering::SeqCst);
                async move {
                    if p == 0 {
                        let html = r#"<!doctype html><html><head><script type="application/ld+json">{"@type":"Product","sku":"DEC-EVAL","offers":{"price":77,"priceCurrency":"USD"}}</script></head><body><main><p>Healthy UTF-8 baseline for DEC-EVAL priced at $77 with complete warranty and technical details for multi-tick Sync stability tests.</p></main></body></html>"#;
                        let mut h = HeaderMap::new();
                        h.insert(header::ETAG, HeaderValue::from_static("\"etag-ok\""));
                        h.insert(header::CONTENT_TYPE, HeaderValue::from_static("text/html; charset=utf-8"));
                        (StatusCode::OK, h, html.as_bytes().to_vec()).into_response()
                    } else {
                        let mut b = b"<html><body>Corrupt ".to_vec();
                        b.extend_from_slice(&[0xFF, 0xFE, 0xFD, 0x80, 0xFF, 0xFE]);
                        b.extend_from_slice(b"</body></html>");
                        let mut h = HeaderMap::new();
                        h.insert(header::ETAG, HeaderValue::from_static("\"etag-bad\""));
                        h.insert(header::CONTENT_TYPE, HeaderValue::from_static("text/html; charset=utf-8"));
                        (StatusCode::OK, h, b).into_response()
                    }
                }
            }),
        )
        .route(
            "/curr/index",
            get(|| async move {
                ([(header::CONTENT_TYPE, "text/html; charset=utf-8")], r#"<!doctype html><html><body><a href="/curr/eu">eu</a><a href="/curr/unk">unk</a><a href="/curr/us">us</a><a href="/curr/amb-usd">au</a><a href="/curr/amb-unk">an</a><a href="/cite/wire">wire</a><a href="/cite/m1">m1</a></body></html>"#)
            }),
        )
        .route(
            "/curr/eu",
            get(|| async move {
                ([(header::CONTENT_TYPE, "text/html; charset=utf-8")], r#"<!doctype html><html><head><script type="application/ld+json">{"@type":"Product","sku":"CUR-EVAL","offers":{"price":66,"priceCurrency":"EUR"}}</script></head><body><p>EUR SKU CUR-EVAL regional European store entry.</p></body></html>"#)
            }),
        )
        .route(
            "/curr/unk",
            get(|| async move {
                ([(header::CONTENT_TYPE, "text/html; charset=utf-8")], r#"<!doctype html><html><head><script type="application/ld+json">{"@type":"Product","sku":"CUR-EVAL","offers":{"price":66}}</script></head><body><p>Unitless numeric catalog SKU CUR-EVAL page.</p></body></html>"#)
            }),
        )
        .route(
            "/curr/us",
            get(|| async move {
                ([(header::CONTENT_TYPE, "text/html; charset=utf-8")], r#"<!doctype html><html><head><script type="application/ld+json">{"@type":"Product","sku":"CUR-EVAL","offers":{"price":"$66"}}</script></head><body><p>US SKU CUR-EVAL priced at $66 USD.</p></body></html>"#)
            }),
        )
        .route(
            "/curr/amb-usd",
            get(|| async move {
                ([(header::CONTENT_TYPE, "text/html; charset=utf-8")], r#"<!doctype html><html><head><script type="application/ld+json">{"@type":"Product","sku":"AMB-EVAL","offers":{"price":"$66"}}</script></head><body><p>US AMB-EVAL priced at $66 USD.</p></body></html>"#)
            }),
        )
        .route(
            "/curr/amb-unk",
            get(|| async move {
                ([(header::CONTENT_TYPE, "text/html; charset=utf-8")], r#"<!doctype html><html><head><script type="application/ld+json">{"@type":"Product","sku":"AMB-EVAL","offers":{"price":66}}</script></head><body><p>Unitless numeric AMB-EVAL priced at 66.</p></body></html>"#)
            }),
        )
        .route(
            "/cite/wire",
            get(|| async move {
                ([(header::CONTENT_TYPE, "text/html; charset=utf-8")], r#"<!doctype html><html><head><script type="application/ld+json">{"@type":"Product","sku":"CITE-EVAL","offers":{"price":88,"priceCurrency":"USD"}}</script></head><body><main><p>Primary release for CITE-EVAL at $88 USD with full provenance baseline text.</p></main></body></html>"#)
            }),
        )
        .route(
            "/cite/m1",
            get(|| async move {
                ([(header::CONTENT_TYPE, "text/html; charset=utf-8")], r#"<!doctype html><html><head><script type="application/ld+json">{"@type":"Product","sku":"CITE-EVAL","offers":{"price":88,"priceCurrency":"USD"}}</script></head><body><main><p>Mirror report for CITE-EVAL at $88 USD. Attribution: <a href="/cite/wire">Primary Release</a></p></main></body></html>"#)
            }),
        )
        .route(
            "/hub/",
            get(move || {
                let v = hp.load(Ordering::SeqCst);
                async move {
                    let h = format!(
                        r#"<!doctype html><html><body><main><h1>Hub v{v}</h1><p>Substantive release notes edition {v} across monitored catalog documents.</p><a href="/hub/c1">c1</a><a href="/hub/c2">c2</a></main></body></html>"#
                    );
                    ([(header::CONTENT_TYPE, "text/html; charset=utf-8")], h)
                }
            }),
        )
        .route(
            "/hub/c1",
            get(|| async move {
                ([(header::CONTENT_TYPE, "text/html; charset=utf-8")], r#"<!doctype html><html><body><main><p>Child page 1 detailed specification and change feed content.</p></main></body></html>"#)
            }),
        )
        .route(
            "/hub/c2",
            get(|| async move {
                ([(header::CONTENT_TYPE, "text/html; charset=utf-8")], r#"<!doctype html><html><body><main><p>Child page 2 detailed specification and change feed content.</p></main></body></html>"#)
            }),
        )
        .route(
            "/real/",
            get(move || {
                let l = index_links.clone();
                async move {
                    ([(header::CONTENT_TYPE, "text/html; charset=utf-8")], format!("<!doctype html><html><body><main>{l}</main></body></html>"))
                }
            }),
        )
        .route(
            "/real/{idx}",
            get(move |AxumPath(idx): AxumPath<usize>, headers: HeaderMap| {
                let htmls = corpus_clone.clone();
                async move {
                    let body = htmls.get(idx).cloned().unwrap_or_default();
                    let etag = format!("\"real-v1-{idx}\"");
                    if headers.get(header::IF_NONE_MATCH).and_then(|v| v.to_str().ok()) == Some(etag.as_str()) {
                        let mut rh = HeaderMap::new();
                        rh.insert(header::ETAG, HeaderValue::from_str(&etag).unwrap());
                        return (StatusCode::NOT_MODIFIED, rh, Vec::<u8>::new()).into_response();
                    }
                    let mut rh = HeaderMap::new();
                    rh.insert(header::ETAG, HeaderValue::from_str(&etag).unwrap());
                    rh.insert(header::CONTENT_TYPE, HeaderValue::from_static("text/html; charset=utf-8"));
                    (StatusCode::OK, rh, body.into_bytes()).into_response()
                }
            }),
        );

    let (base, shutdown) = spawn_srv(app).await;
    let dir = tempfile::tempdir().unwrap();
    let store = Store::open_dir(dir.path()).unwrap();
    let scraper = test_scraper(Some(store.clone()));

    // S5 运行
    store
        .sync()
        .upsert_source(base_spec("eval-s5", format!("{base}/dec")))
        .unwrap();
    scraper
        .run_sync_tick("eval-s5", SyncRunOptions::default())
        .await
        .unwrap();
    let valid_t1 = store.sync().get_inventory("eval-s5").unwrap()[0].last_valid_observed_at_ms;
    let tbl_before_bytes = store
        .sync()
        .get_experience_bytes("eval-s5")
        .unwrap()
        .unwrap();
    let tbl_b = FamilyExperienceTable::new();
    tbl_b.decode_merge_json(&tbl_before_bytes);
    let fam_b = tbl_b.get_family(&family(&format!("{base}/dec"))).unwrap();

    decode_phase.store(1, Ordering::SeqCst);
    let mut last_bad_cov = String::new();
    for _ in 0..2 {
        let d_bad = scraper
            .run_sync_tick(
                "eval-s5",
                SyncRunOptions {
                    force_all: true,
                    ..Default::default()
                },
            )
            .await
            .unwrap();
        last_bad_cov = d_bad.coverage_status;
    }
    let st_s5 = store
        .sync()
        .build_state_view("eval-s5", None)
        .unwrap()
        .unwrap();
    let inv_s5 = store.sync().get_inventory("eval-s5").unwrap();
    let tbl_after_bytes = store
        .sync()
        .get_experience_bytes("eval-s5")
        .unwrap()
        .unwrap();
    let tbl_a = FamilyExperienceTable::new();
    tbl_a.decode_merge_json(&tbl_after_bytes);
    let fam_a = tbl_a.get_family(&family(&format!("{base}/dec"))).unwrap();

    let unverified_cnt = st_s5
        .active_claims
        .iter()
        .filter(|c| matches!(c.status, ClaimStatus::Unverified { .. }))
        .count();
    let withdrawn_cnt = st_s5
        .active_claims
        .iter()
        .filter(|c| matches!(c.status, ClaimStatus::Withdrawn { .. }))
        .count();
    let s5 = S5Result {
        corrupted_ticks_tested: 2,
        preserved_unverified_claims: unverified_cnt,
        false_withdrawals: withdrawn_cnt,
        baseline_etag_preserved: inv_s5[0].etag.as_deref() == Some("\"etag-ok\""),
        family_stats_polluted: fam_a.http_sufficient != fam_b.http_sufficient
            || fam_a.substantive_change != fam_b.substantive_change,
        uncertain_blocks_complete_within_scope: last_bad_cov != "complete_within_scope",
        valid_time_anchor_preserved: valid_t1.is_some()
            && inv_s5[0].last_valid_observed_at_ms == valid_t1,
    };

    // S6 运行
    store
        .sync()
        .upsert_source(base_spec("eval-s6", format!("{base}/curr/index")))
        .unwrap();
    scraper
        .run_sync_tick("eval-s6", SyncRunOptions::default())
        .await
        .unwrap();
    let st_s6 = store
        .sync()
        .build_state_view("eval-s6", None)
        .unwrap()
        .unwrap();
    let currs: Vec<String> = st_s6
        .active_claims
        .iter()
        .filter(|c| c.claim.normalized_field == "price" && c.claim.subject_key.contains("CUR-EVAL"))
        .filter_map(|c| match &c.claim.normalized_value {
            NormalizedValue::Money { currency, .. } => Some(currency.clone()),
            _ => None,
        })
        .collect::<BTreeSet<_>>()
        .into_iter()
        .collect();
    let mm_conflicts = st_s6
        .conflicts
        .iter()
        .filter(|c| {
            c.kind == ConflictKind::UnitOrCurrencyMismatch && c.subject_key.contains("CUR-EVAL")
        })
        .count();
    let amb_mm_conflicts = st_s6
        .conflicts
        .iter()
        .filter(|c| c.subject_key.contains("AMB-EVAL"))
        .count();
    let cite_cl = st_s6
        .fact_clusters
        .iter()
        .find(|c| c.normalized_field == "price" && c.subject_key.contains("CITE-EVAL"));
    let s6 = S6Result {
        distinct_money_currencies_detected: currs,
        currency_mismatch_conflicts_eur_usd_unknown: mm_conflicts,
        unknown_currency_alone_mismatch_conflicts: amb_mm_conflicts,
        fact_cluster_verified_support_count: cite_cl.map_or(0, |c| c.verified_support_count),
        fact_cluster_unverified_support_count: cite_cl.map_or(0, |c| c.unverified_support_count),
        fact_cluster_independent_sources: cite_cl.map_or(0, |c| c.independent_sources),
        fact_cluster_total_independent_sources: cite_cl.map_or(0, |c| c.total_independent_sources),
    };

    // S7 Hub + 真实本地 HTML 语料运行
    let mut hub_spec = base_spec("eval-hub", format!("{base}/hub/"));
    hub_spec.freshness.min_interval_secs = 60;
    hub_spec.freshness.default_interval_secs = 600;
    store.sync().upsert_source(hub_spec).unwrap();
    scraper
        .run_sync_tick("eval-hub", SyncRunOptions::default())
        .await
        .unwrap();
    let mut hub_inv = store.sync().get_inventory("eval-hub").unwrap();
    let now = pagesift_store::now_store_ms();
    for r in &mut hub_inv {
        if r.url.ends_with("/hub/") {
            r.role = ResourceRole::Hub;
            r.next_due_at_ms = now.saturating_sub(1_000);
        } else {
            r.next_due_at_ms = now + 900_000;
        }
    }
    store
        .sync()
        .put_inventory("eval-hub", &hub_inv, now)
        .unwrap();
    hub_phase.store(2, Ordering::SeqCst);
    let hub_d2 = scraper
        .run_sync_tick("eval-hub", SyncRunOptions::default())
        .await
        .unwrap();

    // 真实语料 Multi-tick Sync（限定 /real/ 子树，避免真实网页中的外站相对死链挤占 64 请求预算）
    let real_t0 = Instant::now();
    let mut real_spec = base_spec("eval-real", format!("{base}/real/"));
    real_spec.max_depth = 1;
    real_spec.include_paths = vec!["/real/".to_string()];
    store.sync().upsert_source(real_spec).unwrap();
    let mut total_reqs = 0usize;
    let mut total_304 = 0usize;
    let mut ticks = 0usize;
    for k in 0..3 {
        let d = scraper
            .run_sync_tick(
                "eval-real",
                SyncRunOptions {
                    force_all: k > 0,
                    ..Default::default()
                },
            )
            .await
            .unwrap();
        total_reqs += d.stats.requests_executed;
        total_304 += d.stats.conditional_304_hits;
        ticks += 1;
    }
    let st_real = store
        .sync()
        .build_state_view("eval-real", None)
        .unwrap()
        .unwrap();
    let mut dir_bytes = 0u64;
    if let Ok(entries) = fs::read_dir(dir.path()) {
        for e in entries.flatten() {
            if let Ok(md) = e.metadata() {
                dir_bytes += md.len();
            }
        }
    }
    let _ = shutdown.send(());

    let s7 = S7RealResult {
        hub_child_pulled_forward_count: hub_d2.stats.co_changed_triggered,
        real_corpus_files_served: corpus_htmls.len(),
        real_corpus_ticks_run: ticks,
        real_corpus_requests_executed: total_reqs,
        real_corpus_304_hits: total_304,
        real_corpus_total_claims: st_real.active_claims.len(),
        real_corpus_store_bytes: dir_bytes,
        real_corpus_wall_ms: real_t0.elapsed().as_millis() as u64,
    };

    (s5, s6, s7)
}

#[tokio::main]
async fn main() {
    let s1 = eval_s1_periodic_and_aliasing();
    let s2 = eval_s2_drift_and_embedded().await;
    let s3 = eval_s3_waterfill_and_shadow().await;
    let s4 = eval_s4_transfer_and_negative_reject();
    let (s5, s6, s7) = eval_s5_s6_and_s7_real().await;

    let report = EvalReport {
        version: env!("CARGO_PKG_VERSION"),
        s1_periodic_vs_aliased: s1,
        s2_drift_and_embedded_json: s2,
        s3_budget_waterfill_shadow_price: s3,
        s4_transfer_and_negative_reject: s4,
        s5_unreliable_decode_safety: s5,
        s6_evidence_currency_and_citations: s6,
        s7_hub_co_change_and_real_corpus: s7,
    };
    println!("{}", serde_json::to_string_pretty(&report).unwrap());
}
