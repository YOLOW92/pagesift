//! Unit tests for `StreamingGraphBuilder`, temporal semantics, source independence, and claim lifecycles.

use serde_json::json;

use crate::builder::StreamingGraphBuilder;
use crate::corroborate::{ClaimRecord, resolve_subject_key};
use crate::delta::DocumentInput;
use crate::evidence::{EvidenceAlignment, EvidenceSpan, ExtractionMethod, RawFieldTrace};
use crate::lifecycle::{
    ClaimStatus, ClaimTransitionKind, ObservationOutcome, WithdrawalReason, claims_as_of,
    reconcile_resource_claims,
};
use crate::model::{DocNode, GraphOptions, SourceKind};
use crate::provenance::{ProvenanceEdge, RedirectKind};

#[test]
fn streams_multi_source_corporate_graph_end_to_end() {
    let mut builder = StreamingGraphBuilder::new(GraphOptions {
        subject_keys: vec!["sku".into()],
        ..GraphOptions::enabled()
    });
    let html_delta = builder.ingest(DocumentInput {
        requested_url: "https://shop.example.com/pro".into(),
        final_url: "https://example.com/pro".into(),
        canonical_url: Some("https://example.com/pro".into()),
        canonical_from_header: false,
        source_kind: SourceKind::Html,
        observed_at_ms: 1_000,
        status_code: 200,
        engine: "http".into(),
        content_hash: Some("h1".into()),
        raw_hash: None,
        text_hash: None,
        etag: None,
        last_modified: Some("Wed, 15 Jan 2026 00:00:00 GMT".into()),
        title: Some("Pro Plan".into()),
        markdown: "# Pro Plan\n\nPro tier costs $49/mo. See [Enterprise PDF](/docs/pro.pdf).\n"
            .into(),
        redirect_chain: vec![(
            "https://shop.example.com/pro".into(),
            "https://example.com/pro".into(),
            301,
            RedirectKind::HttpPermanent,
        )],
        links: Vec::new(),
        alternates: Vec::new(),
        duplicate_of: None,
        attachments: vec![("https://example.com/docs/pro.pdf".into(), None, None)],
        structured_json_ld: vec![json!({
            "@type": "Product",
            "sku": "PRO-1",
            "price": "$49/mo"
        })],
        extracted_fields: None,
        raw_traces: Vec::new(),
        chunk_spans: vec![(0, 0, 72)],
        previous_version: None,
        version_diff_stats: None,
        changed_block_ids: Vec::new(),
    });
    assert_eq!(html_delta.evidence[0].confidence, 0.95);
    assert!(html_delta.evidence[0].verified);
    assert!(html_delta.temporal.is_some());
    assert!(
        html_delta
            .temporal
            .as_ref()
            .unwrap()
            .modified_at_ms
            .is_some()
    );

    builder.ingest(DocumentInput {
        requested_url: "https://example.com/docs/pro.pdf".into(),
        final_url: "https://example.com/docs/pro.pdf".into(),
        canonical_url: None,
        canonical_from_header: false,
        source_kind: SourceKind::Pdf,
        observed_at_ms: 2_000,
        status_code: 200,
        engine: "upload".into(),
        content_hash: Some("h2".into()),
        raw_hash: None,
        text_hash: None,
        etag: None,
        last_modified: None,
        title: Some("Pro Spec PDF".into()),
        markdown: "Pro plan (SKU PRO-1) is billed at $59/mo as of 2026-11.\n".into(),
        redirect_chain: Vec::new(),
        links: Vec::new(),
        alternates: Vec::new(),
        duplicate_of: None,
        attachments: Vec::new(),
        structured_json_ld: Vec::new(),
        extracted_fields: Some(json!({"sku": "PRO-1", "price": "$59/mo"})),
        raw_traces: Vec::new(),
        chunk_spans: Vec::new(),
        previous_version: None,
        version_diff_stats: None,
        changed_block_ids: Vec::new(),
    });

    let summary = builder.finalize();
    assert_eq!(summary.stats.nodes, 2);
    assert!(summary.citations.iter().any(|c| c.crawled));
    assert_eq!(summary.stats.conflicts, 1);
    assert_eq!(summary.conflicts[0].variants.len(), 2);
    let sku_cluster = summary
        .fact_clusters
        .iter()
        .find(|fc| fc.normalized_field == "sku")
        .expect("sku fact cluster");
    assert_eq!(sku_cluster.distinct_sources, 2);
    assert_eq!(sku_cluster.independent_sources, 1);
}

fn sample_doc(url: &str, can: Option<&str>, ts: u64, price: &str) -> DocumentInput {
    DocumentInput {
        requested_url: url.into(),
        final_url: url.into(),
        canonical_url: can.map(str::to_owned),
        canonical_from_header: false,
        source_kind: SourceKind::Html,
        observed_at_ms: ts,
        status_code: 200,
        engine: "http".into(),
        content_hash: Some(format!("h_{ts}_{price}")),
        raw_hash: None,
        text_hash: None,
        etag: None,
        last_modified: None,
        title: Some("Sample".into()),
        markdown: format!("# Sample\n\nPrice is {price}.\n"),
        redirect_chain: Vec::new(),
        links: Vec::new(),
        alternates: Vec::new(),
        duplicate_of: None,
        attachments: Vec::new(),
        structured_json_ld: vec![json!({"@type": "Product", "sku": "SKU-9", "price": price})],
        extracted_fields: None,
        raw_traces: Vec::new(),
        chunk_spans: Vec::new(),
        previous_version: None,
        version_diff_stats: None,
        changed_block_ids: Vec::new(),
    }
}

#[test]
fn cross_host_canonical_does_not_hijack_identity_or_subject_key_but_keeps_provenance() {
    let mut builder = StreamingGraphBuilder::new(GraphOptions::enabled());
    let evil = builder.ingest(sample_doc(
        "https://attacker.example/fake",
        Some("https://victim.example/pricing"),
        100,
        "$1/mo",
    ));
    assert_eq!(evil.node.resource_key, "https://attacker.example/fake");
    assert!(evil.provenance.iter().any(|p| matches!(
        p,
        ProvenanceEdge::CanonicalizesTo { cross_host: true, canonical_url, .. }
        if canonical_url == "https://victim.example/pricing"
    )));
    assert_eq!(
        DocNode::derive_resource_key(
            Some("https://victim.example/pricing"),
            "https://attacker.example/fake"
        ),
        "https://attacker.example/fake"
    );
    let sk = resolve_subject_key(
        &[],
        &[],
        &[],
        Some("https://victim.example/pricing"),
        "https://attacker.example/fake",
    );
    assert_eq!(sk.key, "resource:https://attacker.example/fake");
}

#[test]
fn facts_and_conflicts_still_work_when_evidence_is_disabled() {
    let opts = GraphOptions {
        enabled: true,
        evidence: false,
        facts: true,
        conflicts: true,
        ..GraphOptions::enabled()
    };
    let mut builder = StreamingGraphBuilder::new(opts);
    let d1 = builder.ingest(sample_doc(
        "https://example.com/a",
        Some("https://example.com/a"),
        100,
        "$49/mo",
    ));
    let d2 = builder.ingest(sample_doc("https://example.com/b", None, 200, "$79/mo"));
    assert!(d1.evidence.is_empty());
    assert!(d2.evidence.is_empty());
    assert!(!d1.claims.is_empty());
    assert!(!d2.claims.is_empty());

    let summary = builder.into_summary();
    assert_eq!(summary.stats.evidence_spans, 0);
    assert!(summary.stats.claims >= 2);
    assert_eq!(summary.stats.conflicts, 1);
}

#[test]
fn task_level_budgets_mark_truncation_and_into_summary_matches_finalize() {
    let mut opts = GraphOptions::enabled();
    opts.limits.max_nodes = 1;
    opts.limits.max_claims = 2;
    let mut builder = StreamingGraphBuilder::new(opts);
    builder.ingest(sample_doc("https://example.com/1", None, 100, "$10"));
    builder.ingest(sample_doc("https://example.com/2", None, 200, "$20"));
    let fin = builder.finalize();
    let consumed = builder.into_summary();
    assert_eq!(fin, consumed);
    assert_eq!(consumed.stats.nodes, 1);
    assert!(consumed.stats.truncated);
    assert!(
        consumed
            .stats
            .truncation_reasons
            .iter()
            .any(|r| r == "max_nodes")
    );
}

#[test]
fn multi_version_same_resource_emits_claim_transitions_without_false_self_conflict() {
    let mut builder = StreamingGraphBuilder::new(GraphOptions::enabled());
    let d1 = builder.ingest(sample_doc(
        "https://example.com/pricing",
        None,
        100,
        "$49/mo",
    ));
    assert!(d1.claim_transitions.is_empty());

    let d2 = builder.ingest(sample_doc(
        "https://example.com/pricing",
        None,
        200,
        "$59/mo",
    ));
    assert!(
        d2.claim_transitions
            .iter()
            .any(|t| t.kind == ClaimTransitionKind::Updated && t.normalized_field == "price")
    );

    let summary = builder.into_summary();
    // Single resource updating its own price from $49/mo to $59/mo across versions is NOT a conflict!
    assert_eq!(summary.stats.conflicts, 0);
    assert!(!summary.claim_transitions.is_empty());
}

#[test]
fn distinguishes_update_transient_error_omission_and_as_of_query() {
    let mk_claim = |doc_id: &str, ts: u64, val: &str| {
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
    };

    let c1 = mk_claim("d1", 1000, "$99/mo");
    let (recs1, trans1) =
        reconcile_resource_claims(&[], &ObservationOutcome::VerifiedContent, &[c1], 1000);
    assert_eq!(trans1.len(), 1);
    assert_eq!(trans1[0].kind, ClaimTransitionKind::Appeared);

    let c2 = mk_claim("d2", 2000, "$89/mo");
    let (recs2, trans2) =
        reconcile_resource_claims(&recs1, &ObservationOutcome::VerifiedContent, &[c2], 2000);
    assert_eq!(trans2[0].kind, ClaimTransitionKind::Updated);
    assert_eq!(recs2[0].history.len(), 1);

    let (recs3, trans3) = reconcile_resource_claims(
        &recs2,
        &ObservationOutcome::TransientFetchError {
            error: "HTTP 503".into(),
        },
        &[],
        3000,
    );
    assert_eq!(trans3[0].kind, ClaimTransitionKind::Unverified);
    assert!(matches!(recs3[0].status, ClaimStatus::Unverified { .. }));

    let (recs4, trans4) =
        reconcile_resource_claims(&recs3, &ObservationOutcome::VerifiedContent, &[], 4000);
    assert_eq!(trans4[0].kind, ClaimTransitionKind::Withdrawn);
    assert!(matches!(
        recs4[0].status,
        ClaimStatus::Withdrawn {
            reason: WithdrawalReason::OmittedInVerifiedVersion,
            ..
        }
    ));

    let at_1500 = claims_as_of(&recs4, 1500);
    assert_eq!(at_1500.len(), 1);
    assert_eq!(at_1500[0].claim.normalized_value.raw(), "$99/mo");
    assert_eq!(at_1500[0].status, ClaimStatus::Active);

    let at_2500 = claims_as_of(&recs4, 2500);
    assert_eq!(at_2500.len(), 1);
    assert_eq!(at_2500[0].claim.normalized_value.raw(), "$89/mo");
    assert_eq!(at_2500[0].status, ClaimStatus::Active);
}
