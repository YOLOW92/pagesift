use std::collections::BTreeSet;

use super::*;
use crate::sync::{
    FreshnessPolicy, ResourceLifecycleState, ResourceRole, SyncBudget, SyncTickStats,
};

fn base_spec(id: &str) -> DataSourceSpec {
    DataSourceSpec {
        id: id.to_string(),
        instance_id: String::new(),
        name: Some("Docs".to_string()),
        seeds: vec!["https://docs.example.com/".to_string()],
        include_paths: Vec::new(),
        exclude_paths: Vec::new(),
        max_depth: 2,
        allow_subdomains: false,
        freshness: FreshnessPolicy::default(),
        budget: SyncBudget::default(),
        extract_schema: None,
        extract_prompt: None,
        required_fields: Vec::new(),
        allow_browser: true,
        revision: 1,
        created_at_ms: 1000,
        updated_at_ms: 1000,
    }
}

fn base_rec(key: &str, url: &str, state: ResourceLifecycleState, ts: u64) -> ResourceRecord {
    ResourceRecord {
        resource_key: key.to_string(),
        url: url.to_string(),
        canonical_url: None,
        migrated_to: None,
        parent_url: None,
        depth: 0,
        role: ResourceRole::Seed,
        state,
        discovered_at_ms: 1000,
        last_checked_at_ms: Some(ts),
        last_attempted_at_ms: Some(ts),
        last_valid_observed_at_ms: Some(ts),
        last_substantive_change_at_ms: Some(ts),
        next_due_at_ms: ts + 3600,
        etag: Some("\"v1\"".to_string()),
        last_modified: None,
        last_modified_at_ms: None,
        last_modified_dynamic: false,
        content_hash: Some("h1".to_string()),
        latest_doc_id: Some("doc1".to_string()),
        last_status_code: Some(200),
        consecutive_failures: 0,
        consecutive_404s: 0,
        importance_weight: 1.0,
        last_strategy: Some("lightweight_http".to_string()),
        title: Some("Home".to_string()),
        claim_count: 1,
        last_validator_contract_id: None,
        last_authoritative_status: Some(200),
        baseline_sufficient: true,
        has_embedded_json: false,
        text_signature: None,
        confirmed_gone_at_ms: None,
        learn: crate::ResourceLearnStats::default(),
    }
}

#[test]
fn sync_store_persists_sources_inventory_deltas_and_as_of_view() {
    let dir = tempfile::tempdir().unwrap();
    let store = Store::open_dir(dir.path()).unwrap();
    let ss = store.sync();

    let spec = base_spec("docs-src");
    ss.put_source(&spec).unwrap();
    assert_eq!(ss.list_sources().unwrap().len(), 1);

    let rec = base_rec(
        "rk1",
        "https://docs.example.com/",
        ResourceLifecycleState::Active,
        1500,
    );
    ss.put_inventory("docs-src", &[rec], 1500).unwrap();

    let delta = SyncDelta {
        source_id: "docs-src".to_string(),
        instance_id: String::new(),
        tick_id: "tick-1500".to_string(),
        tick_seq: 1,
        started_at_ms: 1400,
        completed_at_ms: 1500,
        coverage_status: "complete_within_scope".to_string(),
        coverage_reasons: vec!["all_in_scope_resources_verified".to_string()],
        stats: SyncTickStats::default(),
        added_resources: Vec::new(),
        modified_resources: Vec::new(),
        migrated_resources: Vec::new(),
        disappeared_resources: Vec::new(),
        claim_transitions: Vec::new(),
        conflicts: Vec::new(),
        adapt_decisions: Vec::new(),
    };
    ss.append_delta(&delta).unwrap();
    assert_eq!(
        ss.get_delta("docs-src", "tick-1500")
            .unwrap()
            .unwrap()
            .tick_id,
        "tick-1500"
    );

    let view = ss.build_state_view("docs-src", None).unwrap().unwrap();
    assert_eq!(view.history_status, SourceHistoryStatus::Complete);
    assert!(!view.history_unavailable);
    assert_eq!(view.spec.as_ref().unwrap().id, "docs-src");
    assert_eq!(view.total_ticks, 1);
    assert_eq!(view.inventory_counts.get("active"), Some(&1));

    let early_view = ss.build_state_view("docs-src", Some(500)).unwrap().unwrap();
    assert_eq!(
        early_view.history_status,
        SourceHistoryStatus::NotCreatedYet
    );
    assert!(!early_view.history_unavailable);
    assert!(early_view.spec.is_none());
    assert_eq!(early_view.total_ticks, 0);
    assert!(early_view.resources.is_empty());

    assert!(ss.delete_source("docs-src").unwrap());
    assert!(ss.get_source("docs-src").unwrap().is_none());
}

#[test]
fn point_in_time_view_isolates_future_spec_edits_state_mutations_and_pruning_c03_c30() {
    let dir = tempfile::tempdir().unwrap();
    let store = Store::open_dir(dir.path()).unwrap();
    let ss = store.sync();

    let mut s1 = base_spec("src-c03");
    s1.include_paths = vec!["/v1".to_string()];
    ss.put_source(&s1).unwrap();
    let s1 = ss.get_source("src-c03").unwrap().unwrap();

    let mut inv1 = vec![base_rec(
        "rk1",
        "https://docs.example.com/v1/a",
        ResourceLifecycleState::Active,
        2000,
    )];
    let mut d1 = SyncDelta {
        source_id: "src-c03".to_string(),
        instance_id: s1.instance_id.clone(),
        tick_id: "t1".to_string(),
        tick_seq: 0,
        started_at_ms: 1900,
        completed_at_ms: 2000,
        coverage_status: "complete_within_scope".to_string(),
        coverage_reasons: vec![],
        stats: SyncTickStats::default(),
        added_resources: vec![],
        modified_resources: vec![],
        migrated_resources: vec![],
        disappeared_resources: vec![],
        claim_transitions: vec![],
        conflicts: vec![],
        adapt_decisions: vec![],
    };
    assert_eq!(
        ss.commit_tick_atomic(TickCommitRequest {
            source_id: "src-c03",
            expected_instance_id: &s1.instance_id,
            expected_spec_revision: 1,
            spec_revision_used: 1,
            inventory: &mut inv1,
            experience_bytes: b"{}",
            delta: &mut d1,
            graph_bundles: vec![],
            conflict_subjects: &BTreeSet::new(),
            conflicts: &[],
        })
        .unwrap(),
        TickCommitOutcome::Committed { tick_seq: 1 }
    );

    // 修订 spec 到 rev=2，然后运行 70 轮使状态变为 Migrated 且超过 64 条 delta 上限
    let mut s2 = s1.clone();
    s2.include_paths = vec!["/v2".to_string()];
    s2.updated_at_ms = 3000;
    let updated = match ss.update_source_cas(s2, Some(1)).unwrap() {
        SpecCasOutcome::Updated(s) => s,
        other => panic!("unexpected cas result: {other:?}"),
    };
    assert_eq!(updated.revision, 2);

    for i in 2..=70u64 {
        let ts = 3000 + i * 100;
        let mut rec = base_rec(
            "rk1",
            "https://docs.example.com/v1/a",
            ResourceLifecycleState::Migrated,
            ts,
        );
        rec.migrated_to = Some("https://docs.example.com/v2/a".to_string());
        let mut inv_i = vec![rec];
        let mut di = SyncDelta {
            source_id: "src-c03".to_string(),
            instance_id: updated.instance_id.clone(),
            tick_id: format!("t{i}"),
            tick_seq: 0,
            started_at_ms: ts - 10,
            completed_at_ms: ts,
            coverage_status: "complete_within_scope".to_string(),
            coverage_reasons: vec![],
            stats: SyncTickStats::default(),
            added_resources: vec![],
            modified_resources: vec![],
            migrated_resources: vec![],
            disappeared_resources: vec![],
            claim_transitions: vec![],
            conflicts: vec![],
            adapt_decisions: vec![],
        };
        ss.commit_tick_atomic(TickCommitRequest {
            source_id: "src-c03",
            expected_instance_id: &updated.instance_id,
            expected_spec_revision: 2,
            spec_revision_used: 2,
            inventory: &mut inv_i,
            experience_bytes: b"{}",
            delta: &mut di,
            graph_bundles: vec![],
            conflict_subjects: &BTreeSet::new(),
            conflicts: &[],
        })
        .unwrap();
    }

    // 回溯到 t=2500：应看到 rev=1、Active、无 migrated_to、total_ticks=1（不被后续 70 轮污染）
    let past = ss.build_state_view("src-c03", Some(2500)).unwrap().unwrap();
    let past_spec = past.spec.as_ref().unwrap();
    assert_eq!(past_spec.revision, 1);
    assert_eq!(past_spec.include_paths, vec!["/v1".to_string()]);
    assert_eq!(past.total_ticks, 1);
    assert_eq!(past.resources.len(), 1);
    assert_eq!(past.resources[0].state, ResourceLifecycleState::Active);
    assert_eq!(past.resources[0].migrated_to, None);

    // 当前视图 total_ticks 必须等于真实累计 70（不被 64 delta 裁剪截断，C30）
    let now_view = ss.build_state_view("src-c03", None).unwrap().unwrap();
    assert_eq!(now_view.total_ticks, 70);
    assert_eq!(now_view.retained_ticks, 64);
}

#[test]
fn atomic_tick_aborts_cleanly_on_fail_writes_and_stale_revision_or_delete_c05_c06_r02() {
    let dir = tempfile::tempdir().unwrap();
    let store = Store::open_dir(dir.path()).unwrap();
    let ss = store.sync();
    let atom_spec = ss.upsert_source(base_spec("src-atom")).unwrap();

    // 1. 注入 fail_writes：整轮应全回滚，零版本/声明/增量写入
    store.fail_writes(true);
    let mut inv = vec![base_rec(
        "rk1",
        "https://docs.example.com/",
        ResourceLifecycleState::Active,
        2000,
    )];
    let mut delta = SyncDelta {
        source_id: "src-atom".to_string(),
        instance_id: atom_spec.instance_id.clone(),
        tick_id: "t-fail".to_string(),
        tick_seq: 0,
        started_at_ms: 1900,
        completed_at_ms: 2000,
        coverage_status: "complete_within_scope".to_string(),
        coverage_reasons: vec![],
        stats: SyncTickStats::default(),
        added_resources: vec![],
        modified_resources: vec![],
        migrated_resources: vec![],
        disappeared_resources: vec![],
        claim_transitions: vec![],
        conflicts: vec![],
        adapt_decisions: vec![],
    };
    let err = ss.commit_tick_atomic(TickCommitRequest {
        source_id: "src-atom",
        expected_instance_id: &atom_spec.instance_id,
        expected_spec_revision: 1,
        spec_revision_used: 1,
        inventory: &mut inv,
        experience_bytes: b"exp-bad",
        delta: &mut delta,
        graph_bundles: vec![],
        conflict_subjects: &BTreeSet::new(),
        conflicts: &[],
    });
    assert!(err.is_err());
    store.fail_writes(false);
    assert!(ss.get_inventory("src-atom").unwrap().is_empty());
    assert!(ss.get_delta("src-atom", "t-fail").unwrap().is_none());

    // 2. CAS revision 不匹配：返回 StaleSpec 且不落盘
    let stale = ss
        .commit_tick_atomic(TickCommitRequest {
            source_id: "src-atom",
            expected_instance_id: &atom_spec.instance_id,
            expected_spec_revision: 99,
            spec_revision_used: 99,
            inventory: &mut inv,
            experience_bytes: b"exp",
            delta: &mut delta,
            graph_bundles: vec![],
            conflict_subjects: &BTreeSet::new(),
            conflicts: &[],
        })
        .unwrap();
    assert!(matches!(stale, TickCommitOutcome::StaleSpec { .. }));
    assert!(ss.get_inventory("src-atom").unwrap().is_empty());

    // 3. Source 删除后提交应返回 SourceDeleted 且不复活（R02）
    assert!(ss.delete_source("src-atom").unwrap());
    let deleted = ss
        .commit_tick_atomic(TickCommitRequest {
            source_id: "src-atom",
            expected_instance_id: &atom_spec.instance_id,
            expected_spec_revision: 1,
            spec_revision_used: 1,
            inventory: &mut inv,
            experience_bytes: b"exp",
            delta: &mut delta,
            graph_bundles: vec![],
            conflict_subjects: &BTreeSet::new(),
            conflicts: &[],
        })
        .unwrap();
    assert_eq!(deleted, TickCommitOutcome::SourceDeleted);
    assert!(ss.get_source("src-atom").unwrap().is_none());
}

#[test]
fn decodes_legacy_v155_resource_record_and_state_view_with_defaults() {
    let legacy_rec_json = serde_json::json!({
        "resourceKey": "https://ex.com/a",
        "url": "https://ex.com/a",
        "depth": 0,
        "role": "seed",
        "state": "active",
        "discoveredAtMs": 1000,
        "lastCheckedAtMs": 5000,
        "nextDueAtMs": 9000,
        "consecutiveFailures": 0,
        "consecutive404s": 0,
        "importanceWeight": 1.0,
        "claimCount": 2
    });
    let rec: ResourceRecord = serde_json::from_value(legacy_rec_json).expect("decode legacy rec");
    assert_eq!(rec.valid_observation_anchor_ms(), Some(5000));
    assert!(!rec.last_modified_dynamic);

    let mut transient_legacy = rec.clone();
    transient_legacy.state = ResourceLifecycleState::TransientFailure;
    assert_eq!(transient_legacy.valid_observation_anchor_ms(), None);

    let legacy_view_json = serde_json::json!({
        "sourceId": "s-legacy",
        "totalTicks": 1,
        "inventoryCounts": {},
        "resources": [],
        "activeClaims": [],
        "conflicts": [],
        "factClusters": [{
            "clusterId": "fct-1",
            "subjectKey": "sku:1",
            "normalizedField": "price",
            "value": { "kind": "money", "amount_e4": 100000, "currency": "USD", "period": "unknown", "raw": "$10" },
            "supportCount": 1,
            "distinctSources": 1,
            "independentSources": 1,
            "distinctHosts": 1,
            "distinctFormats": ["html"],
            "claimIds": ["c1"],
            "evidence": []
        }]
    });
    let view: SourceStateView =
        serde_json::from_value(legacy_view_json).expect("decode legacy state view");
    assert!(!view.fact_clusters_truncated);
    assert_eq!(view.fact_clusters[0].unverified_support_count, 0);
}

#[path = "store_history_tests.rs"]
mod history_tests;
