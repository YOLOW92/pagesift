use std::collections::BTreeSet;

use super::*;
use crate::graph::{VersionBundleRequest, VersionCommitRequest};
use crate::sync::{
    FreshnessPolicy, ResourceLifecycleState, ResourceRole, SyncBudget, SyncTickStats,
};
use pagesift_graph::{
    ClaimRecord, ExtractionMethod, ObservationOutcome, RawFieldTrace, SourceKind, VersionRecord,
    align_traces, build_indexed_blocks,
};

fn spec_at(id: &str, seed: &str, created_at_ms: u64, updated_at_ms: u64) -> DataSourceSpec {
    DataSourceSpec {
        id: id.to_string(),
        instance_id: String::new(),
        name: Some(id.to_string()),
        seeds: vec![seed.to_string()],
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
        created_at_ms,
        updated_at_ms,
    }
}

fn rec_at(
    key: &str,
    url: &str,
    state: ResourceLifecycleState,
    discovered_at_ms: u64,
    checked_at_ms: u64,
) -> ResourceRecord {
    ResourceRecord {
        resource_key: key.to_string(),
        url: url.to_string(),
        canonical_url: None,
        migrated_to: None,
        parent_url: None,
        depth: 0,
        role: ResourceRole::Seed,
        state,
        discovered_at_ms,
        last_checked_at_ms: Some(checked_at_ms),
        last_substantive_change_at_ms: Some(checked_at_ms),
        next_due_at_ms: checked_at_ms + 3600,
        etag: Some("\"v1\"".to_string()),
        last_modified: None,
        content_hash: Some("h1".to_string()),
        latest_doc_id: Some("doc-1".to_string()),
        last_status_code: Some(200),
        consecutive_failures: 0,
        consecutive_404s: 0,
        importance_weight: 1.0,
        last_strategy: Some("lightweight_http".to_string()),
        title: Some("Doc".to_string()),
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

fn delta_at(source_id: &str, instance_id: &str, tick_id: &str, ts: u64) -> SyncDelta {
    SyncDelta {
        source_id: source_id.to_string(),
        instance_id: instance_id.to_string(),
        tick_id: tick_id.to_string(),
        tick_seq: 0,
        started_at_ms: ts.saturating_sub(10),
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
    }
}

fn sample_claim(url: &str, key: &str, doc_id: &str, price: &str, ts: u64) -> ClaimRecord {
    let trace = RawFieldTrace {
        field_path: "price".into(),
        field_name: "price".into(),
        value: serde_json::json!(price),
        raw_quote: price.into(),
        attr_name: None,
        method: ExtractionMethod::JsonLd {
            entity_type: Some("Product".into()),
            entity_id: None,
        },
        source_url: Some(url.into()),
        visible_in_dom: true,
    };
    let md = format!("# Item\n\nPrice: {price}");
    let blocks = build_indexed_blocks(doc_id, &md, &[], 64, 512);
    let ev = align_traces(doc_id, url, std::slice::from_ref(&trace), &blocks)
        .into_iter()
        .next()
        .unwrap();
    ClaimRecord::from_trace(
        "sku:aba",
        key,
        doc_id,
        url,
        SourceKind::Html,
        "http",
        ts,
        None,
        &trace,
        ev,
    )
    .with_contract("ctr:aba")
}

#[test]
fn p0_02_delete_recreate_same_id_prevents_aba_and_persists_across_reopen() {
    let dir = tempfile::tempdir().unwrap();
    let spec_a = {
        let store = Store::open_dir(dir.path()).unwrap();
        let ss = store.sync();
        let a = ss
            .upsert_source(spec_at("docs", "https://a.example.com/", 1000, 1000))
            .unwrap();
        assert!(!a.instance_id.is_empty());
        assert_eq!(a.revision, 1);
        assert!(ss.delete_source("docs").unwrap());
        a
    };

    // Reopen store: `sgen:docs` must still yield a distinct monotonic `instance_id` for instance B.
    let store = Store::open_dir(dir.path()).unwrap();
    let ss = store.sync();
    let gs = store.graph();

    let spec_b = ss
        .upsert_source(spec_at("docs", "https://b.example.com/", 2000, 2000))
        .unwrap();
    assert_eq!(spec_b.revision, 1);
    assert_ne!(spec_a.instance_id, spec_b.instance_id);

    // Old instance A tries to commit a tick with matching `revision = 1` + graph bundle + claim.
    let url = "https://a.example.com/p";
    let key = pagesift_graph::normalize_resource_key(url);
    let claim = sample_claim(url, &key, "doc-a", "99.00", 1500);
    let ver = VersionRecord {
        resource_key: key.clone(),
        doc_id: "doc-a".into(),
        url: url.into(),
        canonical_url: None,
        observed_at_ms: 1500,
        content_hash: Some("ha".into()),
        text_hash: Some("ta".into()),
        status_code: 200,
        etag: None,
        last_modified: None,
    };
    let prep = gs
        .prepare_version_bundle(&VersionBundleRequest {
            version: Some(VersionCommitRequest {
                expected_prev_doc_id: None,
                alias_keys: &[],
                record: &ver,
                edge: None,
                snapshot_bytes: Some(b"stale-a"),
                snapshot_unavailable_reason: None,
            }),
            url_or_key: &key,
            contract_id: "ctr:aba",
            observation_outcome: &ObservationOutcome::VerifiedContent,
            claims: std::slice::from_ref(&claim),
            observed_at_ms: 1500,
            doc_id: Some("doc-a"),
            doc_node: None,
            provenance: &[],
            citations: &[],
        })
        .unwrap();
    let mut inv_a = vec![rec_at(
        &key,
        url,
        ResourceLifecycleState::Active,
        1000,
        1500,
    )];
    let mut delta_a = delta_at("docs", &spec_a.instance_id, "t-stale-a", 1500);
    let outcome = ss
        .commit_tick_atomic(TickCommitRequest {
            source_id: "docs",
            expected_instance_id: &spec_a.instance_id,
            expected_spec_revision: 1,
            spec_revision_used: 1,
            inventory: &mut inv_a,
            experience_bytes: b"poison-exp",
            delta: &mut delta_a,
            graph_bundles: vec![prep],
            conflict_subjects: &BTreeSet::new(),
            conflicts: &[],
        })
        .unwrap();
    assert_eq!(outcome, TickCommitOutcome::SourceDeleted);

    // Instance B must be completely untouched across all namespaces.
    assert!(ss.get_inventory("docs").unwrap().is_empty());
    assert!(ss.get_experience_bytes("docs").unwrap().is_none());
    assert!(ss.list_deltas("docs", 16).unwrap().is_empty());
    assert!(gs.get_resource_claims(url).unwrap().is_empty());
    assert!(gs.history_as_of(url, None, 16).unwrap().versions.is_empty());

    // Old instance A CAS update and stale `append_delta` must both be rejected.
    let cas_res = ss.update_source_cas(spec_a.clone(), Some(1)).unwrap();
    assert_eq!(cas_res, SpecCasOutcome::NotFound);
    assert_eq!(
        ss.get_source("docs").unwrap().unwrap().seeds,
        vec!["https://b.example.com/".to_string()]
    );
    assert!(ss.append_delta(&delta_a).is_err());

    // Valid commit for instance B succeeds cleanly.
    let mut inv_b = vec![rec_at(
        "rk-b",
        "https://b.example.com/",
        ResourceLifecycleState::Active,
        2000,
        2100,
    )];
    let mut delta_b = delta_at("docs", &spec_b.instance_id, "t-b-1", 2100);
    let ok = ss
        .commit_tick_atomic(TickCommitRequest {
            source_id: "docs",
            expected_instance_id: &spec_b.instance_id,
            expected_spec_revision: 1,
            spec_revision_used: 1,
            inventory: &mut inv_b,
            experience_bytes: b"b-exp",
            delta: &mut delta_b,
            graph_bundles: vec![],
            conflict_subjects: &BTreeSet::new(),
            conflicts: &[],
        })
        .unwrap();
    assert_eq!(ok, TickCommitOutcome::Committed { tick_seq: 1 });
}

#[test]
fn p1_06_pre_creation_and_missing_revision_never_leak_future_spec() {
    let dir = tempfile::tempdir().unwrap();
    let store = Store::open_dir(dir.path()).unwrap();
    let ss = store.sync();

    ss.upsert_source(spec_at("docs", "https://a.example.com/", 1000, 1000))
        .unwrap();
    assert!(ss.delete_source("docs").unwrap());
    let b1 = ss
        .upsert_source(spec_at("docs", "https://b.example.com/", 2000, 2000))
        .unwrap();

    // Timestamp between deleted A (1000) and newly created B (2000) must report NotCreatedYet.
    let before_b = ss.build_state_view("docs", Some(1500)).unwrap().unwrap();
    assert_eq!(before_b.history_status, SourceHistoryStatus::NotCreatedYet);
    assert!(!before_b.history_unavailable);
    assert!(before_b.spec.is_none());
    assert!(before_b.resources.is_empty());

    // Update B through revisions 2..=11, then delete `srev:` for revision 1 to simulate
    // missing early revision history after modification: querying t=2000 must NOT leak rev 11.
    let mut cur = b1;
    for r in 2..=11u64 {
        let mut next = cur.clone();
        next.include_paths = vec![format!("/v{r}")];
        next.updated_at_ms = 2000 + r * 10;
        cur = match ss.update_source_cas(next, Some(cur.revision)).unwrap() {
            SpecCasOutcome::Updated(s) => s,
            other => panic!("unexpected cas: {other:?}"),
        };
    }
    let rev10_view = ss.build_state_view("docs", Some(2105)).unwrap().unwrap();
    assert_eq!(rev10_view.history_status, SourceHistoryStatus::Complete);
    assert_eq!(rev10_view.spec.as_ref().unwrap().revision, 10);

    // Legacy store without `srev:` entries, where `current_spec.updated_at_ms > cutoff`.
    let leg_spec = spec_at("legacy-edited", "https://leg.example.com/", 1000, 3000);
    let leg_bytes = serde_json::to_vec(&leg_spec).unwrap();
    store
        .put(ns::SYNC_STATE, b"src:legacy-edited", &leg_bytes, 3000)
        .unwrap();
    let leg_past = ss
        .build_state_view("legacy-edited", Some(1500))
        .unwrap()
        .unwrap();
    assert_eq!(
        leg_past.history_status,
        SourceHistoryStatus::HistoryUnavailable
    );
    assert!(leg_past.history_unavailable);
    assert!(leg_past.spec.is_none());
}

#[test]
fn p1_07_legacy_no_checkpoint_evicted_checkpoints_and_corrupt_json_propagation() {
    let dir = tempfile::tempdir().unwrap();
    let store = Store::open_dir(dir.path()).unwrap();
    let ss = store.sync();

    // 1. Legacy 1.5.0 style (`put_inventory` + `append_delta`, no `sck:` checkpoints).
    let leg = ss
        .upsert_source(spec_at("leg", "https://leg.example.com/", 1000, 1000))
        .unwrap();
    let rec = rec_at(
        "rk-leg",
        "https://leg.example.com/",
        ResourceLifecycleState::Active,
        1000,
        2000,
    );
    ss.put_inventory("leg", &[rec], 2000).unwrap();
    ss.append_delta(&delta_at("leg", &leg.instance_id, "t-leg", 2000))
        .unwrap();

    let past_leg = ss.build_state_view("leg", Some(1500)).unwrap().unwrap();
    assert_eq!(
        past_leg.history_status,
        SourceHistoryStatus::HistoryUnavailable
    );
    assert!(past_leg.history_unavailable);
    assert!(past_leg.resources.is_empty());

    // 2. Pruned checkpoints (> 256 ticks): querying evicted early tick => HistoryUnavailable,
    // while querying before `first_tick_at_ms` => Complete (0 ticks) and retained tick => Complete.
    let pruned = ss
        .upsert_source(spec_at("pruned", "https://pruned.example.com/", 500, 500))
        .unwrap();
    for i in 1..=260u64 {
        let ts = 1000 + i * 10;
        let mut inv = vec![rec_at(
            "rk-p",
            "https://pruned.example.com/",
            ResourceLifecycleState::Active,
            500,
            ts,
        )];
        let mut d = delta_at("pruned", &pruned.instance_id, &format!("tp-{i}"), ts);
        ss.commit_tick_atomic(TickCommitRequest {
            source_id: "pruned",
            expected_instance_id: &pruned.instance_id,
            expected_spec_revision: 1,
            spec_revision_used: 1,
            inventory: &mut inv,
            experience_bytes: b"{}",
            delta: &mut d,
            graph_bundles: vec![],
            conflict_subjects: &BTreeSet::new(),
            conflicts: &[],
        })
        .unwrap();
    }
    let pre_first = ss.build_state_view("pruned", Some(800)).unwrap().unwrap();
    assert_eq!(pre_first.history_status, SourceHistoryStatus::Complete);
    assert!(!pre_first.history_unavailable);
    assert_eq!(pre_first.total_ticks, 0);

    let evicted = ss.build_state_view("pruned", Some(1020)).unwrap().unwrap();
    assert_eq!(
        evicted.history_status,
        SourceHistoryStatus::HistoryUnavailable
    );
    assert!(evicted.history_unavailable);

    let retained = ss.build_state_view("pruned", Some(3550)).unwrap().unwrap();
    assert_eq!(retained.history_status, SourceHistoryStatus::Complete);
    assert!(!retained.history_unavailable);

    // 3. Corrupt JSON in `sck:` must return `Err(Error::Corrupt)` instead of being swallowed.
    store
        .put(
            ns::SYNC_STATE,
            b"sck:pruned:00000000000000000999:00000000000000000001",
            b"{not-valid-json",
            999,
        )
        .unwrap();
    let corrupt_err = ss.build_state_view("pruned", Some(3550)).unwrap_err();
    assert!(matches!(corrupt_err, Error::Corrupt(_)));
}

#[path = "store_tick_crash_tests.rs"]
mod store_tick_crash_tests;
