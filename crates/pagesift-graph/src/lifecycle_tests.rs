//! Point-in-time claim lifecycle regression tests (P1-04).

use serde_json::json;

use crate::corroborate::ClaimRecord;
use crate::evidence::{EvidenceAlignment, EvidenceSpan, ExtractionMethod, RawFieldTrace};
use crate::lifecycle::{
    ClaimLifecycleRecord, ClaimStatus, ClaimTimelineEntry, ObservationOutcome,
    claims_as_of_checked, reconcile_resource_claims,
};
use crate::model::SourceKind;

fn mk_claim(doc_id: &str, ts: u64, val: &str) -> ClaimRecord {
    let trace = RawFieldTrace {
        field_path: "/price".into(),
        field_name: "price".into(),
        value: json!(val),
        raw_quote: val.into(),
        attr_name: None,
        method: ExtractionMethod::CssSelector {
            selector: ".price".into(),
            relocated_from: None,
            step: None,
        },
        source_url: None,
        visible_in_dom: true,
    };
    let span = EvidenceSpan {
        doc_id: doc_id.into(),
        url: "https://example.com/pricing".into(),
        block_id: Some(format!("{doc_id}#b0")),
        chunk_idx: None,
        start: Some(0),
        end: Some(val.len()),
        excerpt: val.into(),
        confidence: 1.0,
        alignment: EvidenceAlignment::ExactBlock,
        verified: true,
        field_path: "/price".into(),
        method: trace.method.clone(),
    };
    ClaimRecord::from_trace(
        "sku:PRO",
        "https://example.com/pricing",
        doc_id,
        "https://example.com/pricing",
        SourceKind::Html,
        "http",
        ts,
        None,
        &trace,
        span,
    )
}

#[test]
fn point_in_time_does_not_leak_future_confirmations_or_truncate_timestamps() {
    let c10 = mk_claim("d1", 10_000, "$99");
    let (recs1, _) =
        reconcile_resource_claims(&[], &ObservationOutcome::VerifiedContent, &[c10], 10_000);
    let (recs2, _) =
        reconcile_resource_claims(&recs1, &ObservationOutcome::NotModified304, &[], 11_000);
    let (recs3, _) =
        reconcile_resource_claims(&recs2, &ObservationOutcome::NotModified304, &[], 12_000);

    // At 10:30, only the 10:00 observation existed.
    let (at_1030, unavail_1030) = claims_as_of_checked(&recs3, 10_500);
    assert!(!unavail_1030);
    assert_eq!(at_1030.len(), 1);
    assert_eq!(at_1030[0].observation_count, 1);
    assert_eq!(at_1030[0].last_confirmed_at_ms, 10_000);
    assert_eq!(at_1030[0].claim.observed_at_ms, 10_000);
    assert_eq!(at_1030[0].timeline.len(), 1);
    assert_eq!(at_1030[0].timeline[0].until_ms, 10_000);
    assert_eq!(at_1030[0].timeline[0].observation_count, 1);
    assert_eq!(at_1030[0].timeline[0].confirmations.len(), 1);

    // At 11:30, two confirmations had happened; last confirmation was 11:00.
    let (at_1130, unavail_1130) = claims_as_of_checked(&recs3, 11_500);
    assert!(!unavail_1130);
    assert_eq!(at_1130[0].observation_count, 2);
    assert_eq!(at_1130[0].last_confirmed_at_ms, 11_000);
    assert_eq!(at_1130[0].timeline[0].until_ms, 11_000);

    // Identical verified re-crawl with same doc_id updates claim.observed_at_ms cleanly too.
    let c13 = mk_claim("d1", 13_000, "$99");
    let (recs4, _) =
        reconcile_resource_claims(&recs3, &ObservationOutcome::VerifiedContent, &[c13], 13_000);
    let (at_1250, _) = claims_as_of_checked(&recs4, 12_500);
    assert_eq!(at_1250[0].observation_count, 3);
    assert_eq!(at_1250[0].last_confirmed_at_ms, 12_000);
    assert_eq!(at_1250[0].claim.observed_at_ms, 10_000);
    let (at_1350, _) = claims_as_of_checked(&recs4, 13_500);
    assert_eq!(at_1350[0].observation_count, 4);
    assert_eq!(at_1350[0].claim.observed_at_ms, 13_000);
}

#[test]
fn coalesce_preserves_evidence_provenance_and_status_reason_changes() {
    let mut c1 = mk_claim("d1", 1_000, "$99");
    let mut c2 = mk_claim("d2", 2_000, "$99");
    c2.evidence.excerpt = "$99.00 (updated block)".into();

    let (r1, _) = reconcile_resource_claims(
        &[],
        &ObservationOutcome::VerifiedContent,
        &[c1.clone()],
        1_000,
    );
    let (r2, _) =
        reconcile_resource_claims(&r1, &ObservationOutcome::VerifiedContent, &[c2], 2_000);
    // Changing doc_id/evidence must start a distinct timeline entry rather than coalescing.
    assert_eq!(r2[0].timeline.len(), 2);

    // Distinct Unverified failures must not merge across different reasons/since_ms.
    let (r3, _) = reconcile_resource_claims(
        &r2,
        &ObservationOutcome::TransientFetchError {
            error: "timeout".into(),
        },
        &[],
        3_000,
    );
    let (r4, _) = reconcile_resource_claims(
        &r3,
        &ObservationOutcome::TransientFetchError {
            error: "503 unavailable".into(),
        },
        &[],
        4_000,
    );
    assert_eq!(r4[0].timeline.len(), 4);

    let (at_1500, _) = claims_as_of_checked(&r4, 1_500);
    assert_eq!(at_1500[0].claim.doc_id, "d1");
    assert_eq!(at_1500[0].claim.evidence.excerpt, "$99");

    let (at_3500, _) = claims_as_of_checked(&r4, 3_500);
    assert_eq!(
        at_3500[0].status,
        ClaimStatus::Unverified {
            since_ms: 3_000,
            reason: "timeout".into()
        }
    );

    // Future updates/withdrawals must not leak into earlier `history`.
    c1 = mk_claim("d3", 5_000, "$79");
    let (r5, _) =
        reconcile_resource_claims(&r4, &ObservationOutcome::VerifiedContent, &[c1], 5_000);
    let (r6, _) = reconcile_resource_claims(
        &r5,
        &ObservationOutcome::ConfirmedGone {
            reason: "410".into(),
        },
        &[],
        6_000,
    );
    let (at_2500, _) = claims_as_of_checked(&r6, 2_500);
    assert!(at_2500[0].history.is_empty());
    let (at_5500, _) = claims_as_of_checked(&r6, 5_500);
    assert_eq!(at_5500[0].history.len(), 1);
    assert_eq!(at_5500[0].history[0].doc_id, "d2");
    let (at_6500, _) = claims_as_of_checked(&r6, 6_500);
    assert_eq!(at_6500[0].history.len(), 2);
}

#[test]
fn legacy_coalesced_and_timelineless_records_flag_history_unavailable_without_forging() {
    let c10 = mk_claim("d1", 10_000, "$99");
    let legacy_entry = ClaimTimelineEntry {
        since_ms: 10_000,
        until_ms: 12_000,
        status: ClaimStatus::Active,
        claim: c10.clone(),
        last_confirmed_at_ms: 12_000,
        observation_count: 3,
        confirmations: Vec::new(),
    };
    let legacy_rec = ClaimLifecycleRecord {
        lifecycle_id: "clc-legacy".into(),
        claim: ClaimRecord {
            observed_at_ms: 12_000,
            ..c10.clone()
        },
        status: ClaimStatus::Active,
        first_observed_at_ms: 10_000,
        last_confirmed_at_ms: 12_000,
        observation_count: 3,
        history: Vec::new(),
        timeline: vec![legacy_entry],
        history_truncated: false,
    };

    // Mid-interval query on legacy entry cannot know exact confirmations at 10:30:
    // must report `history_unavailable = true` and must neither forge 10_500 nor leak 12_000 / 3.
    let (mid, unavail_mid) = claims_as_of_checked(std::slice::from_ref(&legacy_rec), 10_500);
    assert!(unavail_mid);
    assert_ne!(mid[0].last_confirmed_at_ms, 10_500);
    assert_ne!(mid[0].last_confirmed_at_ms, 12_000);
    assert_ne!(mid[0].observation_count, 3);
    assert!(mid[0].timeline[0].until_ms <= 10_500);

    // After legacy interval ends, all intra-interval events already completed -> exact!
    let (after, unavail_after) = claims_as_of_checked(&[legacy_rec], 12_500);
    assert!(!unavail_after);
    assert_eq!(after[0].observation_count, 3);
    assert_eq!(after[0].last_confirmed_at_ms, 12_000);

    // 1.5.0 timelineless record with later NotModified304 confirmations:
    let v150 = ClaimLifecycleRecord {
        lifecycle_id: "clc-150".into(),
        claim: c10,
        status: ClaimStatus::Active,
        first_observed_at_ms: 10_000,
        last_confirmed_at_ms: 12_000,
        observation_count: 3,
        history: Vec::new(),
        timeline: Vec::new(),
        history_truncated: false,
    };
    let (v150_mid, v150_unavail) = claims_as_of_checked(&[v150], 10_500);
    assert!(v150_unavail);
    assert_ne!(v150_mid[0].last_confirmed_at_ms, 12_000);
    assert_ne!(v150_mid[0].observation_count, 3);
}

#[test]
fn many_confirmations_split_cleanly_while_preserving_exact_point_in_time_queries() {
    let c0 = mk_claim("d1", 1_000, "$99");
    let (mut recs, _) =
        reconcile_resource_claims(&[], &ObservationOutcome::VerifiedContent, &[c0], 1_000);
    for step in 2..=100u64 {
        (recs, _) = reconcile_resource_claims(
            &recs,
            &ObservationOutcome::NotModified304,
            &[],
            step * 1_000,
        );
    }
    assert!(recs[0].timeline.len() >= 2);
    assert!(!recs[0].history_truncated);
    for step in [1u64, 30, 64, 65, 80, 100] {
        let (res, unavail) = claims_as_of_checked(&recs, step * 1_000 + 500);
        assert!(!unavail, "step={step}");
        assert_eq!(res[0].observation_count, step as u32);
        assert_eq!(res[0].last_confirmed_at_ms, step * 1_000);
    }
}
