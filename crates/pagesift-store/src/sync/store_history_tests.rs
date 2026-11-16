use super::*;

fn test_claim_at(
    url: &str,
    key: &str,
    doc_id: &str,
    price: &str,
    ts: u64,
) -> pagesift_graph::ClaimRecord {
    use pagesift_graph::{
        ClaimRecord, ExtractionMethod, RawFieldTrace, SourceKind, align_traces,
        build_indexed_blocks,
    };
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
        "sku:t",
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
    .with_contract("ctr:t")
}

#[test]
fn t04_claim_lifecycle_multi_transition_as_of_and_truncation_blind_spot() {
    use crate::{VersionBundleRequest, VersionCommitRequest};
    use pagesift_graph::{
        ClaimLifecycleRecord, ClaimStatus, ObservationOutcome, VersionRecord, claims_as_of_checked,
    };

    let dir = tempfile::tempdir().unwrap();
    let store = Store::open_dir(dir.path()).unwrap();
    let ss = store.sync();
    let gs = store.graph();
    let spec_t04 = ss.upsert_source(base_spec("src-t04")).unwrap();

    let url = "https://docs.example.com/item";
    let key = pagesift_graph::normalize_resource_key(url);
    let mut prev_doc: Option<&str> = None;

    let steps: &[(u64, &str, Option<&str>, ObservationOutcome)] = &[
        (
            1000,
            "doc-1",
            Some("10.00"),
            ObservationOutcome::VerifiedContent,
        ),
        (
            2000,
            "doc-1",
            Some("10.00"),
            ObservationOutcome::VerifiedContent,
        ),
        (
            3000,
            "doc-err",
            None,
            ObservationOutcome::TransientFetchError {
                error: "403 Forbidden".into(),
            },
        ),
        (
            4000,
            "doc-1",
            Some("10.00"),
            ObservationOutcome::VerifiedContent,
        ),
        (5000, "doc-5", None, ObservationOutcome::VerifiedContent),
        (
            6000,
            "doc-6",
            Some("12.00"),
            ObservationOutcome::VerifiedContent,
        ),
    ];

    for (idx, (ts, doc_id, price_opt, outcome)) in steps.iter().enumerate() {
        let claim_vec = price_opt
            .map(|p| vec![test_claim_at(url, &key, doc_id, p, *ts)])
            .unwrap_or_default();
        let ver_rec = VersionRecord {
            resource_key: key.clone(),
            doc_id: (*doc_id).into(),
            url: url.into(),
            canonical_url: None,
            observed_at_ms: *ts,
            content_hash: Some(format!("ch-{doc_id}")),
            text_hash: Some(format!("th-{doc_id}")),
            status_code: 200,
            etag: None,
            last_modified: None,
        };
        let include_ver =
            matches!(outcome, ObservationOutcome::VerifiedContent) && prev_doc != Some(*doc_id);
        let req = VersionBundleRequest {
            version: include_ver.then_some(VersionCommitRequest {
                expected_prev_doc_id: prev_doc,
                alias_keys: &[],
                record: &ver_rec,
                edge: None,
                snapshot_bytes: Some(b"ok"),
                snapshot_unavailable_reason: None,
            }),
            url_or_key: &key,
            contract_id: "ctr:t",
            observation_outcome: outcome,
            claims: &claim_vec,
            observed_at_ms: *ts,
            doc_id: Some(doc_id),
            doc_node: None,
            provenance: &[],
            citations: &[],
        };
        if include_ver {
            prev_doc = Some(*doc_id);
        }
        let prep = gs.prepare_version_bundle(&req).unwrap();
        let mut inv = vec![base_rec(&key, url, ResourceLifecycleState::Active, *ts)];
        let mut delta = SyncDelta {
            source_id: "src-t04".into(),
            instance_id: spec_t04.instance_id.clone(),
            tick_id: format!("t-{idx}"),
            tick_seq: 0,
            started_at_ms: *ts - 1,
            completed_at_ms: *ts,
            coverage_status: "complete_within_scope".into(),
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
            source_id: "src-t04",
            expected_instance_id: &spec_t04.instance_id,
            expected_spec_revision: 1,
            spec_revision_used: 1,
            inventory: &mut inv,
            experience_bytes: &[],
            delta: &mut delta,
            graph_bundles: vec![prep],
            conflict_subjects: &BTreeSet::new(),
            conflicts: &[],
        })
        .unwrap();
    }

    for (ts, want_active, want_unverified, want_withdrawn, want_val) in [
        (1000, true, false, false, "10.00"),
        (2000, true, false, false, "10.00"),
        (3000, false, true, false, "10.00"),
        (4000, true, false, false, "10.00"),
        (5000, false, false, true, "10.00"),
        (6000, true, false, false, "12.00"),
    ] {
        let res = gs.claims_for_resource_as_of_checked(url, Some(ts)).unwrap();
        assert!(!res.history_unavailable, "timeline covers ts={ts}");
        assert_eq!(res.claims.len(), 1);
        let st = &res.claims[0].status;
        assert_eq!(*st == ClaimStatus::Active, want_active, "ts={ts}: {st:?}");
        assert_eq!(
            matches!(st, ClaimStatus::Unverified { .. }),
            want_unverified,
            "ts={ts}: {st:?}"
        );
        assert_eq!(
            matches!(st, ClaimStatus::Withdrawn { .. }),
            want_withdrawn,
            "ts={ts}: {st:?}"
        );
        assert_eq!(res.claims[0].claim.raw_value, serde_json::json!(want_val));
    }

    let mut truncated =
        ClaimLifecycleRecord::new_active(test_claim_at(url, &key, "doc-late", "99.00", 10_000));
    truncated.history_truncated = true;
    truncated.first_observed_at_ms = 100;
    let (_, hist_unavail) = claims_as_of_checked(&[truncated], 500);
    assert!(
        hist_unavail,
        "query prior to truncated retained timeline must set history_unavailable=true"
    );
}

#[test]
fn t06_crash_fault_injection_exhaustive_across_steps_and_reopen() {
    use crate::{VersionBundleRequest, VersionCommitRequest};
    use pagesift_graph::{ObservationOutcome, VersionRecord};

    let url = "https://docs.example.com/item";
    let key = pagesift_graph::normalize_resource_key(url);

    for crash_step in 0..8usize {
        let dir = tempfile::tempdir().unwrap();
        {
            let store = Store::open_dir(dir.path()).unwrap();
            let ss = store.sync();
            let gs = store.graph();
            let spec_crash = ss.upsert_source(base_spec("src-crash")).unwrap();

            let commit_step =
                |step: u64, doc_id: &str, prev_doc: Option<&str>, price: &str, exp_rev: u64| {
                    let ts = 1000 + step * 100;
                    let ver = VersionRecord {
                        resource_key: key.clone(),
                        doc_id: doc_id.into(),
                        url: url.into(),
                        canonical_url: None,
                        observed_at_ms: ts,
                        content_hash: Some(format!("c-{doc_id}")),
                        text_hash: Some(format!("t-{doc_id}")),
                        status_code: 200,
                        etag: None,
                        last_modified: None,
                    };
                    let claim = vec![test_claim_at(url, &key, doc_id, price, ts)];
                    let prep = gs
                        .prepare_version_bundle(&VersionBundleRequest {
                            version: Some(VersionCommitRequest {
                                expected_prev_doc_id: prev_doc,
                                alias_keys: &[],
                                record: &ver,
                                edge: None,
                                snapshot_bytes: Some(b"ok"),
                                snapshot_unavailable_reason: None,
                            }),
                            url_or_key: &key,
                            contract_id: "ctr:t",
                            observation_outcome: &ObservationOutcome::VerifiedContent,
                            claims: &claim,
                            observed_at_ms: ts,
                            doc_id: Some(doc_id),
                            doc_node: None,
                            provenance: &[],
                            citations: &[],
                        })
                        .unwrap();
                    let mut rec = base_rec(&key, url, ResourceLifecycleState::Active, ts);
                    rec.latest_doc_id = Some(doc_id.into());
                    let mut inv = vec![rec];
                    let mut delta = SyncDelta {
                        source_id: "src-crash".into(),
                        instance_id: spec_crash.instance_id.clone(),
                        tick_id: format!("t-{step}"),
                        tick_seq: 0,
                        started_at_ms: ts - 10,
                        completed_at_ms: ts,
                        coverage_status: "complete_within_scope".into(),
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
                        source_id: "src-crash",
                        expected_instance_id: &spec_crash.instance_id,
                        expected_spec_revision: exp_rev,
                        spec_revision_used: exp_rev,
                        inventory: &mut inv,
                        experience_bytes: b"exp",
                        delta: &mut delta,
                        graph_bundles: vec![prep],
                        conflict_subjects: &BTreeSet::new(),
                        conflicts: &[],
                    })
                };

            commit_step(0, "doc-0", None, "10.00", 1).unwrap();
            store.set_fail_after_writes(Some(crash_step));
            let _ = commit_step(1, "doc-1", Some("doc-0"), "20.00", 1);
            let _ = ss.upsert_source(base_spec("src-crash"));
            let _ = commit_step(2, "doc-2", Some("doc-1"), "30.00", 2);
            store.set_fail_after_writes(None);
        }

        let reopened = Store::open_dir(dir.path()).expect("reopen after crash succeeds cleanly");
        let rss = reopened.sync();
        let rgs = reopened.graph();
        let inv = rss.get_inventory("src-crash").unwrap();
        let deltas = rss.list_deltas("src-crash", 16).unwrap();
        let view = rss.build_state_view("src-crash", None).unwrap().unwrap();
        let hist = rgs.history_as_of(url, None, 16).unwrap();
        let claims = rgs.get_resource_claims(url).unwrap();

        assert_eq!(deltas.len(), view.total_ticks as usize);
        assert_eq!(
            hist.versions.len(),
            deltas.len(),
            "no partial version vs delta on crash_step={crash_step}"
        );
        assert_eq!(inv.len(), 1);
        assert_eq!(
            inv[0].latest_doc_id.as_deref(),
            hist.versions.last().map(|v| v.doc_id.as_str())
        );
        let want_price = match inv[0].latest_doc_id.as_deref() {
            Some("doc-0") => "10.00",
            Some("doc-1") => "20.00",
            Some("doc-2") => "30.00",
            other => panic!("unexpected doc_id after crash_step={crash_step}: {other:?}"),
        };
        assert_eq!(claims.len(), 1);
        assert_eq!(claims[0].claim.raw_value, serde_json::json!(want_price));
    }
}
