use std::collections::BTreeSet;

use super::*;
use crate::graph::{VersionBundleRequest, VersionCommitRequest};
use crate::sync::ResourceLifecycleState;
use pagesift_graph::{ObservationOutcome, VersionRecord};

#[test]
fn p0_01_commit_tick_atomic_partial_write_crash_recovers_all_or_nothing() {
    let dir = tempfile::tempdir().unwrap();
    {
        let store = Store::open_dir(dir.path()).unwrap();
        let ss = store.sync();
        let gs = store.graph();
        let spec = ss
            .upsert_source(spec_at(
                "crash-src",
                "https://crash.example.com/",
                1000,
                1000,
            ))
            .unwrap();
        let url = "https://crash.example.com/item";
        let key = pagesift_graph::normalize_resource_key(url);

        // Commit tick 1 cleanly.
        let c1 = sample_claim(url, &key, "doc-1", "10.00", 1500);
        let v1 = VersionRecord {
            resource_key: key.clone(),
            doc_id: "doc-1".into(),
            url: url.into(),
            canonical_url: None,
            observed_at_ms: 1500,
            content_hash: Some("c1".into()),
            text_hash: Some("t1".into()),
            status_code: 200,
            etag: None,
            last_modified: None,
        };
        let prep1 = gs
            .prepare_version_bundle(&VersionBundleRequest {
                version: Some(VersionCommitRequest {
                    expected_prev_doc_id: None,
                    alias_keys: &[],
                    record: &v1,
                    edge: None,
                    snapshot_bytes: Some(b"v1"),
                    snapshot_unavailable_reason: None,
                }),
                url_or_key: &key,
                contract_id: "ctr:aba",
                observation_outcome: &ObservationOutcome::VerifiedContent,
                claims: &[c1],
                observed_at_ms: 1500,
                doc_id: Some("doc-1"),
                doc_node: None,
                provenance: &[],
            })
            .unwrap();
        let mut inv1 = vec![rec_at(
            &key,
            url,
            ResourceLifecycleState::Active,
            1000,
            1500,
        )];
        let mut d1 = delta_at("crash-src", &spec.instance_id, "t-1", 1500);
        assert_eq!(
            ss.commit_tick_atomic(TickCommitRequest {
                source_id: "crash-src",
                expected_instance_id: &spec.instance_id,
                expected_spec_revision: 1,
                spec_revision_used: 1,
                inventory: &mut inv1,
                experience_bytes: b"exp-1",
                delta: &mut d1,
                graph_bundles: vec![prep1],
                conflict_subjects: &BTreeSet::new(),
                conflicts: &[],
            })
            .unwrap(),
            TickCommitOutcome::Committed { tick_seq: 1 }
        );

        // Simulate OS crash mid-batch when writing tick 2 (partial `AtomicTx` without `TxCommit`).
        let c2 = sample_claim(url, &key, "doc-2", "20.00", 2500);
        let v2 = VersionRecord {
            resource_key: key.clone(),
            doc_id: "doc-2".into(),
            url: url.into(),
            canonical_url: None,
            observed_at_ms: 2500,
            content_hash: Some("c2".into()),
            text_hash: Some("t2".into()),
            status_code: 200,
            etag: None,
            last_modified: None,
        };
        let prep2 = gs
            .prepare_version_bundle(&VersionBundleRequest {
                version: Some(VersionCommitRequest {
                    expected_prev_doc_id: Some("doc-1"),
                    alias_keys: &[],
                    record: &v2,
                    edge: None,
                    snapshot_bytes: Some(b"v2-updated-payload"),
                    snapshot_unavailable_reason: None,
                }),
                url_or_key: &key,
                contract_id: "ctr:aba",
                observation_outcome: &ObservationOutcome::VerifiedContent,
                claims: &[c2],
                observed_at_ms: 2500,
                doc_id: Some("doc-2"),
                doc_node: None,
                provenance: &[],
            })
            .unwrap();
        let mut rec2 = rec_at(&key, url, ResourceLifecycleState::Active, 1000, 2500);
        rec2.latest_doc_id = Some("doc-2".into());
        let mut inv2 = vec![rec2];
        let mut d2 = delta_at("crash-src", &spec.instance_id, "t-2", 2500);

        store.set_crash_after_tx_bytes(Some(128));
        let err = ss
            .commit_tick_atomic(TickCommitRequest {
                source_id: "crash-src",
                expected_instance_id: &spec.instance_id,
                expected_spec_revision: 1,
                spec_revision_used: 1,
                inventory: &mut inv2,
                experience_bytes: b"exp-2",
                delta: &mut d2,
                graph_bundles: vec![prep2],
                conflict_subjects: &BTreeSet::new(),
                conflicts: &[],
            })
            .unwrap_err();
        assert!(matches!(err, Error::Io(_)));
    }

    // Reopen after crash: tick 1 must be 100% intact, zero partial tick 2 records visible.
    let reopened = Store::open_dir(dir.path()).unwrap();
    let rss = reopened.sync();
    let rgs = reopened.graph();
    let view = rss.build_state_view("crash-src", None).unwrap().unwrap();
    assert_eq!(view.total_ticks, 1);
    assert_eq!(view.last_tick_id.as_deref(), Some("t-1"));
    assert_eq!(view.resources.len(), 1);
    assert_eq!(view.resources[0].latest_doc_id.as_deref(), Some("doc-1"));
    assert_eq!(
        rss.get_experience_bytes("crash-src").unwrap().as_deref(),
        Some(b"exp-1".as_slice())
    );
    let url = "https://crash.example.com/item";
    assert_eq!(rgs.history_as_of(url, None, 16).unwrap().versions.len(), 1);
    assert_eq!(
        rgs.get_resource_claims(url).unwrap()[0].claim.raw_value,
        serde_json::json!("10.00")
    );
}
