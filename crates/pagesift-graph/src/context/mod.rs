//! 1.6 Context shared data contracts: stable [`KnowledgeUnit`] identity, dependency-governed
//! [`ContextDelta`], and multi-dimensional Search / Pack / Action DTOs.

pub mod delta;
pub mod query;
pub mod unit;

pub use delta::{
    AssetDependency, AssetInvalidation, AssetKind, AssetSpecStamp, ConsumerCheckpoint,
    ContextDelta, ContextDeltaPollResult, IndexVisibility, MaintenanceMode, ReplayStatus,
    UnitChangeKind, UnitTransition, choose_maintenance_mode,
};
pub use query::{
    AcquisitionStepRecord, ActionMode, ContextActionOutcome, ContextActionRequest, ContextPack,
    ContextPackRequest, ContextSearchHit, ContextSearchReport, ContextSearchRequest,
    CoverageDiagnosis, InformationGap, PackCostBreakdown, PackOrdering, PackSelector,
    PackedSection, ProvenanceEntry, SearchDimensionScores, StoppingReason, UnitMatchMode,
    ValidityStatus,
};
pub use unit::{
    AmbiguousCandidate, KnowledgeUnit, TableStructureMeta, UnitKind, UnitLineage, UnitLineageKind,
    UnitLocation, UnitStatus,
};

#[cfg(test)]
mod tests {
    use super::*;
    use crate::model::SourceKind;

    fn sample_unit(byte_start: usize, heading: &str, text: &str, hash: &str) -> KnowledgeUnit {
        let unit_id = KnowledgeUnit::mint_unit_id(
            "https://example.com/pricing",
            UnitKind::Paragraph,
            heading,
            hash,
            0,
        );
        let revision_id = KnowledgeUnit::compute_revision_id(&unit_id, hash, &[], None);
        KnowledgeUnit {
            unit_id,
            revision_id,
            content_hash: hash.to_owned(),
            kind: UnitKind::Paragraph,
            text: text.to_owned(),
            tokens: 12,
            subject_key: None,
            entity_anchors: Vec::new(),
            has_negation_or_qualifier: false,
            has_units_or_currency: true,
            location: UnitLocation {
                doc_id: "doc_1".to_owned(),
                resource_key: "https://example.com/pricing".to_owned(),
                source_url: "https://example.com/pricing".to_owned(),
                source_kind: SourceKind::Html,
                block_ids: vec!["doc_1#b1".to_owned()],
                byte_start,
                byte_end: byte_start + text.len(),
                page_number: None,
                heading_path: vec![heading.to_owned()],
                code_language: None,
                table_meta: None,
            },
            lineage: UnitLineage::created(),
            status: UnitStatus::Active,
            temporal: None,
            origin_root: "https://example.com/pricing".to_owned(),
            origin_independent: true,
            claim_ids: Vec::new(),
            evidence_spans: Vec::new(),
            observed_at_ms: 1_700_000_000_000,
        }
    }

    #[test]
    fn relocation_preserves_content_assets_but_invalidates_location_assets() {
        let spec = AssetSpecStamp::default();
        let u_v1 = sample_unit(
            100,
            "Enterprise",
            "Enterprise seat costs $49/month.",
            "h_ent",
        );
        let emb_dep = AssetDependency::from_unit(AssetKind::Embedding, &u_v1, spec.clone());
        let struct_dep = AssetDependency::from_unit(AssetKind::StructureIndex, &u_v1, spec.clone());

        let mut u_relocated = u_v1.clone();
        u_relocated.location.doc_id = "doc_2".to_owned();
        u_relocated.location.byte_start = 420;
        u_relocated.location.byte_end = 420 + u_relocated.text.len();

        // Pure relocation keeps Embedding valid without recomputation, while StructureIndex refreshes.
        assert!(emb_dep.validate(&u_relocated, &spec).is_ok());
        assert_eq!(
            struct_dep.validate(&u_relocated, &spec),
            Err("location_or_structure_changed".to_owned())
        );

        // Upgrading the embedding model invalidates Embedding even when text is identical.
        let mut new_model_spec = spec.clone();
        new_model_spec.model_id = "embed-v2".to_owned();
        assert_eq!(
            emb_dep.validate(&u_relocated, &new_model_spec),
            Err("model_id_changed".to_owned())
        );

        // Lexical index ignores model_id changes when tokenizer_version is unchanged.
        let lex_dep = AssetDependency::from_unit(AssetKind::LexicalIndex, &u_v1, spec);
        assert!(lex_dep.validate(&u_relocated, &new_model_spec).is_ok());
    }

    #[test]
    fn adaptive_maintenance_switches_to_rebuild_under_high_churn_or_tombstones() {
        assert_eq!(
            choose_maintenance_mode(200, 10, 5),
            MaintenanceMode::Incremental
        );
        assert!(matches!(
            choose_maintenance_mode(200, 110, 5),
            MaintenanceMode::FullRebuild { .. }
        ));
        assert!(matches!(
            choose_maintenance_mode(100, 4, 50),
            MaintenanceMode::FullRebuild { .. }
        ));
    }

    #[test]
    fn expired_status_and_delimiter_boundaries_are_enforced() {
        let spec = AssetSpecStamp::default();
        let u1 = sample_unit(0, "Pricing", "Pro costs $29/month.", "h1");
        let lex_dep = AssetDependency::from_unit(AssetKind::LexicalIndex, &u1, spec.clone());

        let mut u_expired = u1.clone();
        u_expired.status = UnitStatus::Expired {
            valid_until: "2025-12-31T23:59:59Z".to_owned(),
        };
        assert_eq!(
            lex_dep.validate(&u_expired, &spec),
            Err("unit_status_expired".to_owned())
        );

        // Comma inside block_id or claim_id must never collide with multi-element lists
        let mut loc_a = u1.location.clone();
        loc_a.block_ids = vec!["doc#b1,doc#b2".to_owned()];
        let mut loc_b = u1.location.clone();
        loc_b.block_ids = vec!["doc#b1".to_owned(), "doc#b2".to_owned()];
        assert_ne!(loc_a.location_hash(), loc_b.location_hash());

        let rev_a = KnowledgeUnit::compute_revision_id(
            &u1.unit_id,
            &u1.content_hash,
            &["c1,c2".to_owned()],
            None,
        );
        let rev_b = KnowledgeUnit::compute_revision_id(
            &u1.unit_id,
            &u1.content_hash,
            &["c1".to_owned(), "c2".to_owned()],
            None,
        );
        assert_ne!(rev_a, rev_b);
    }
}
