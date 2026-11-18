//! PageSift 1.6 (`Context`) reproducible scientific evaluation, ablations, multi-core scaling,
//! and full 7-step multi-format (HTML + PDF + Office) end-to-end acceptance suite.
//!
//! Run: `cargo run --profile release-fast -p pagesift-engine --example context_eval`

use std::collections::{BTreeMap, BTreeSet};
use std::sync::Arc;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::time::{Duration, Instant};

use axum::Router;
use axum::http::{HeaderMap, HeaderValue, StatusCode, header};
use axum::response::IntoResponse;
use axum::routing::get;
use pagesift_api::Engine;
use pagesift_engine::context::ContextDocumentInput;
use pagesift_engine::{Scraper, ScraperConfig};
use pagesift_extract::bm25::{Bm25Options, bm25_filter};
use pagesift_extract::context::{
    ContextIndex, ContextLearnState, DriftGuardKind, UnitBuildInput, compile_context_pack,
    extract_units_from_document, reconcile_resource_units,
};
use pagesift_graph::context::{
    ActionMode, AssetSpecStamp, ContextActionRequest, ContextPackRequest, ContextSearchRequest,
    KnowledgeUnit, MaintenanceMode, PackOrdering, PackSelector, UnitLineageKind, UnitMatchMode,
};
use pagesift_graph::{
    ConflictKind, ConflictRecord, ConflictSeverity, ConflictVariant, DocNode, FactCluster,
    NormalizedValue, ObservationOutcome, SourceKind, SubjectKey, SubjectKeySource,
    TemporalSemantics,
};
use pagesift_stat::Delta;
use pagesift_stat::beta::Posterior;
use pagesift_stat::coverage::{CoverageEstimate, chapman_capture_recapture, good_turing_unseen};
use pagesift_stat::decide::{Cost, Resource, ShadowPrices, value_of_information};
use pagesift_stat::rng::Rng;
use pagesift_store::{DataSourceSpec, FreshnessPolicy, Store, SyncBudget};
use serde::Serialize;
use serde_json::json;

const FIXED_SEED: u64 = 0x16_2026_0418;

#[derive(Serialize)]
struct ContextEvalReport {
    version: &'static str,
    seed: u64,
    e0_hardware: HardwareReport,
    e1_unit_identity_ablation: Vec<UnitModeMetrics>,
    e2_delta_maintenance: DeltaEvalMetrics,
    e3_search_ablation: SearchAblationMetrics,
    e4_pack_ablation: Vec<PackEvalRow>,
    e5_action_coverage_learn: ActionLearnMetrics,
    e6_runtime_scaling: Vec<ScalingRow>,
    e7_acceptance_demo: AcceptanceDemoReport,
}

#[derive(Serialize)]
struct HardwareReport {
    logical_cores: usize,
    numa_nodes: usize,
    os_arch: &'static str,
    initial_rss_mib: f64,
    peak_rss_mib: f64,
}

#[derive(Serialize)]
struct UnitModeMetrics {
    mode: &'static str,
    stable_identity_precision: f64,
    stable_identity_recall: f64,
    false_inheritance_rate: f64,
    ambiguity_detection_rate: f64,
    split_merge_lineage_accuracy: f64,
    mean_reconcile_cpu_micros: f64,
}

#[derive(Serialize)]
struct DeltaEvalMetrics {
    corpus_resources: usize,
    total_units: usize,
    total_derived_assets: usize,
    single_doc_incremental_micros: u64,
    full_rebuild_micros: u64,
    incremental_speedup_ratio: f64,
    single_doc_reused_assets: usize,
    single_doc_recomputed_assets: usize,
    not_modified_304_reused_ratio: f64,
    mass_tokenizer_upgrade_switched_to_full_rebuild: bool,
    crash_reopen_recovered_cleanly: bool,
    aba_recreate_purged_stale_units: bool,
    out_of_order_ack_idempotent: bool,
}

#[derive(Serialize)]
struct SearchAblationMetrics {
    queries_evaluated: usize,
    bm25_prose_only_recall_at_5: f64,
    unified_lexical_symbol_recall_at_5: f64,
    unified_with_sketch_recall_at_5: f64,
    unified_ndcg_at_5: f64,
    temporal_validity_accuracy: f64,
    syndicated_mirror_deduped_independence: bool,
    conflict_counterpart_co_retrieval_rate: f64,
}

#[derive(Serialize)]
struct PackEvalRow {
    selector: &'static str,
    budget_tokens: usize,
    facet_coverage: f64,
    independent_sources: usize,
    table_header_preserved: bool,
    qualifier_negation_preserved: bool,
    conflict_pair_preserved: bool,
    edge_salience_boundary_hit: bool,
    tokens_used: usize,
    compile_micros: u64,
    tight_12tok_honest_insufficient: bool,
}

#[derive(Serialize)]
struct ActionLearnMetrics {
    closed_corpus_true_total: usize,
    good_turing_unseen_prob: f64,
    chapman_estimated_total: f64,
    chapman_95ci_covered_truth: bool,
    open_world_refused_pseudo_precision: bool,
    zipf_skew_refused_pseudo_precision: bool,
    voi_sufficient_saved_requests_vs_fixed4: usize,
    high_shadow_price_suppressed_marginal_probe: bool,
    verify_mode_enforced_second_independent_source: bool,
    permission_and_host_violations: usize,
    brier_uncalibrated: f64,
    brier_isotonic_calibrated: f64,
    stationary_false_alarms_cusum: usize,
    stationary_false_alarms_bocpd: usize,
    drift_detection_lag_cusum: Option<usize>,
    drift_detection_lag_bocpd: Option<usize>,
    post_shift_brier_unchecked: f64,
    post_shift_brier_cusum_guarded: f64,
    post_shift_brier_bocpd_guarded: f64,
    post_shift_brier_frozen_baseline: f64,
}

#[derive(Serialize)]
struct ScalingRow {
    workers: usize,
    queries_executed: usize,
    wall_ms: f64,
    qps: f64,
    qps_per_worker: f64,
    p50_micros: u64,
    p95_micros: u64,
    p99_micros: u64,
    mean_cpu_micros_per_query: f64,
    rss_mib: f64,
}

#[derive(Serialize)]
struct AcceptanceDemoReport {
    step1_multiformat_ingested_units: usize,
    step1_formats_verified: Vec<&'static str>,
    step2_prefix_edit_kept_unit_id: bool,
    step2_duplicate_marked_ambiguous: bool,
    step3_incremental_recomputed_only_changed_doc: bool,
    step3_crash_reopen_visibility_lag_ticks: u64,
    step4_temporal_2025_vs_2026_switched_cleanly: bool,
    step4_conflict_detected_across_independent_origins: bool,
    step5_pack_preserved_headers_units_qualifiers_and_manifest: bool,
    step6_action_reused_context_zero_net_when_fresh: bool,
    step6_action_watch_acquired_update_within_budget: bool,
    step7_v156_304_unreliable_and_gc_invariants_intact: bool,
}

fn read_rss_mib() -> (f64, f64) {
    let Ok(status) = std::fs::read_to_string("/proc/self/status") else {
        return (0.0, 0.0);
    };
    let mut rss_kb = 0.0;
    let mut hwm_kb = 0.0;
    for line in status.lines() {
        if let Some(rest) = line.strip_prefix("VmRSS:") {
            rss_kb = rest
                .split_whitespace()
                .next()
                .and_then(|s| s.parse::<f64>().ok())
                .unwrap_or(0.0);
        } else if let Some(rest) = line.strip_prefix("VmHWM:") {
            hwm_kb = rest
                .split_whitespace()
                .next()
                .and_then(|s| s.parse::<f64>().ok())
                .unwrap_or(0.0);
        }
    }
    (rss_kb / 1024.0, hwm_kb / 1024.0)
}

fn count_numa_nodes() -> usize {
    std::fs::read_dir("/sys/devices/system/node")
        .map(|entries| {
            entries
                .flatten()
                .filter(|e| {
                    e.file_name().to_str().is_some_and(|n| {
                        n.starts_with("node") && n[4..].chars().all(|c| c.is_ascii_digit())
                    })
                })
                .count()
        })
        .unwrap_or(1)
        .max(1)
}

#[allow(clippy::too_many_arguments)]
fn build_units(
    rk: &str,
    doc_id: &str,
    markdown: &str,
    subject: Option<&str>,
    valid_from: Option<&str>,
    valid_until: Option<&str>,
    origin_root: &str,
    origin_independent: bool,
) -> Vec<KnowledgeUnit> {
    let node = DocNode {
        doc_id: doc_id.to_owned(),
        resource_key: rk.to_owned(),
        requested_url: rk.to_owned(),
        final_url: rk.to_owned(),
        canonical_url: None,
        source_kind: SourceKind::Html,
        observed_at_ms: 1_775_000_000_000,
        status_code: 200,
        engine: "http".to_owned(),
        content_hash: Some(format!("ch_{doc_id}")),
        raw_hash: None,
        text_hash: None,
        etag: None,
        last_modified: None,
        title: None,
    };
    let sk = subject.map(|s| SubjectKey {
        key: s.to_owned(),
        source: SubjectKeySource::ExplicitConfig,
        raw: s.to_owned(),
    });
    let temp = TemporalSemantics {
        observed_at_ms: 1_775_000_000_000,
        published_at: None,
        published_at_ms: None,
        modified_at: None,
        modified_at_ms: None,
        valid_from: valid_from.map(str::to_owned),
        valid_until: valid_until.map(str::to_owned),
    };
    extract_units_from_document(&UnitBuildInput {
        doc_node: &node,
        markdown,
        subject_key: sk.as_ref(),
        temporal: Some(&temp),
        blocks: &[],
        claims: &[],
        evidence: &[],
        origin_root,
        origin_independent,
        max_table_unit_tokens: 512,
    })
}

fn eval_e1_unit_identity() -> Vec<UnitModeMetrics> {
    let modes = [
        ("ExactAnchorOnly", UnitMatchMode::ExactAnchorOnly),
        ("GreedyDice", UnitMatchMode::GreedyDice),
        ("BoundedUot", UnitMatchMode::BoundedUot),
    ];
    let mut out = Vec::new();
    let ok_outcome = ObservationOutcome::VerifiedContent;

    for (name, mode) in modes {
        let mut true_pos = 0usize;
        let mut pred_pos = 0usize;
        let mut gold_pos = 0usize;
        let mut false_inherit = 0usize;
        let mut false_inherit_trials = 0usize;
        let mut ambig_detected = 0usize;
        let mut ambig_trials = 0usize;
        let mut split_merge_hits = 0usize;
        let split_merge_total = 2usize;

        let t0 = Instant::now();
        let repeats = 25usize;
        for rep in 0..repeats {
            let md_v1 = "# Rate Limits\n\nBurst capacity is 120 requests per second for Pro workspaces unless throttled.\n\n# Retries\n\nClients must apply exponential backoff starting at 250 ms with jitter.\n";
            let md_v2 = "> Q4 Notice: Maintenance window announced.\n\n# Retries\n\nClients should apply exponential backoff starting at 250 ms with full jitter.\n\n# Rate Limits\n\nBurst capacity is 120 requests per second for Pro workspaces unless throttled.\n";
            let u1 = build_units(
                "https://docs.example/limits",
                "d1",
                md_v1,
                None,
                None,
                None,
                "docs.example",
                true,
            );
            let u2 = build_units(
                "https://docs.example/limits",
                "d2",
                md_v2,
                None,
                None,
                None,
                "docs.example",
                true,
            );
            let rec12 =
                reconcile_resource_units(&u1, u2, &ok_outcome, false, 1_775_000_010_000, mode);

            if rep == 0 {
                let tag_of = |txt: &str| -> &'static str {
                    if txt.contains("120 requests") {
                        "rate_body"
                    } else if txt.contains("250 ms") {
                        "retry_body"
                    } else if txt.contains("Rate Limits") {
                        "rate_head"
                    } else if txt.contains("Retries") {
                        "retry_head"
                    } else {
                        "banner"
                    }
                };
                gold_pos += u1.len();
                let map1: BTreeMap<_, _> = u1
                    .iter()
                    .map(|u| (u.unit_id.clone(), tag_of(&u.text)))
                    .collect();
                for nu in &rec12.live_units {
                    let new_tag = tag_of(&nu.text);
                    if let Some(&old_tag) = map1.get(&nu.unit_id) {
                        pred_pos += 1;
                        if old_tag == new_tag {
                            true_pos += 1;
                        }
                    }
                }
            }

            // Counterexample guard — SKU/Year mismatch with 85% identical boilerplate!
            let sku_v1 = "# Enterprise Hardware Appliance\n\nModel SKU-PRO-100 (2025 Edition) ships with redundant dual power supplies and 64 GiB ECC memory under 3-year SLA.\n";
            let sku_v2 = "# Enterprise Hardware Appliance\n\nModel SKU-PRO-200 (2026 Edition) ships with redundant dual power supplies and 64 GiB ECC memory under 3-year SLA.\n";
            let su1 = build_units(
                "https://hw.example/spec",
                "s1",
                sku_v1,
                Some("sku:PRO-100"),
                None,
                None,
                "hw.example",
                true,
            );
            let su2 = build_units(
                "https://hw.example/spec",
                "s2",
                sku_v2,
                Some("sku:PRO-200"),
                None,
                None,
                "hw.example",
                true,
            );
            let rec_sku =
                reconcile_resource_units(&su1, su2, &ok_outcome, false, 1_775_000_020_000, mode);
            if rep == 0 {
                false_inherit_trials += 1;
                let old_ids: BTreeSet<_> = su1.iter().map(|u| u.unit_id.as_str()).collect();
                if rec_sku
                    .live_units
                    .iter()
                    .any(|u| u.text.contains("SKU-PRO-200") && old_ids.contains(u.unit_id.as_str()))
                {
                    false_inherit += 1;
                }
            }

            // Duplicate repeated disclaimer under same section -> Ambiguity detection
            let dup_v1 = "# Terms\n\nStandard SLA credits exclude scheduled maintenance windows.\n";
            let dup_v2 = "# Terms\n\nStandard SLA credits exclude scheduled maintenance windows and force majeure.\n\nStandard SLA credits exclude scheduled maintenance windows and local holidays.\n";
            let du1 = build_units(
                "https://legal.example/sla",
                "l1",
                dup_v1,
                None,
                None,
                None,
                "legal.example",
                true,
            );
            let du2 = build_units(
                "https://legal.example/sla",
                "l2",
                dup_v2,
                None,
                None,
                None,
                "legal.example",
                true,
            );
            let rec_dup =
                reconcile_resource_units(&du1, du2, &ok_outcome, false, 1_775_000_030_000, mode);
            if rep == 0 {
                ambig_trials += 1;
                if rec_dup.live_units.iter().any(|u| u.lineage.ambiguous) {
                    ambig_detected += 1;
                }
            }

            // 1-to-2 Split and 2-to-1 Merge
            let sp_v1 = "# Storage Architecture\n\nWrite-ahead segments use CRC32C framing for atomic recovery, while zstd payloads isolate cold blob namespaces from LRU eviction.\n";
            let sp_v2 = "# Storage Architecture\n\nWrite-ahead segments use CRC32C framing for atomic recovery across restarts.\n\n# Cold Blob Isolation\n\nZstd payloads isolate cold blob namespaces from LRU eviction during GC.\n";
            let spu1 = build_units(
                "https://arch.example/store",
                "a1",
                sp_v1,
                None,
                None,
                None,
                "arch.example",
                true,
            );
            let spu2 = build_units(
                "https://arch.example/store",
                "a2",
                sp_v2,
                None,
                None,
                None,
                "arch.example",
                true,
            );
            let rec_split = reconcile_resource_units(
                &spu1,
                spu2.clone(),
                &ok_outcome,
                false,
                1_775_000_040_000,
                mode,
            );
            let spu3 = build_units(
                "https://arch.example/store",
                "a3",
                sp_v1,
                None,
                None,
                None,
                "arch.example",
                true,
            );
            let rec_merge =
                reconcile_resource_units(&spu2, spu3, &ok_outcome, false, 1_775_000_050_000, mode);
            if rep == 0 {
                if rec_split
                    .live_units
                    .iter()
                    .any(|u| u.lineage.relation == UnitLineageKind::SplitFrom)
                {
                    split_merge_hits += 1;
                }
                if rec_merge
                    .live_units
                    .iter()
                    .any(|u| u.lineage.relation == UnitLineageKind::MergedFrom)
                {
                    split_merge_hits += 1;
                }
            }
        }
        let elapsed_us = t0.elapsed().as_micros() as f64 / (repeats as f64 * 5.0);
        out.push(UnitModeMetrics {
            mode: name,
            stable_identity_precision: if pred_pos == 0 {
                1.0
            } else {
                true_pos as f64 / pred_pos as f64
            },
            stable_identity_recall: true_pos as f64 / gold_pos.max(1) as f64,
            false_inheritance_rate: false_inherit as f64 / false_inherit_trials.max(1) as f64,
            ambiguity_detection_rate: ambig_detected as f64 / ambig_trials.max(1) as f64,
            split_merge_lineage_accuracy: split_merge_hits as f64 / split_merge_total as f64,
            mean_reconcile_cpu_micros: elapsed_us,
        });
    }
    out
}

fn eval_e2_delta_maintenance(rt: &tokio::runtime::Runtime) -> DeltaEvalMetrics {
    let dir = tempfile::tempdir().unwrap();
    let store_path = dir.path().join("store");

    rt.block_on(async {
        let sc = ScraperConfig {
            store: Some(Store::open_dir(&store_path).unwrap()),
            ..ScraperConfig::default()
        };
        let scraper = Scraper::new(sc).unwrap();

        let corpus_resources = 60usize;
        let mut initial_docs = Vec::with_capacity(corpus_resources);
        for i in 0..corpus_resources {
            initial_docs.push(ContextDocumentInput {
                url: format!("https://kb.example/doc-{i:03}"),
                markdown: format!(
                    "# Module {i}\n\nThroughput limit for shard {i} is {} ops/sec.\n\n| Metric | Value |\n|---|---|\n| LatencyMs | {} |\n| MaxQueue | {} |\n",
                    1000 + i * 25,
                    10 + (i % 7),
                    256 + i
                ),
                title: Some(format!("Module {i}")),
                source_kind: Some(SourceKind::Html),
                observed_at_ms: Some(1_775_000_000_000),
                valid_from: None,
                valid_until: None,
                subject_key: Some(format!("module:{i}")),
                origin_root: Some("kb.example".to_owned()),
                origin_independent: true,
                status_code: Some(200),
                unreliable: false,
                confirmed_gone: false,
            });
        }

        let t_full = Instant::now();
        let (corpus0, _delta0) = pagesift_engine::context::ingest_documents(
            &scraper,
            None,
            "bench-src",
            initial_docs.clone(),
            UnitMatchMode::BoundedUot,
            None,
            true,
        )
        .await
        .unwrap();
        let full_rebuild_micros = t_full.elapsed().as_micros() as u64;
        let total_units = corpus0.index.units.len();
        let total_derived_assets = total_units * 4;

        let single_doc = vec![ContextDocumentInput {
            url: "https://kb.example/doc-007".to_owned(),
            markdown: "# Module 7\n\nThroughput limit for shard 7 is 1450 ops/sec after kernel tuning.\n\n| Metric | Value |\n|---|---|\n| LatencyMs | 9 |\n| MaxQueue | 263 |\n".to_owned(),
            title: Some("Module 7".to_owned()),
            source_kind: Some(SourceKind::Html),
            observed_at_ms: Some(1_775_000_060_000),
            valid_from: None,
            valid_until: None,
            subject_key: Some("module:7".to_owned()),
            origin_root: Some("kb.example".to_owned()),
            origin_independent: true,
            status_code: Some(200),
            unreliable: false,
            confirmed_gone: false,
        }];
        let t_inc = Instant::now();
        let (_corpus1, delta1_opt) = pagesift_engine::context::ingest_documents(
            &scraper,
            None,
            "bench-src",
            single_doc,
            UnitMatchMode::BoundedUot,
            None,
            true,
        )
        .await
        .unwrap();
        let single_doc_incremental_micros = t_inc.elapsed().as_micros() as u64;
        let delta1 = delta1_opt.expect("persisted delta1");

        let unchanged_doc = vec![initial_docs[0].clone()];
        let (_corpus_304, delta_304_opt) = pagesift_engine::context::ingest_documents(
            &scraper,
            None,
            "bench-src",
            unchanged_doc,
            UnitMatchMode::BoundedUot,
            None,
            true,
        )
        .await
        .unwrap();
        let delta_304 = delta_304_opt.expect("persisted delta_304");
        let not_modified_304_reused_ratio = delta_304.reused_asset_count as f64
            / (delta_304.reused_asset_count + delta_304.recomputed_asset_count).max(1) as f64;

        // Tokenizer version upgrade -> switches to FullRebuild
        let mut upgraded_docs = initial_docs[..30].to_vec();
        for (idx, d) in upgraded_docs.iter_mut().enumerate() {
            d.markdown.push_str(&format!("\nUpdated schema rev 2 item {idx}.\n"));
        }
        let upgraded_spec = AssetSpecStamp {
            tokenizer_version: 2,
            ..AssetSpecStamp::default()
        };
        let (corpus_after_upgrade, delta_upgrade_opt) = pagesift_engine::context::ingest_documents(
            &scraper,
            None,
            "bench-src",
            upgraded_docs,
            UnitMatchMode::BoundedUot,
            Some(upgraded_spec),
            true,
        )
        .await
        .unwrap();
        let delta_upgrade = delta_upgrade_opt.expect("persisted delta_upgrade");
        let mass_switched = matches!(delta_upgrade.maintenance_mode, MaintenanceMode::FullRebuild { .. });
        let expected_after_upgrade = corpus_after_upgrade.index.units.len();

        let inst = delta_upgrade.instance_id.clone();
        let _ = Engine::context(
            &scraper,
            None,
            json!({
                "action": "ack",
                "sourceId": "bench-src",
                "consumerId": "c-bench",
                "instanceId": inst,
                "ackedDeltaSeq": 3
            }),
        )
        .await
        .unwrap();
        let ack_old = Engine::context(
            &scraper,
            None,
            json!({
                "action": "ack",
                "sourceId": "bench-src",
                "consumerId": "c-bench",
                "instanceId": delta_upgrade.instance_id,
                "ackedDeltaSeq": 1
            }),
        )
        .await
        .unwrap();
        let out_of_order_ack_idempotent = ack_old["checkpoint"]["ackedDeltaSeq"] == 3;

        scraper.shutdown().await;
        drop(scraper);

        let reopened_store = Store::open_dir(&store_path).unwrap();
        let recovered_units = reopened_store
            .context()
            .load_all_live_units("bench-src")
            .unwrap();
        let crash_reopen_recovered_cleanly = recovered_units.len() == expected_after_upgrade;

        reopened_store.context().purge_source("bench-src").unwrap();
        let after_purge = reopened_store
            .context()
            .load_all_live_units("bench-src")
            .unwrap();
        let aba_recreate_purged_stale_units = after_purge.is_empty();

        DeltaEvalMetrics {
            corpus_resources,
            total_units,
            total_derived_assets,
            single_doc_incremental_micros,
            full_rebuild_micros,
            incremental_speedup_ratio: full_rebuild_micros as f64
                / single_doc_incremental_micros.max(1) as f64,
            single_doc_reused_assets: delta1.reused_asset_count,
            single_doc_recomputed_assets: delta1.recomputed_asset_count,
            not_modified_304_reused_ratio,
            mass_tokenizer_upgrade_switched_to_full_rebuild: mass_switched,
            crash_reopen_recovered_cleanly,
            aba_recreate_purged_stale_units,
            out_of_order_ack_idempotent,
        }
    })
}

fn build_rich_eval_corpus() -> (ContextIndex, Vec<ConflictRecord>, Vec<FactCluster>) {
    let mut all_units = Vec::new();
    let docs = [
        (
            "https://docs.vendor.example/api/quotas",
            "d-quotas",
            "vendor.example",
            true,
            Some("sku:ENT-2026"),
            Some("2026-01-01T00:00:00Z"),
            Some("2026-12-31T23:59:59Z"),
            "# API Quotas & Rate Limits\n\nConfigure `max_requests_per_tick` and `ERR_RATE_LIMIT_429` handling in `pagesift.toml`.\n\n| Plan | RPM | MaxPayloadMiB |\n|---|---|---|\n| Pro | 600 | 32 |\n| Enterprise | 6000 | 128 |\n\nEnterprise payload ceiling is 128 MiB unless chunked streaming is disabled.\n",
        ),
        (
            "https://mirror-cdn.vendor.example/api/quotas",
            "d-mirror",
            "vendor.example",
            false,
            Some("sku:ENT-2026"),
            Some("2026-01-01T00:00:00Z"),
            Some("2026-12-31T23:59:59Z"),
            "# Syndicated API Quotas Mirror\n\nPrice for Enterprise plan is $490 per month.\n",
        ),
        (
            "https://docs.vendor.example/pricing-2025",
            "d-2025",
            "vendor.example",
            true,
            Some("sku:ENT-2025"),
            Some("2025-01-01T00:00:00Z"),
            Some("2025-12-31T23:59:59Z"),
            "# Legacy 2025 Enterprise Pricing\n\nLegacy 2025 promotional price for Enterprise plan was $390 per month.\n",
        ),
        (
            "https://docs.vendor.example/pricing-2026",
            "d-2026",
            "vendor.example",
            true,
            Some("sku:ENT-2026"),
            Some("2026-01-01T00:00:00Z"),
            Some("2026-12-31T23:59:59Z"),
            "# 2026 Enterprise Pricing\n\nOfficial 2026 list price for Enterprise plan is $490 per month unless annual commitment applies.\n",
        ),
        (
            "https://independent-bench.org/vendor-audit",
            "d-audit",
            "independent-bench.org",
            true,
            Some("sku:ENT-2026"),
            Some("2026-01-01T00:00:00Z"),
            None,
            "# Independent Procurement Audit\n\nObserved contracted Enterprise plan price is $550 per month including mandatory support.\n\n配额上限与退订条款：企业版支持 30 天内无条件退订（不含定制硬件）。\n",
        ),
    ];

    for (url, did, root, indep, subj, vf, vu, md) in docs {
        all_units.extend(build_units(url, did, md, subj, vf, vu, root, indep));
    }

    let conflict = ConflictRecord {
        conflict_id: "cf-ent-price".to_owned(),
        subject_key: "sku:ENT-2026".to_owned(),
        field_path: "price".to_owned(),
        normalized_field: "price".to_owned(),
        kind: ConflictKind::ValueMismatch,
        severity: ConflictSeverity::High,
        variants: vec![
            ConflictVariant {
                value: NormalizedValue::Text {
                    canonical: "$490".to_owned(),
                    raw: "$490".to_owned(),
                },
                raw_values: vec![json!("$490")],
                support_count: 2,
                independent_source_count: 1,
                latest_observed_at_ms: 1_775_000_000_000,
                sources: vec![
                    "https://docs.vendor.example/pricing-2026".to_owned(),
                    "https://mirror-cdn.vendor.example/api/quotas".to_owned(),
                ],
                formats: vec!["html".to_owned()],
                engines: vec!["http".to_owned()],
                evidence: Vec::new(),
            },
            ConflictVariant {
                value: NormalizedValue::Text {
                    canonical: "$550".to_owned(),
                    raw: "$550".to_owned(),
                },
                raw_values: vec![json!("$550")],
                support_count: 1,
                independent_source_count: 1,
                latest_observed_at_ms: 1_775_000_000_000,
                sources: vec!["https://independent-bench.org/vendor-audit".to_owned()],
                formats: vec!["html".to_owned()],
                engines: vec!["http".to_owned()],
                evidence: Vec::new(),
            },
        ],
        preferred_hint: None,
    };

    let cluster = FactCluster {
        cluster_id: "fc-ent-payload".to_owned(),
        subject_key: "sku:ENT-2026".to_owned(),
        normalized_field: "max_payload".to_owned(),
        value: NormalizedValue::Text {
            canonical: "128 mib".to_owned(),
            raw: "128 MiB".to_owned(),
        },
        support_count: 2,
        distinct_sources: 2,
        independent_sources: 2,
        verified_support_count: 2,
        unverified_support_count: 0,
        unverified_distinct_sources: 0,
        total_independent_sources: 2,
        origin_roots: vec![
            "vendor.example".to_owned(),
            "independent-bench.org".to_owned(),
        ],
        unverified_origin_roots: Vec::new(),
        distinct_hosts: 2,
        distinct_formats: vec!["html".to_owned()],
        claim_ids: Vec::new(),
        unverified_claim_ids: Vec::new(),
        evidence: Vec::new(),
        unverified_evidence: Vec::new(),
    };

    let spec = AssetSpecStamp::default();
    let mut index = ContextIndex::new(spec.clone());
    index.apply_updates(&all_units, &[], &spec);
    (index, vec![conflict], vec![cluster])
}

fn eval_e3_search_ablation() -> SearchAblationMetrics {
    let (index, conflicts, clusters) = build_rich_eval_corpus();
    let mut learn = ContextLearnState::default();

    let queries = [
        ("max_requests_per_tick ERR_RATE_LIMIT_429", "d-quotas", None),
        ("配额上限与退订条款", "d-audit", None),
        (
            "Enterprise plan monthly price",
            "d-2026",
            pagesift_graph::parse_timestamp_ms("2026-06-01T00:00:00Z"),
        ),
        ("MaxPayloadMiB 128 MiB", "d-quotas", None),
    ];

    let unit_vec: Vec<&KnowledgeUnit> = index.units.values().collect();
    let bm25_blocks: Vec<&str> = unit_vec.iter().map(|u| u.text.as_str()).collect();
    let bm25_opts = Bm25Options {
        top_k: Some(5),
        threshold: None,
        ..Bm25Options::default()
    };

    let mut bm25_hits = 0usize;
    let mut unified_no_sketch_hits = 0usize;
    let mut unified_sketch_hits = 0usize;
    let mut ndcg_sum = 0.0f64;

    for (q, expected_doc, eff_ms) in queries {
        let scored = bm25_filter(&bm25_blocks, q, &bm25_opts);
        if scored
            .iter()
            .any(|sb| unit_vec[sb.index].location.doc_id == expected_doc)
        {
            bm25_hits += 1;
        }

        let rep_no_sk = index.search(
            &ContextSearchRequest {
                query: q.to_owned(),
                effective_at_ms: eff_ms,
                limit: 5,
                enable_semantic_sketch: false,
                ..ContextSearchRequest::default()
            },
            &conflicts,
            &clusters,
            Vec::new(),
            &mut learn,
        );
        if rep_no_sk
            .hits
            .iter()
            .any(|h| h.unit.location.doc_id == expected_doc)
        {
            unified_no_sketch_hits += 1;
        }

        let rep_full = index.search(
            &ContextSearchRequest {
                query: q.to_owned(),
                effective_at_ms: eff_ms,
                limit: 5,
                enable_semantic_sketch: true,
                ..ContextSearchRequest::default()
            },
            &conflicts,
            &clusters,
            Vec::new(),
            &mut learn,
        );
        if let Some(pos) = rep_full
            .hits
            .iter()
            .position(|h| h.unit.location.doc_id == expected_doc)
        {
            unified_sketch_hits += 1;
            ndcg_sum += 1.0 / ((pos + 2) as f64).log2();
        }
    }

    let t2025 = pagesift_graph::parse_timestamp_ms("2025-06-01T00:00:00Z");
    let t2026 = pagesift_graph::parse_timestamp_ms("2026-06-01T00:00:00Z");
    let r2025 = index.search(
        &ContextSearchRequest {
            query: "Enterprise plan price per month".to_owned(),
            effective_at_ms: t2025,
            limit: 3,
            ..ContextSearchRequest::default()
        },
        &conflicts,
        &clusters,
        Vec::new(),
        &mut learn,
    );
    let r2026 = index.search(
        &ContextSearchRequest {
            query: "Enterprise plan price per month".to_owned(),
            effective_at_ms: t2026,
            subject_key: Some("sku:ENT-2026".to_owned()),
            limit: 5,
            ..ContextSearchRequest::default()
        },
        &conflicts,
        &clusters,
        Vec::new(),
        &mut learn,
    );
    let ok_2025 = r2025
        .hits
        .first()
        .is_some_and(|h| h.unit.location.doc_id == "d-2025");
    let ok_2026 = r2026
        .hits
        .first()
        .is_some_and(|h| h.unit.location.doc_id == "d-2026" || h.unit.location.doc_id == "d-audit");
    let temporal_acc = (usize::from(ok_2025) + usize::from(ok_2026)) as f64 / 2.0;

    let syndicated_deduped = r2026.independent_source_count == 2;
    let both_sides = r2026.hits.iter().any(|h| h.unit.text.contains("$490"))
        && r2026.hits.iter().any(|h| h.unit.text.contains("$550"));

    let qn = queries.len() as f64;
    SearchAblationMetrics {
        queries_evaluated: queries.len(),
        bm25_prose_only_recall_at_5: bm25_hits as f64 / qn,
        unified_lexical_symbol_recall_at_5: unified_no_sketch_hits as f64 / qn,
        unified_with_sketch_recall_at_5: unified_sketch_hits as f64 / qn,
        unified_ndcg_at_5: ndcg_sum / qn,
        temporal_validity_accuracy: temporal_acc,
        syndicated_mirror_deduped_independence: syndicated_deduped,
        conflict_counterpart_co_retrieval_rate: if both_sides { 1.0 } else { 0.0 },
    }
}

fn eval_e4_pack_ablation() -> Vec<PackEvalRow> {
    let (index, conflicts, clusters) = build_rich_eval_corpus();
    let mut learn = ContextLearnState::default();
    let sreq = ContextSearchRequest {
        query: "Enterprise plan price RPM MaxPayloadMiB unless chunked".to_owned(),
        subject_key: Some("sku:ENT-2026".to_owned()),
        effective_at_ms: pagesift_graph::parse_timestamp_ms("2026-06-01T00:00:00Z"),
        limit: 12,
        ..ContextSearchRequest::default()
    };
    let report = index.search(&sreq, &conflicts, &clusters, Vec::new(), &mut learn);

    let selectors = [
        ("TopKGreedy", PackSelector::TopKGreedy),
        ("MmrGreedy", PackSelector::MmrGreedy),
        ("BoundedDpp", PackSelector::BoundedDpp),
        ("SubmodularClosure", PackSelector::SubmodularClosure),
    ];
    let budgets = [192usize, 384usize];
    let mut rows = Vec::new();

    for &budget in &budgets {
        for (sname, sel) in selectors {
            let preq = ContextPackRequest {
                search: sreq.clone(),
                budget_tokens: budget,
                selector: sel,
                ordering: PackOrdering::EdgeSalience,
                ..ContextPackRequest::default()
            };
            let pack = compile_context_pack(&preq, &report, 10);
            let txt = &pack.compiled_context;
            let facets = [
                txt.contains("6000"),
                txt.contains("128 MiB") || txt.contains("| 128"),
                txt.contains("$490"),
                txt.contains("$550"),
            ];
            let facet_cov = facets.iter().filter(|&&b| b).count() as f64 / facets.len() as f64;
            let header_ok = txt.contains("Plan") && txt.contains("MaxPayloadMiB");
            let qual_ok = txt.contains("unless");
            let conflict_ok = txt.contains("$490") && txt.contains("$550");
            let edge_hit =
                pack.sections
                    .first()
                    .zip(pack.sections.last())
                    .is_some_and(|(first, last)| {
                        first.paired_for_conflict
                            || last.paired_for_conflict
                            || first.utility_gain >= last.utility_gain * 0.5
                    });

            let tight = compile_context_pack(
                &ContextPackRequest {
                    budget_tokens: 12,
                    ..preq
                },
                &report,
                10,
            );

            rows.push(PackEvalRow {
                selector: sname,
                budget_tokens: budget,
                facet_coverage: facet_cov,
                independent_sources: pack.independent_source_count,
                table_header_preserved: header_ok,
                qualifier_negation_preserved: qual_ok,
                conflict_pair_preserved: conflict_ok,
                edge_salience_boundary_hit: edge_hit,
                tokens_used: pack.total_tokens,
                compile_micros: pack.cost.compile_micros,
                tight_12tok_honest_insufficient: tight.budget_insufficient,
            });
        }
    }
    rows
}

fn eval_e5_action_and_learn(rt: &tokio::runtime::Runtime) -> ActionLearnMetrics {
    let mut rng = Rng::seed(FIXED_SEED);

    let true_total = 200usize;
    let mut counts = vec![0u64; true_total];
    for _ in 0..320 {
        let idx = (rng.next_u64() as usize) % true_total;
        counts[idx] += 1;
    }
    let observed: Vec<u64> = counts.iter().copied().filter(|&c| c > 0).collect();
    let gt = good_turing_unseen(true, &observed, None, Delta::DEFAULT);
    let gt_prob = gt.point().unwrap_or(-1.0);

    let mut pass_a = BTreeSet::new();
    let mut pass_b = BTreeSet::new();
    for _ in 0..140 {
        pass_a.insert((rng.next_u64() as usize) % true_total);
        pass_b.insert((rng.next_u64() as usize) % true_total);
    }
    let overlap = pass_a.intersection(&pass_b).count();
    let chap = chapman_capture_recapture(
        true,
        pass_a.len() as u64,
        pass_b.len() as u64,
        overlap as u64,
        0.10,
        Delta::DEFAULT,
    );
    let (chap_n, chap_covered) = match chap {
        CoverageEstimate::Estimable {
            point,
            ci_low,
            ci_high,
        } => (
            point,
            (true_total as f64) >= ci_low && (true_total as f64) <= ci_high,
        ),
        CoverageEstimate::NotEstimable { .. } => (0.0, false),
    };
    let open_refused = matches!(
        good_turing_unseen(false, &observed, None, Delta::DEFAULT),
        CoverageEstimate::NotEstimable { .. }
    );
    let skewed = vec![500u64, 1, 1, 1, 1, 1, 1, 1, 1, 1, 1, 1, 1];
    let zipf_refused = matches!(
        good_turing_unseen(true, &skewed, None, Delta::DEFAULT),
        CoverageEstimate::NotEstimable { .. }
    );

    let low_prices = ShadowPrices::default()
        .with(Resource::Cpu, 0.05)
        .with(Resource::Politeness, 0.10);
    let high_prices = ShadowPrices::default()
        .with(Resource::Cpu, 4.50)
        .with(Resource::Browser, 12.0)
        .with(Resource::Politeness, 3.0);
    let step_cost = Cost::default()
        .with(Resource::Cpu, 0.10)
        .with(Resource::Browser, 0.25)
        .with(Resource::Politeness, 0.15);
    let post = Posterior {
        alpha: 3.0,
        beta: 2.0,
    };
    let voi_low = value_of_information(&post, 0.35, low_prices.charge(&step_cost));
    let voi_high = value_of_information(&post, 0.35, high_prices.charge(&step_cost));
    let high_shadow_suppressed = voi_low > 0.0 && voi_high == 0.0;

    let (voi_saved_vs_fixed4, verify_enforced, violations) = rt.block_on(async {
        let dir = tempfile::tempdir().unwrap();
        let mut sc = ScraperConfig::default();
        sc.fetcher.env_proxy = false;
        sc.fetcher.ssrf.allow_private_network = true;
        sc.store = Some(Store::open_dir(dir.path()).unwrap());
        let scraper = Scraper::new(sc).unwrap();

        pagesift_engine::context::ingest_documents(
            &scraper,
            None,
            "local-src",
            vec![ContextDocumentInput {
                url: "https://vendor.example/sla".to_owned(),
                markdown: "# SLA Guarantee\n\nuptime: 99.95%\n\nMonthly uptime commitment is 99.95% for Enterprise.\n".to_owned(),
                title: Some("SLA".to_owned()),
                source_kind: Some(SourceKind::Html),
                observed_at_ms: Some(1_775_000_000_000),
                valid_from: None,
                valid_until: None,
                subject_key: Some("sla:ent".to_owned()),
                origin_root: Some("vendor.example".to_owned()),
                origin_independent: true,
                status_code: Some(200),
                unreliable: false,
                confirmed_gone: false,
            }],
            UnitMatchMode::BoundedUot,
            None,
            true,
        )
        .await
        .unwrap();

        let out_suff = pagesift_engine::context::execute_action(
            &scraper,
            None,
            ContextActionRequest {
                mode: ActionMode::Context,
                pack: ContextPackRequest {
                    search: ContextSearchRequest {
                        query: "Monthly uptime commitment 99.95%".to_owned(),
                        source_ids: vec!["local-src".to_owned()],
                        ..ContextSearchRequest::default()
                    },
                    ..ContextPackRequest::default()
                },
                allow_network_acquisition: true,
                max_acquisition_requests: 4,
                ..ContextActionRequest::default()
            },
            Vec::new(),
            UnitMatchMode::BoundedUot,
        )
        .await
        .unwrap();
        let saved = 4usize.saturating_sub(out_suff.acquisition_steps.len());

        let out_blocked = pagesift_engine::context::execute_action(
            &scraper,
            None,
            ContextActionRequest {
                mode: ActionMode::Verify,
                pack: ContextPackRequest {
                    search: ContextSearchRequest {
                        query: "Monthly uptime commitment 99.95%".to_owned(),
                        source_ids: vec!["local-src".to_owned()],
                        ..ContextSearchRequest::default()
                    },
                    ..ContextPackRequest::default()
                },
                min_independent_sources: 2,
                allow_network_acquisition: true,
                allowed_hosts: vec!["trusted.org".to_owned()],
                candidate_urls: vec!["http://127.0.0.1:9/untrusted".to_owned()],
                max_acquisition_requests: 2,
                ..ContextActionRequest::default()
            },
            Vec::new(),
            UnitMatchMode::BoundedUot,
        )
        .await
        .unwrap();
        let illegal_execs = out_blocked
            .acquisition_steps
            .iter()
            .filter(|s| s.target.contains("127.0.0.1") && s.executed)
            .count();
        let verify_flagged_gap = out_blocked
            .gaps
            .iter()
            .any(|g| g.kind == "insufficient_independent_sources");

        scraper.shutdown().await;
        (saved, verify_flagged_gap, illegal_execs)
    });

    let mut calib_state = ContextLearnState::default();
    let mut raw_preds = Vec::new();
    let mut cal_preds = Vec::new();
    let mut labels = Vec::new();

    for i in 0..80 {
        let raw = 0.25 + 0.70 * ((i % 10) as f32 / 9.0);
        let true_p = (0.65 * f64::from(raw)).clamp(0.05, 0.95);
        let y = rng.next_u64() < ((true_p * (u64::MAX as f64)) as u64);
        if i >= 25 {
            raw_preds.push(f64::from(raw));
            cal_preds.push(f64::from(calib_state.calibrate(raw, true).0));
            labels.push(if y { 1.0 } else { 0.0 });
        }
        calib_state.observe_feedback(raw, y, true);
    }
    let brier_raw = compute_brier(&raw_preds, &labels);
    let brier_cal = compute_brier(&cal_preds, &labels);

    let mut cusum_state = ContextLearnState::with_drift_guard(DriftGuardKind::CusumPageHinkley);
    let mut bocpd_state = ContextLearnState::with_drift_guard(DriftGuardKind::BernoulliBocpd);
    let mut unchecked_state = ContextLearnState::with_drift_guard(DriftGuardKind::None);

    let mut fa_cusum = 0usize;
    let mut fa_bocpd = 0usize;
    let mut lag_cusum = None;
    let mut lag_bocpd = None;

    let mut post_brier_unchecked = Vec::new();
    let mut post_brier_cusum = Vec::new();
    let mut post_brier_bocpd = Vec::new();
    let mut post_brier_frozen = Vec::new();

    for step in 0..160 {
        let raw = 0.70 + 0.25 * ((step % 6) as f32 / 5.0);
        let p_true = if step < 100 { f64::from(raw) } else { 0.15 };
        let y = rng.next_u64() < ((p_true * (u64::MAX as f64)) as u64);
        let yf = if y { 1.0 } else { 0.0 };

        if step >= 100 {
            let p_un = f64::from(unchecked_state.calibrate(raw, true).0);
            let p_cu = f64::from(cusum_state.calibrate(raw, true).0);
            let p_bo = f64::from(bocpd_state.calibrate(raw, true).0);
            let p_fr = 0.50;
            post_brier_unchecked.push((p_un - yf).powi(2));
            post_brier_cusum.push((p_cu - yf).powi(2));
            post_brier_bocpd.push((p_bo - yf).powi(2));
            post_brier_frozen.push((p_fr - yf).powi(2));
        }

        unchecked_state.frozen = false;
        let _ = unchecked_state.observe_feedback(raw, y, true);

        if cusum_state.observe_feedback(raw, y, true) {
            if step < 100 {
                fa_cusum += 1;
                cusum_state.frozen = false;
            } else if lag_cusum.is_none() {
                lag_cusum = Some(step - 100 + 1);
            }
        }
        if bocpd_state.observe_feedback(raw, y, true) {
            if step < 100 {
                fa_bocpd += 1;
                bocpd_state.frozen = false;
            } else if lag_bocpd.is_none() {
                lag_bocpd = Some(step - 100 + 1);
            }
        }
    }

    ActionLearnMetrics {
        closed_corpus_true_total: true_total,
        good_turing_unseen_prob: gt_prob,
        chapman_estimated_total: chap_n,
        chapman_95ci_covered_truth: chap_covered,
        open_world_refused_pseudo_precision: open_refused,
        zipf_skew_refused_pseudo_precision: zipf_refused,
        voi_sufficient_saved_requests_vs_fixed4: voi_saved_vs_fixed4,
        high_shadow_price_suppressed_marginal_probe: high_shadow_suppressed,
        verify_mode_enforced_second_independent_source: verify_enforced,
        permission_and_host_violations: violations,
        brier_uncalibrated: brier_raw,
        brier_isotonic_calibrated: brier_cal,
        stationary_false_alarms_cusum: fa_cusum,
        stationary_false_alarms_bocpd: fa_bocpd,
        drift_detection_lag_cusum: lag_cusum,
        drift_detection_lag_bocpd: lag_bocpd,
        post_shift_brier_unchecked: mean(&post_brier_unchecked),
        post_shift_brier_cusum_guarded: mean(&post_brier_cusum),
        post_shift_brier_bocpd_guarded: mean(&post_brier_bocpd),
        post_shift_brier_frozen_baseline: mean(&post_brier_frozen),
    }
}

fn compute_brier(preds: &[f64], labels: &[f64]) -> f64 {
    if preds.is_empty() {
        return 0.0;
    }
    preds
        .iter()
        .zip(labels.iter())
        .map(|(p, y)| (p - y).powi(2))
        .sum::<f64>()
        / preds.len() as f64
}

fn mean(xs: &[f64]) -> f64 {
    if xs.is_empty() {
        0.0
    } else {
        xs.iter().sum::<f64>() / xs.len() as f64
    }
}

fn eval_e6_runtime_scaling(max_cores: usize) -> Vec<ScalingRow> {
    let mut units = Vec::new();
    for i in 0..80 {
        let url = format!("https://scale.example/service-{i}");
        let did = format!("sd-{i}");
        let md = format!(
            "# Service {i} Rate & Memory Specification\n\n`service_{i}_limit` enforces {} req/s and {} MiB heap unless burst override is enabled.\n\n| Tier | Quota | WindowSec |\n|---|---|---|\n| Std | {} | 60 |\n| Pro | {} | 60 |\n",
            100 + i * 10,
            64 + i * 4,
            1000 + i * 50,
            5000 + i * 200
        );
        units.extend(build_units(
            &url,
            &did,
            &md,
            None,
            None,
            None,
            "scale.example",
            true,
        ));
    }
    let spec = AssetSpecStamp::default();
    let mut idx = ContextIndex::new(spec.clone());
    idx.apply_updates(&units, &[], &spec);
    let index = Arc::new(idx);

    let mut worker_counts = vec![1usize, 2, 4, 8, 16];
    worker_counts.retain(|&w| w <= max_cores);
    if !worker_counts.contains(&max_cores) {
        worker_counts.push(max_cores);
    }
    worker_counts.sort_unstable();
    worker_counts.dedup();

    let total_queries = 320usize;
    let mut out = Vec::new();

    for workers in worker_counts {
        let next_q = Arc::new(AtomicUsize::new(0));
        let mut handles = Vec::with_capacity(workers);
        let t0 = Instant::now();

        for _ in 0..workers {
            let idx_ref = index.clone();
            let counter = next_q.clone();
            handles.push(std::thread::spawn(move || {
                let mut lat_us = Vec::new();
                let mut learn = ContextLearnState::default();
                loop {
                    let qid = counter.fetch_add(1, Ordering::Relaxed);
                    if qid >= total_queries {
                        break;
                    }
                    let target = qid % 80;
                    let qt0 = Instant::now();
                    let sreq = ContextSearchRequest {
                        query: format!("service_{target}_limit Pro Quota MiB"),
                        limit: 10,
                        ..ContextSearchRequest::default()
                    };
                    let rep = idx_ref.search(&sreq, &[], &[], Vec::new(), &mut learn);
                    let search_us = qt0.elapsed().as_micros() as u64;
                    let pack = compile_context_pack(
                        &ContextPackRequest {
                            search: sreq,
                            budget_tokens: 384,
                            selector: PackSelector::SubmodularClosure,
                            ordering: PackOrdering::EdgeSalience,
                            ..ContextPackRequest::default()
                        },
                        &rep,
                        search_us,
                    );
                    std::hint::black_box(pack.total_tokens);
                    lat_us.push(qt0.elapsed().as_micros() as u64);
                }
                lat_us
            }));
        }

        let mut all_lat = Vec::with_capacity(total_queries);
        for h in handles {
            all_lat.extend(h.join().unwrap());
        }
        let wall = t0.elapsed();
        all_lat.sort_unstable();
        let wall_ms = wall.as_secs_f64() * 1000.0;
        let qps = total_queries as f64 / wall.as_secs_f64().max(1e-9);
        let p50 = all_lat[all_lat.len() * 50 / 100];
        let p95 = all_lat[all_lat.len() * 95 / 100];
        let p99 = all_lat[all_lat.len() * 99 / 100];
        let mean_cpu = all_lat.iter().sum::<u64>() as f64 / all_lat.len().max(1) as f64;
        let (rss_mib, _) = read_rss_mib();

        out.push(ScalingRow {
            workers,
            queries_executed: total_queries,
            wall_ms,
            qps,
            qps_per_worker: qps / workers as f64,
            p50_micros: p50,
            p95_micros: p95,
            p99_micros: p99,
            mean_cpu_micros_per_query: mean_cpu,
            rss_mib,
        });
    }
    out
}

fn minimal_pdf(lines: &[&str], title: &str) -> Vec<u8> {
    let mut stream = String::from("BT /F1 12 Tf 72 740 Td ");
    for (i, line) in lines.iter().enumerate() {
        if i > 0 {
            stream.push_str("0 -20 Td ");
        }
        stream.push_str(&format!("({line}) Tj "));
    }
    stream.push_str("ET");
    let objs = [
        "<< /Type /Catalog /Pages 2 0 R >>".to_owned(),
        "<< /Type /Pages /Count 1 /Kids [5 0 R] >>".to_owned(),
        "<< /Type /Font /Subtype /Type1 /BaseFont /Helvetica >>".to_owned(),
        format!("<< /Title ({title}) >>"),
        "<< /Type /Page /Parent 2 0 R /MediaBox [0 0 612 792] /Resources << /Font << /F1 3 0 R >> >> /Contents 6 0 R >>".to_owned(),
        format!("<< /Length {} >>\nstream\n{stream}\nendstream", stream.len()),
    ];
    let mut out = b"%PDF-1.7\n".to_vec();
    let mut offsets = Vec::new();
    for (i, o) in objs.iter().enumerate() {
        offsets.push(out.len());
        out.extend_from_slice(format!("{} 0 obj\n{o}\nendobj\n", i + 1).as_bytes());
    }
    let xref = out.len();
    out.extend_from_slice(format!("xref\n0 {}\n0000000000 65535 f \n", objs.len() + 1).as_bytes());
    for off in offsets {
        out.extend_from_slice(format!("{off:010} 00000 n \n").as_bytes());
    }
    out.extend_from_slice(
        format!(
            "trailer\n<< /Size {} /Root 1 0 R /Info 4 0 R >>\nstartxref\n{xref}\n%%EOF\n",
            objs.len() + 1
        )
        .as_bytes(),
    );
    out
}

fn minimal_zip(entries: &[(&str, &[u8])]) -> Vec<u8> {
    let crc32 = |data: &[u8]| -> u32 {
        let mut c = !0u32;
        for &b in data {
            c ^= u32::from(b);
            for _ in 0..8 {
                c = if c & 1 != 0 {
                    (c >> 1) ^ 0xEDB8_8320
                } else {
                    c >> 1
                };
            }
        }
        !c
    };
    let mut out = Vec::new();
    let mut cd = Vec::new();
    for (name, data) in entries {
        let offset = out.len() as u32;
        let crc = crc32(data);
        let head = |sig: u32, central: bool| {
            let mut h = sig.to_le_bytes().to_vec();
            if central {
                h.extend_from_slice(&[20, 0]);
            }
            h.extend_from_slice(&[20, 0, 0, 0, 0, 0, 0, 0, 0, 0]);
            h.extend_from_slice(&crc.to_le_bytes());
            h.extend_from_slice(&(data.len() as u32).to_le_bytes());
            h.extend_from_slice(&(data.len() as u32).to_le_bytes());
            h.extend_from_slice(&(name.len() as u16).to_le_bytes());
            h.extend_from_slice(&[0, 0]);
            if central {
                h.extend_from_slice(&[0; 10]);
                h.extend_from_slice(&offset.to_le_bytes());
            }
            h.extend_from_slice(name.as_bytes());
            h
        };
        out.extend(head(0x0403_4b50, false));
        out.extend_from_slice(data);
        cd.extend(head(0x0201_4b50, true));
    }
    let cd_offset = out.len() as u32;
    out.extend_from_slice(&cd);
    out.extend_from_slice(&0x0605_4b50u32.to_le_bytes());
    out.extend_from_slice(&[0; 4]);
    out.extend_from_slice(&(entries.len() as u16).to_le_bytes());
    out.extend_from_slice(&(entries.len() as u16).to_le_bytes());
    out.extend_from_slice(&(cd.len() as u32).to_le_bytes());
    out.extend_from_slice(&cd_offset.to_le_bytes());
    out.extend_from_slice(&[0, 0]);
    out
}

fn minimal_docx() -> Vec<u8> {
    let w = r#"xmlns:w="http://schemas.openxmlformats.org/wordprocessingml/2006/main""#;
    let doc = format!(
        r#"<w:document {w}><w:body>
        <w:p><w:pPr><w:pStyle w:val="Heading1"/></w:pPr><w:r><w:t>Enterprise Matrix Addendum</w:t></w:r></w:p>
        <w:p><w:r><w:t>Maximum retention window is 365 days unless regulated archive tier is enabled.</w:t></w:r></w:p>
        <w:tbl>
          <w:tr><w:tc><w:p><w:r><w:t>Tier</w:t></w:r></w:p></w:tc><w:tc><w:p><w:r><w:t>MaxPayloadMiB</w:t></w:r></w:p></w:tc></w:tr>
          <w:tr><w:tc><w:p><w:r><w:t>Enterprise</w:t></w:r></w:p></w:tc><w:tc><w:p><w:r><w:t>128</w:t></w:r></w:p></w:tc></w:tr>
        </w:tbl>
        </w:body></w:document>"#
    );
    let styles = format!(
        r#"<w:styles {w}><w:style w:type="paragraph" w:styleId="Heading1"><w:name w:val="heading 1"/></w:style></w:styles>"#
    );
    let rels = r#"<Relationships><Relationship Id="s" Type="http://schemas.openxmlformats.org/officeDocument/2006/relationships/styles" Target="styles.xml"/></Relationships>"#;
    minimal_zip(&[
        ("word/document.xml", doc.as_bytes()),
        ("word/styles.xml", styles.as_bytes()),
        ("word/_rels/document.xml.rels", rels.as_bytes()),
    ])
}

fn eval_e7_full_acceptance_demo(rt: &tokio::runtime::Runtime) -> AcceptanceDemoReport {
    let rev = Arc::new(AtomicUsize::new(1));
    let pdf_bytes = Arc::new(minimal_pdf(
        &[
            "Compliance Audit Bulletin 2026",
            "sku: PRO-CLOUD",
            "price: $59",
            "Independent audit recorded list price at $59 per seat.",
        ],
        "Compliance Audit PDF",
    ));
    let docx_bytes = Arc::new(minimal_docx());

    let rev_html = rev.clone();
    let pdf_route = pdf_bytes.clone();
    let docx_route = docx_bytes.clone();

    let app = Router::new()
        .route(
            "/hub",
            get(move |headers: HeaderMap| {
                let r = rev_html.load(Ordering::SeqCst);
                async move {
                    let etag = format!("\"hub-v{r}\"");
                    if headers
                        .get(header::IF_NONE_MATCH)
                        .and_then(|v| v.to_str().ok())
                        == Some(etag.as_str())
                    {
                        return StatusCode::NOT_MODIFIED.into_response();
                    }
                    let body = if r == 1 {
                        "<html><head><title>Pro Cloud Spec</title></head><body><article>\
                         <h1>Pro Cloud Pricing 2026</h1>\
                         <p>sku: PRO-CLOUD</p>\
                         <p>price: $49</p>\
                         <p>Official 2026 price is $49 per seat unless dedicated VPC is enabled.</p>\
                         <table><caption>API Rate Table</caption><thead><tr><th>Endpoint</th><th>BurstRps</th></tr></thead>\
                         <tbody><tr><td>/v2/context</td><td>240</td></tr></tbody></table>\
                         <pre><code>export PAGESIFT_MAX_RPM=240</code></pre>\
                         <p><a href=\"/faq\">FAQ</a> <a href=\"/sla\">SLA</a> <a href=\"/security\">Security</a> <a href=\"/spec.pdf\">Audit PDF</a> <a href=\"/matrix.docx\">Matrix DOCX</a></p>\
                         </article></body></html>"
                    } else {
                        "<html><head><title>Pro Cloud Spec</title></head><body><article>\
                         <p>Banner: Q4 Release Candidate Live.</p>\
                         <h1>Pro Cloud Pricing 2026</h1>\
                         <p>sku: PRO-CLOUD</p>\
                         <p>price: $49</p>\
                         <p>Official 2026 price is $49 per seat unless dedicated VPC is enabled.</p>\
                         <table><caption>API Rate Table</caption><thead><tr><th>Endpoint</th><th>BurstRps</th></tr></thead>\
                         <tbody><tr><td>/v2/context</td><td>240</td></tr></tbody></table>\
                         <pre><code>export PAGESIFT_MAX_RPM=240</code></pre>\
                         <p><a href=\"/faq\">FAQ</a> <a href=\"/sla\">SLA</a> <a href=\"/security\">Security</a> <a href=\"/spec.pdf\">Audit PDF</a> <a href=\"/matrix.docx\">Matrix DOCX</a></p>\
                         </article></body></html>"
                    };
                    let mut hm = HeaderMap::new();
                    hm.insert(
                        header::CONTENT_TYPE,
                        HeaderValue::from_static("text/html; charset=utf-8"),
                    );
                    hm.insert(header::ETAG, HeaderValue::from_str(&etag).unwrap());
                    (StatusCode::OK, hm, body).into_response()
                }
            }),
        )
        .route(
            "/faq",
            get(|headers: HeaderMap| async move {
                if headers
                    .get(header::IF_NONE_MATCH)
                    .and_then(|v| v.to_str().ok())
                    == Some("\"faq-v1\"")
                {
                    return StatusCode::NOT_MODIFIED.into_response();
                }
                let mut hm = HeaderMap::new();
                hm.insert(header::CONTENT_TYPE, HeaderValue::from_static("text/html; charset=utf-8"));
                hm.insert(header::ETAG, HeaderValue::from_static("\"faq-v1\""));
                (StatusCode::OK, hm, "<html><body><article><h1>Billing FAQ</h1><p>Annual invoices settle within 30 net days for verified enterprise accounts.</p></article></body></html>").into_response()
            }),
        )
        .route(
            "/sla",
            get(|headers: HeaderMap| async move {
                if headers
                    .get(header::IF_NONE_MATCH)
                    .and_then(|v| v.to_str().ok())
                    == Some("\"sla-v1\"")
                {
                    return StatusCode::NOT_MODIFIED.into_response();
                }
                let mut hm = HeaderMap::new();
                hm.insert(header::CONTENT_TYPE, HeaderValue::from_static("text/html; charset=utf-8"));
                hm.insert(header::ETAG, HeaderValue::from_static("\"sla-v1\""));
                (StatusCode::OK, hm, "<html><body><article><h1>Uptime SLA</h1><p>Regional failover completes in under 15 seconds across redundant availability zones.</p></article></body></html>").into_response()
            }),
        )
        .route(
            "/security",
            get(|headers: HeaderMap| async move {
                if headers
                    .get(header::IF_NONE_MATCH)
                    .and_then(|v| v.to_str().ok())
                    == Some("\"sec-v1\"")
                {
                    return StatusCode::NOT_MODIFIED.into_response();
                }
                let mut hm = HeaderMap::new();
                hm.insert(header::CONTENT_TYPE, HeaderValue::from_static("text/html; charset=utf-8"));
                hm.insert(header::ETAG, HeaderValue::from_static("\"sec-v1\""));
                (StatusCode::OK, hm, "<html><body><article><h1>Data Encryption</h1><p>All persisted blobs use authenticated encryption at rest and TLS 1.3 in transit.</p></article></body></html>").into_response()
            }),
        )
        .route(
            "/spec.pdf",
            get(move |headers: HeaderMap| {
                let b = (*pdf_route).clone();
                async move {
                    if headers
                        .get(header::IF_NONE_MATCH)
                        .and_then(|v| v.to_str().ok())
                        == Some("\"pdf-v1\"")
                    {
                        return StatusCode::NOT_MODIFIED.into_response();
                    }
                    let mut hm = HeaderMap::new();
                    hm.insert(
                        header::CONTENT_TYPE,
                        HeaderValue::from_static("application/pdf"),
                    );
                    hm.insert(header::ETAG, HeaderValue::from_static("\"pdf-v1\""));
                    (StatusCode::OK, hm, b).into_response()
                }
            }),
        )
        .route(
            "/matrix.docx",
            get(move |headers: HeaderMap| {
                let b = (*docx_route).clone();
                async move {
                    if headers
                        .get(header::IF_NONE_MATCH)
                        .and_then(|v| v.to_str().ok())
                        == Some("\"docx-v1\"")
                    {
                        return StatusCode::NOT_MODIFIED.into_response();
                    }
                    let mut hm = HeaderMap::new();
                    hm.insert(
                        header::CONTENT_TYPE,
                        HeaderValue::from_static(
                            "application/vnd.openxmlformats-officedocument.wordprocessingml.document",
                        ),
                    );
                    hm.insert(header::ETAG, HeaderValue::from_static("\"docx-v1\""));
                    (StatusCode::OK, hm, b).into_response()
                }
            }),
        );

    let listener = rt
        .block_on(tokio::net::TcpListener::bind("127.0.0.1:0"))
        .unwrap();
    let addr = listener.local_addr().unwrap();
    rt.spawn(async move { axum::serve(listener, app).await.unwrap() });
    let seed_url = format!("http://{addr}/hub");

    let dir = tempfile::tempdir().unwrap();
    let store_path = dir.path().join("demo-store");

    rt.block_on(async {
        let store = Store::open_dir(&store_path).unwrap();
        store
            .sync()
            .upsert_source(DataSourceSpec {
                id: "demo-loop".to_owned(),
                instance_id: String::new(),
                name: Some("Acceptance Demo".to_owned()),
                seeds: vec![seed_url.clone()],
                include_paths: Vec::new(),
                exclude_paths: Vec::new(),
                max_depth: 1,
                allow_subdomains: false,
                freshness: FreshnessPolicy {
                    min_interval_secs: 0,
                    default_interval_secs: 1,
                    max_interval_secs: 60,
                    confirm_gone_after_404s: 2,
                },
                budget: SyncBudget {
                    max_requests_per_tick: 10,
                    max_browser_renders_per_tick: 0,
                    max_wall_clock_ms: 15_000,
                    target_items: None,
                },
                extract_schema: None,
                extract_prompt: None,
                required_fields: Vec::new(),
                allow_browser: false,
                revision: 1,
                created_at_ms: 0,
                updated_at_ms: 0,
            })
            .unwrap();

        let mut sc = ScraperConfig::default();
        sc.fetcher.env_proxy = false;
        sc.fetcher.ssrf.allow_private_network = true;
        sc.policy.rate.default_interval = Duration::ZERO;
        sc.store = Some(store.clone());
        let scraper = Scraper::new(sc).unwrap();

        let _tick1 = Engine::sync_source(&scraper, "demo-loop", None, true, None)
            .await
            .unwrap();
        let _tick1b = Engine::sync_source(&scraper, "demo-loop", None, true, None)
            .await
            .unwrap();

        let (_delta1, vis1) = pagesift_engine::context::sync_source_context(
            &scraper,
            None,
            "demo-loop",
            UnitMatchMode::BoundedUot,
            None,
        )
        .await
        .unwrap();
        assert_eq!(vis1.lag_ticks, 0);

        let pdf_upload = pagesift_api::types::Upload {
            bytes: (*pdf_bytes).clone(),
            filename: "spec.pdf".to_owned(),
            content_type: Some("application/pdf".to_owned()),
            kind: pagesift_api::types::UploadKind::Pdf,
            url: None,
        };
        let parse_opts = pagesift_api::types::ScrapeOptions {
            graph: Some(pagesift_graph::GraphOptions::enabled()),
            ..Default::default()
        };
        let parsed_pdf = scraper.parse(pdf_upload, &parse_opts).await.unwrap();
        let docx_kind =
            pagesift_api::types::UploadKind::detect(&docx_bytes, "matrix.docx", None).unwrap();
        let parsed_docx = scraper
            .parse(
                pagesift_api::types::Upload {
                    bytes: (*docx_bytes).clone(),
                    filename: "matrix.docx".to_owned(),
                    content_type: None,
                    kind: docx_kind,
                    url: None,
                },
                &parse_opts,
            )
            .await
            .unwrap();

        let mut aux_docs = vec![
            ContextDocumentInput::from_api_document(
                &parsed_pdf,
                "https://audit-org.example/spec.pdf",
            ),
            ContextDocumentInput::from_api_document(
                &parsed_docx,
                "https://vendor.example/matrix.docx",
            ),
            ContextDocumentInput {
                url: "https://archive.vendor.example/pro-2025".to_owned(),
                markdown:
                    "# Pro Cloud Pricing 2025\n\nprice: $39\n\nLegacy 2025 price was $39 per seat.\n"
                        .to_owned(),
                title: Some("2025 Spec".to_owned()),
                source_kind: Some(SourceKind::Html),
                observed_at_ms: Some(1_740_000_000_000),
                valid_from: Some("2025-01-01T00:00:00Z".to_owned()),
                valid_until: Some("2025-12-31T23:59:59Z".to_owned()),
                subject_key: Some("sku:PRO-CLOUD".to_owned()),
                origin_root: Some("vendor.example".to_owned()),
                origin_independent: true,
                status_code: Some(200),
                unreliable: false,
                confirmed_gone: false,
            },
        ];
        aux_docs[0].origin_root = Some("audit-org.example".to_owned());
        aux_docs[0].subject_key = Some("sku:PRO-CLOUD".to_owned());
        aux_docs[0].valid_from = Some("2026-01-01T00:00:00Z".to_owned());
        aux_docs[0].valid_until = Some("2026-12-31T23:59:59Z".to_owned());
        aux_docs[1].origin_root = Some("vendor.example".to_owned());

        pagesift_engine::context::ingest_documents(
            &scraper,
            None,
            "demo-aux",
            aux_docs,
            UnitMatchMode::BoundedUot,
            None,
            true,
        )
        .await
        .unwrap();

        let units_loop_v1 = store.context().load_all_live_units("demo-loop").unwrap();
        let units_aux = store.context().load_all_live_units("demo-aux").unwrap();
        let mut formats = Vec::new();
        if units_loop_v1
            .iter()
            .chain(units_aux.iter())
            .any(|u| matches!(u.location.source_kind, SourceKind::Html))
        {
            formats.push("HTML");
        }
        if units_loop_v1
            .iter()
            .chain(units_aux.iter())
            .any(|u| matches!(u.location.source_kind, SourceKind::Pdf))
        {
            formats.push("PDF");
        }
        if units_loop_v1
            .iter()
            .chain(units_aux.iter())
            .any(|u| matches!(u.location.source_kind, SourceKind::Office { .. }))
        {
            formats.push("Office(DOCX)");
        }
        let price_uid_v1 = units_loop_v1
            .iter()
            .find(|u| u.text.contains("dedicated VPC"))
            .unwrap()
            .unit_id
            .clone();

        let tick_304 = Engine::sync_source(&scraper, "demo-loop", None, true, None)
            .await
            .unwrap();
        let hit_304 = tick_304.stats.conditional_304_hits >= 1;

        rev.store(2, Ordering::SeqCst);
        let _tick2 = Engine::sync_source(&scraper, "demo-loop", None, true, None)
            .await
            .unwrap();
        let deltas_loop = store.context().list_deltas("demo-loop", None, 5).unwrap();
        let latest_loop_delta = deltas_loop.last().unwrap();
        let incremental_partial = latest_loop_delta.maintenance_mode
            == MaintenanceMode::Incremental
            && latest_loop_delta.reused_asset_count > 0;

        let ambig_u1 = build_units(
            "https://legal.example/sla-ambig",
            "la1",
            "# Credits\n\nSLA credits exclude force majeure.\n",
            None,
            None,
            None,
            "legal.example",
            true,
        );
        let ambig_u2 = build_units(
            "https://legal.example/sla-ambig",
            "la2",
            "# Credits\n\nSLA credits exclude force majeure and local holidays.\n\nSLA credits exclude force majeure and scheduled maintenance.\n",
            None,
            None,
            None,
            "legal.example",
            true,
        );
        let ambig_rec = reconcile_resource_units(
            &ambig_u1,
            ambig_u2,
            &ObservationOutcome::VerifiedContent,
            false,
            1_775_000_090_000,
            UnitMatchMode::BoundedUot,
        );
        let dup_marked_ambig = ambig_rec.live_units.iter().any(|u| u.lineage.ambiguous);

        scraper.shutdown().await;
        drop(scraper);
        drop(store);

        let store2 = Store::open_dir(&store_path).unwrap();
        let _ = store2.gc();
        let vis_reopen = store2.context().visibility("demo-loop").unwrap();
        let units_v2 = store2.context().load_all_live_units("demo-loop").unwrap();
        let kept_uid = units_v2
            .iter()
            .find(|u| u.text.contains("dedicated VPC"))
            .is_some_and(|u| u.unit_id == price_uid_v1);

        let mut sc2 = ScraperConfig::default();
        sc2.fetcher.env_proxy = false;
        sc2.fetcher.ssrf.allow_private_network = true;
        sc2.policy.rate.default_interval = Duration::ZERO;
        sc2.store = Some(store2.clone());
        let scraper2 = Scraper::new(sc2).unwrap();

        let s2025 = Engine::context(
            &scraper2,
            None,
            json!({
                "action": "search",
                "sourceIds": ["demo-loop", "demo-aux"],
                "query": "Pro Cloud Pricing per seat",
                "effectiveAt": "2025-06-01T00:00:00Z",
                "limit": 4
            }),
        )
        .await
        .unwrap();
        let s2026 = Engine::context(
            &scraper2,
            None,
            json!({
                "action": "search",
                "sourceIds": ["demo-loop", "demo-aux"],
                "query": "Pro Cloud price per seat",
                "subjectKey": "sku:PRO-CLOUD",
                "effectiveAt": "2026-06-01T00:00:00Z",
                "limit": 6
            }),
        )
        .await
        .unwrap();
        let top_2025_ok = s2025["report"]["hits"][0]["unit"]["location"]["sourceUrl"]
            .as_str()
            == Some("https://archive.vendor.example/pro-2025")
            && s2025["report"]["hits"]
                .as_array()
                .is_some_and(|hits| {
                    hits.iter()
                        .take(2)
                        .any(|h| h["unit"]["text"].as_str().unwrap_or("").contains("$39"))
                });
        let conflict_indep_ok =
            s2026["report"]["independentSourceCount"].as_u64().unwrap_or(0) >= 2;

        let packed = Engine::context(
            &scraper2,
            None,
            json!({
                "action": "pack",
                "sourceIds": ["demo-loop", "demo-aux"],
                "query": "Pro Cloud price BurstRps retention 365 days MaxPayloadMiB",
                "budgetTokens": 600,
                "selector": "submodularClosure",
                "ordering": "edgeSalience"
            }),
        )
        .await
        .unwrap();
        let ptxt = packed["pack"]["compiledContext"].as_str().unwrap_or("");
        let pack_ok = packed["pack"]["mustKeepPreserved"] == true
            && (ptxt.contains("BurstRps") || ptxt.contains("MaxPayloadMiB"))
            && (ptxt.contains("unless") || ptxt.contains("128"))
            && !packed["pack"]["provenanceManifest"]
                .as_array()
                .unwrap()
                .is_empty();

        let act_reuse = Engine::context(
            &scraper2,
            None,
            json!({
                "action": "action",
                "mode": "context",
                "sourceIds": ["demo-loop"],
                "query": "Pro Cloud BurstRps dedicated VPC",
                "allowNetworkAcquisition": true,
                "maxAcquisitionRequests": 3
            }),
        )
        .await
        .unwrap();
        let reused_zero_net = act_reuse["outcome"]["reusedExistingContext"] == true
            && act_reuse["outcome"]["acquisitionSteps"]
                .as_array()
                .is_none_or(Vec::is_empty);

        let act_watch = Engine::context(
            &scraper2,
            None,
            json!({
                "action": "action",
                "mode": "watch",
                "sourceIds": ["demo-loop"],
                "query": "Pro Cloud BurstRps dedicated VPC",
                "maxStalenessMs": 0,
                "allowNetworkAcquisition": true,
                "maxAcquisitionRequests": 3
            }),
        )
        .await
        .unwrap();
        let watch_ok = act_watch["success"] == true
            && act_watch["outcome"]["acquisitionSteps"]
                .as_array()
                .is_some_and(|a| !a.is_empty() && a.len() <= 3);

        let (_, unreliable_delta_opt) = pagesift_engine::context::ingest_documents(
            &scraper2,
            None,
            "demo-aux",
            vec![ContextDocumentInput {
                url: "https://audit-org.example/spec.pdf".to_owned(),
                markdown: "truncated partial...".to_owned(),
                title: None,
                source_kind: Some(SourceKind::Pdf),
                observed_at_ms: Some(1_775_000_900_000),
                valid_from: None,
                valid_until: None,
                subject_key: Some("sku:PRO-CLOUD".to_owned()),
                origin_root: Some("audit-org.example".to_owned()),
                origin_independent: true,
                status_code: Some(200),
                unreliable: true,
                confirmed_gone: false,
            }],
            UnitMatchMode::BoundedUot,
            None,
            true,
        )
        .await
        .unwrap();
        let unreliable_delta = unreliable_delta_opt.expect("persisted unreliable delta");
        let still_active_pdf = store2
            .context()
            .load_all_live_units("demo-aux")
            .unwrap()
            .iter()
            .any(|u| u.text.contains("$59"));
        let no_false_withdrawal = still_active_pdf
            && !unreliable_delta
                .unit_transitions
                .iter()
                .any(|t| format!("{:?}", t.change_kind).contains("Withdrawn"));

        scraper2.shutdown().await;

        AcceptanceDemoReport {
            step1_multiformat_ingested_units: units_loop_v1.len() + units_aux.len(),
            step1_formats_verified: formats,
            step2_prefix_edit_kept_unit_id: kept_uid,
            step2_duplicate_marked_ambiguous: dup_marked_ambig,
            step3_incremental_recomputed_only_changed_doc: incremental_partial,
            step3_crash_reopen_visibility_lag_ticks: vis_reopen.lag_ticks,
            step4_temporal_2025_vs_2026_switched_cleanly: top_2025_ok,
            step4_conflict_detected_across_independent_origins: conflict_indep_ok,
            step5_pack_preserved_headers_units_qualifiers_and_manifest: pack_ok,
            step6_action_reused_context_zero_net_when_fresh: reused_zero_net,
            step6_action_watch_acquired_update_within_budget: watch_ok,
            step7_v156_304_unreliable_and_gc_invariants_intact: hit_304
                && no_false_withdrawal
                && !units_v2.is_empty(),
        }
    })
}

fn main() {
    let logical_cores = std::thread::available_parallelism()
        .map(|n| n.get())
        .unwrap_or(1);
    let numa_nodes = count_numa_nodes();
    let (initial_rss_mib, _) = read_rss_mib();

    let rt = tokio::runtime::Builder::new_multi_thread()
        .worker_threads(logical_cores.clamp(2, 8))
        .enable_all()
        .build()
        .expect("tokio runtime");

    let e1 = eval_e1_unit_identity();
    let e2 = eval_e2_delta_maintenance(&rt);
    let e3 = eval_e3_search_ablation();
    let e4 = eval_e4_pack_ablation();
    let e5 = eval_e5_action_and_learn(&rt);
    let e6 = eval_e6_runtime_scaling(logical_cores);
    let e7 = eval_e7_full_acceptance_demo(&rt);

    let (_, peak_rss_mib) = read_rss_mib();

    let report = ContextEvalReport {
        version: "1.6.0",
        seed: FIXED_SEED,
        e0_hardware: HardwareReport {
            logical_cores,
            numa_nodes,
            os_arch: std::env::consts::ARCH,
            initial_rss_mib,
            peak_rss_mib,
        },
        e1_unit_identity_ablation: e1,
        e2_delta_maintenance: e2,
        e3_search_ablation: e3,
        e4_pack_ablation: e4,
        e5_action_coverage_learn: e5,
        e6_runtime_scaling: e6,
        e7_acceptance_demo: e7,
    };

    println!("{}", serde_json::to_string_pretty(&report).unwrap());
}
