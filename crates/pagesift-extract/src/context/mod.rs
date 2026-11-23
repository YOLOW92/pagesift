//! 1.6 Context algorithms: unified unit scanning, bounded-UOT cross-version identity matching,
//! dependency-tracked multi-document search, constrained context packing, and drift-guarded calibration.

pub mod calib;
pub mod dpp;
mod index_maint;
pub mod pack;
mod pack_select;
pub mod search;
pub(crate) mod temporal_eval;
pub mod tokenize_unified;
pub(crate) mod unit_cost;
pub mod unit_match;
pub mod unit_scan;
pub mod uot;

pub use calib::{ContextLearnState, DriftGuardKind};
pub use pack::compile_context_pack;
pub use search::{ContextIndex, IndexUpdateSummary};
pub use tokenize_unified::{LexicalFeatures, analyze_text, lexical_jaccard, sketch_similarity};
pub use unit_match::{ReconciledUnits, reconcile_resource_units};
pub use unit_scan::{UnitBuildInput, extract_units_from_document};

#[cfg(test)]
mod tests {
    use super::*;
    use pagesift_graph::context::{
        AssetSpecStamp, ContextPackRequest, ContextSearchRequest, PackOrdering, PackSelector,
        UnitChangeKind, UnitKind, UnitLineageKind, UnitMatchMode, UnitStatus,
    };
    use pagesift_graph::{
        ConflictKind, ConflictRecord, ConflictSeverity, ConflictVariant, DocNode,
        EvidenceAlignment, EvidenceSpan, ExtractionMethod, MoneyPeriod, NormalizedValue,
        ObservationOutcome, SourceKind, TemporalSemantics, parse_timestamp_ms,
    };

    fn make_doc(doc_id: &str, url: &str, ts: u64) -> DocNode {
        DocNode {
            doc_id: doc_id.to_owned(),
            resource_key: url.to_owned(),
            requested_url: url.to_owned(),
            final_url: url.to_owned(),
            canonical_url: None,
            source_kind: SourceKind::Html,
            observed_at_ms: ts,
            status_code: 200,
            engine: "http".to_owned(),
            content_hash: Some(format!("ch_{doc_id}")),
            raw_hash: None,
            text_hash: None,
            etag: None,
            last_modified: None,
            title: Some("Test Doc".to_owned()),
        }
    }

    #[test]
    fn unit_identity_survives_prefix_insertion_and_guards_distinct_skus() {
        let url = "https://example.com/catalog";
        let md_v1 = "# Catalog\n\n## Alpha Widget\n\nSKU-101 enterprise license costs $99 per seat monthly.\n\n## Beta Widget\n\nSKU-202 standard license costs $49 per seat monthly.\n";
        let n1 = make_doc("doc_v1", url, 1_000);
        let u_v1 = extract_units_from_document(&UnitBuildInput::simple(&n1, md_v1));
        let alpha_v1 = u_v1
            .iter()
            .find(|u| u.text.contains("SKU-101"))
            .expect("alpha unit");

        // Version 2: insert a new banner section at the top, remove Alpha SKU-101, and change Gamma SKU-303 to $99!
        let md_v2 = "# Promo Banner\n\nLimited autumn announcement.\n\n# Catalog\n\n## Gamma Widget\n\nSKU-303 enterprise license costs $99 per seat monthly.\n\n## Beta Widget\n\nSKU-202 standard license costs $49 per seat monthly.\n";
        let n2 = make_doc("doc_v2", url, 2_000);
        let u_v2_raw = extract_units_from_document(&UnitBuildInput::simple(&n2, md_v2));

        let rec = reconcile_resource_units(
            &u_v1,
            u_v2_raw,
            &ObservationOutcome::VerifiedContent,
            false,
            2_000,
            UnitMatchMode::BoundedUot,
        );

        // Beta Widget was shifted down by the banner, yet MUST keep its exact logical `unit_id` (`Relocated`)!
        let beta_v1 = u_v1.iter().find(|u| u.text.contains("SKU-202")).unwrap();
        let beta_v2 = rec
            .live_units
            .iter()
            .find(|u| u.text.contains("SKU-202"))
            .unwrap();
        assert_eq!(beta_v1.unit_id, beta_v2.unit_id);
        assert_eq!(beta_v2.lineage.relation, UnitLineageKind::Relocated);

        // Gamma SKU-303 has nearly identical wording & same $99 price as deleted Alpha SKU-101,
        // MUST NOT falsely inherit SKU-101's identity!
        let gamma_v2 = rec
            .live_units
            .iter()
            .find(|u| u.text.contains("SKU-303"))
            .unwrap();
        assert_ne!(gamma_v2.unit_id, alpha_v1.unit_id);
        assert_eq!(gamma_v2.lineage.relation, UnitLineageKind::Created);
    }

    #[test]
    fn bounded_uot_detects_paragraph_split_and_barrier_preserves_unverified() {
        let url = "https://example.com/spec";
        let md_v1 = "# Limits\n\nRate limiter enforces 120 requests per minute per tenant and rejects payloads exceeding 16 MiB compressed size with HTTP 413.\n";
        let n1 = make_doc("doc_1", url, 1_000);
        let u_v1 = extract_units_from_document(&UnitBuildInput::simple(&n1, md_v1));
        let orig_para = u_v1.iter().find(|u| u.kind == UnitKind::Paragraph).unwrap();

        // Split the single paragraph into two distinct paragraphs in v2
        let md_v2 = "# Limits\n\nRate limiter enforces 120 requests per minute per tenant across API keys.\n\nGateway rejects payloads exceeding 16 MiB compressed size with HTTP 413 status.\n";
        let n2 = make_doc("doc_2", url, 2_000);
        let u_v2_raw = extract_units_from_document(&UnitBuildInput::simple(&n2, md_v2));

        let rec_split = reconcile_resource_units(
            &u_v1,
            u_v2_raw,
            &ObservationOutcome::VerifiedContent,
            false,
            2_000,
            UnitMatchMode::BoundedUot,
        );
        let split_children: Vec<_> = rec_split
            .live_units
            .iter()
            .filter(|u| u.lineage.relation == UnitLineageKind::SplitFrom)
            .collect();
        assert_eq!(split_children.len(), 2);
        assert_ne!(split_children[0].unit_id, split_children[1].unit_id);
        assert_eq!(
            split_children[0].lineage.parent_unit_ids,
            vec![orig_para.unit_id.clone()]
        );
        assert!(
            rec_split
                .transitions
                .iter()
                .any(|t| t.change_kind == UnitChangeKind::Split)
        );

        // Transient 503 error or truncated capture must mark units `Unverified`, NOT `ConfirmedWithdrawn`!
        let rec_err = reconcile_resource_units(
            &rec_split.live_units,
            Vec::new(),
            &ObservationOutcome::TransientFetchError {
                error: "503".to_owned(),
            },
            false,
            3_000,
            UnitMatchMode::BoundedUot,
        );
        assert_eq!(rec_err.live_units.len(), rec_split.live_units.len());
        assert!(
            rec_err
                .live_units
                .iter()
                .all(|u| matches!(u.status, UnitStatus::Unverified { .. }))
        );
        assert!(rec_err.retired_units.is_empty());
    }

    #[test]
    fn search_respects_as_of_and_temporal_validity_and_pack_keeps_table_headers_and_conflicts() {
        let mut idx = ContextIndex::new(AssetSpecStamp::default());
        let mut learn = ContextLearnState::default();

        // Doc 2025: historical price $100 valid in 2025
        let mut d_2025 = make_doc(
            "doc_2025",
            "https://vendor.example/pricing-2025",
            1_736_000_000_000,
        );
        let t_2025 = TemporalSemantics {
            observed_at_ms: d_2025.observed_at_ms,
            published_at: None,
            published_at_ms: None,
            modified_at: None,
            modified_at_ms: None,
            valid_from: Some("2025-01-01T00:00:00Z".to_owned()),
            valid_until: Some("2025-12-31T23:59:59Z".to_owned()),
        };
        let md_2025 = "Table 1: Enterprise Pricing (2025)\n\n| Tier | Monthly Rate | Notes |\n| --- | --- | --- |\n| Pro | $100 | Excludes regional tax |\n";
        let units_2025 = extract_units_from_document(&UnitBuildInput {
            doc_node: &d_2025,
            markdown: md_2025,
            subject_key: None,
            temporal: Some(&t_2025),
            blocks: &[],
            claims: &[],
            evidence: &[],
            origin_root: "https://vendor.example",
            origin_independent: true,
            max_table_unit_tokens: 512,
        });

        // Doc 2026: newer price $140 valid in 2026 + an opposing analyst report saying $125
        d_2025.observed_at_ms = 1_768_000_000_000;
        let d_2026 = make_doc(
            "doc_2026",
            "https://vendor.example/pricing-2026",
            1_768_000_000_000,
        );
        let t_2026 = TemporalSemantics {
            observed_at_ms: d_2026.observed_at_ms,
            published_at: None,
            published_at_ms: None,
            modified_at: None,
            modified_at_ms: None,
            valid_from: Some("2026-01-01T00:00:00Z".to_owned()),
            valid_until: Some("2026-12-31T23:59:59Z".to_owned()),
        };
        let md_2026 = "Pro tier monthly rate is $140 excluding VAT for `--max-requests` quota.";
        let units_2026 = extract_units_from_document(&UnitBuildInput {
            doc_node: &d_2026,
            markdown: md_2026,
            subject_key: None,
            temporal: Some(&t_2026),
            blocks: &[],
            claims: &[],
            evidence: &[],
            origin_root: "https://vendor.example",
            origin_independent: true,
            max_table_unit_tokens: 512,
        });

        let d_audit = make_doc(
            "doc_audit",
            "https://audit.independent.org/review",
            1_768_000_500_000,
        );
        let units_audit = extract_units_from_document(&UnitBuildInput {
            doc_node: &d_audit,
            markdown: "Independent audit measured effective Pro tier monthly rate at $125 under standard contract.",
            subject_key: None,
            temporal: Some(&t_2026),
            blocks: &[],
            claims: &[],
            evidence: &[],
            origin_root: "https://audit.independent.org",
            origin_independent: true,
            max_table_unit_tokens: 512,
        });

        let mut all = Vec::new();
        all.extend(units_2025);
        all.extend(units_2026);
        all.extend(units_audit);
        idx.apply_updates(&all, &[], &AssetSpecStamp::default());

        // 1. Temporal query for mid-2025 (`effective_at_ms` in June 2025) MUST return $100 table, NOT 2026 $140!
        let june_2025 = parse_timestamp_ms("2025-06-15T00:00:00Z").unwrap();
        let rep_2025 = idx.search(
            &ContextSearchRequest {
                query: "Pro tier monthly rate".to_owned(),
                effective_at_ms: Some(june_2025),
                ..Default::default()
            },
            &[],
            &[],
            Vec::new(),
            &mut learn,
        );
        assert_eq!(rep_2025.hits.len(), 1);
        assert!(rep_2025.hits[0].unit.text.contains("$100"));
        assert!(
            rep_2025.hits[0]
                .unit
                .text
                .contains("Table 1: Enterprise Pricing (2025)")
        );

        // 2. Active conflict between `doc_2026` and `doc_audit`: Pack must preserve BOTH sides + headers!
        let conflict = ConflictRecord {
            conflict_id: "cnf_1".to_owned(),
            subject_key: "https://vendor.example".to_owned(),
            field_path: "/price".to_owned(),
            normalized_field: "price".to_owned(),
            kind: ConflictKind::ValueMismatch,
            severity: ConflictSeverity::High,
            variants: vec![
                ConflictVariant {
                    value: NormalizedValue::Money {
                        amount_e4: 1_400_000,
                        currency: "USD".to_owned(),
                        period: MoneyPeriod::Monthly,
                        raw: "$140".to_owned(),
                    },
                    raw_values: vec![serde_json::json!("$140")],
                    support_count: 1,
                    independent_source_count: 1,
                    latest_observed_at_ms: 1_768_000_000_000,
                    sources: vec!["https://vendor.example/pricing-2026".to_owned()],
                    formats: vec!["html".to_owned()],
                    engines: vec!["http".to_owned()],
                    evidence: vec![EvidenceSpan {
                        doc_id: "doc_2026".to_owned(),
                        url: "https://vendor.example/pricing-2026".to_owned(),
                        block_id: None,
                        chunk_idx: None,
                        start: None,
                        end: None,
                        excerpt: "$140".to_owned(),
                        confidence: 1.0,
                        alignment: EvidenceAlignment::ExactBlock,
                        verified: true,
                        field_path: "/price".to_owned(),
                        method: ExtractionMethod::JsonLd {
                            entity_type: None,
                            entity_id: None,
                        },
                    }],
                },
                ConflictVariant {
                    value: NormalizedValue::Money {
                        amount_e4: 1_250_000,
                        currency: "USD".to_owned(),
                        period: MoneyPeriod::Monthly,
                        raw: "$125".to_owned(),
                    },
                    raw_values: vec![serde_json::json!("$125")],
                    support_count: 1,
                    independent_source_count: 1,
                    latest_observed_at_ms: 1_768_000_500_000,
                    sources: vec!["https://audit.independent.org/review".to_owned()],
                    formats: vec!["html".to_owned()],
                    engines: vec!["http".to_owned()],
                    evidence: vec![EvidenceSpan {
                        doc_id: "doc_audit".to_owned(),
                        url: "https://audit.independent.org/review".to_owned(),
                        block_id: None,
                        chunk_idx: None,
                        start: None,
                        end: None,
                        excerpt: "$125".to_owned(),
                        confidence: 1.0,
                        alignment: EvidenceAlignment::ExactBlock,
                        verified: true,
                        field_path: "/price".to_owned(),
                        method: ExtractionMethod::JsonLd {
                            entity_type: None,
                            entity_id: None,
                        },
                    }],
                },
            ],
            preferred_hint: None,
        };

        let rep_symbol = idx.search(
            &ContextSearchRequest {
                query: "--max-requests".to_owned(),
                include_conflicts: true,
                ..Default::default()
            },
            std::slice::from_ref(&conflict),
            &[],
            Vec::new(),
            &mut learn,
        );
        // Even though only `doc_2026` mentions `--max-requests`, `doc_audit` is co-retrieved as conflict counterpart!
        assert_eq!(rep_symbol.independent_source_count, 2);

        let packed = compile_context_pack(
            &ContextPackRequest {
                search: ContextSearchRequest {
                    query: "--max-requests".to_owned(),
                    ..Default::default()
                },
                budget_tokens: 512,
                selector: PackSelector::SubmodularClosure,
                ordering: PackOrdering::EdgeSalience,
                ..Default::default()
            },
            &rep_symbol,
            10,
        );
        assert!(packed.must_keep_preserved);
        assert!(!packed.budget_insufficient);
        assert!(packed.compiled_context.contains("$140"));
        assert!(packed.compiled_context.contains("$125"));
        assert!(packed.compiled_context.contains("CONFLICTS DETECTED"));
        assert_eq!(packed.provenance_manifest.len(), 2);
    }
}
