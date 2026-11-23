//! 1.7 Resolve deterministic verification core: context-preserving claim decomposition,
//! quantitative/currency/period checking, seven-layer Check, Minimal-Cut-Set-driven Counter
//! probing, and conservative domain-stratified Learn with drift fallback.

pub mod check;
pub mod counter;
pub mod decompose;
pub(crate) mod evidence_eval;
pub mod learn;
pub mod quant;

pub use check::{
    build_conclusion_record, build_scope_dependency, unit_scope_trigger_predicates,
    verify_answer_deterministic,
};
pub use counter::{plan_counter_probes, run_local_counter_probes};
pub use decompose::{MAX_DECOMPOSED_CLAIMS, analyze_atomic_claim, decompose_claims};
pub use learn::{
    MIN_RESOLVE_CALIB_SAMPLES, ResolveLearnState, evaluate_calibration_metrics, probe_kind_key,
};
pub use quant::{ExtractedQuantity, UnitDim, compare_quantities, extract_quantities};

#[cfg(test)]
mod tests {
    use super::*;
    use pagesift_graph::context::AssetSpecStamp;
    use pagesift_graph::resolve::{
        ProofStandard, QuantitativeMismatchKind, ResolutionState, ResolveCheckRequest,
        ScopeDomainKind,
    };
    use pagesift_graph::{DocNode, SourceKind, TemporalSemantics, sha256_hex};

    use crate::context::{ContextIndex, UnitBuildInput, extract_units_from_document};

    fn add_doc(
        idx: &mut ContextIndex,
        url: &str,
        md: &str,
        origin_root: &str,
        origin_indep: bool,
        observed_at_ms: u64,
        valid_until: Option<&str>,
    ) {
        let ch = sha256_hex(md.as_bytes());
        let node = DocNode {
            doc_id: format!("doc_{}", &ch[..12]),
            resource_key: pagesift_graph::normalize_resource_key(url),
            requested_url: url.into(),
            final_url: url.into(),
            canonical_url: None,
            source_kind: SourceKind::Html,
            observed_at_ms,
            status_code: 200,
            engine: "test".into(),
            content_hash: Some(ch),
            raw_hash: None,
            text_hash: None,
            etag: None,
            last_modified: None,
            title: None,
        };
        let temp = TemporalSemantics {
            observed_at_ms,
            published_at: None,
            published_at_ms: None,
            modified_at: None,
            modified_at_ms: None,
            valid_from: None,
            valid_until: valid_until.map(str::to_owned),
        };
        let units = extract_units_from_document(&UnitBuildInput {
            doc_node: &node,
            markdown: md,
            subject_key: None,
            temporal: Some(&temp),
            blocks: &[],
            claims: &[],
            evidence: &[],
            origin_root,
            origin_independent: origin_indep,
            max_table_unit_tokens: 512,
        });
        idx.apply_updates(&units, &[], &AssetSpecStamp::default());
    }

    #[test]
    fn catches_period_mismatch_mirror_inflation_stale_year_causal_leap_and_comparative_counterexample()
     {
        let mut idx = ContextIndex::default();
        // 1. Billing period trap: table explicitly says Annual ($99/yr), NOT monthly!
        add_doc(
            &mut idx,
            "https://vendor-a.example/pricing",
            "# Pricing\n\n| Plan | Annual Price |\n|---|---|\n| Plan Pro | $99 |\n| Plan Lite | $29 |\n",
            "vendor-a.example",
            true,
            1_770_000_000_000,
            None,
        );
        // 2. Syndicated mirror repeating Vendor A (MUST NOT count as 2nd independent root!)
        add_doc(
            &mut idx,
            "https://mirror-news.example/reprint",
            "Plan Pro is priced at $99/yr according to Vendor A.",
            "vendor-a.example",
            false,
            1_770_000_000_000,
            None,
        );
        // 3. Stale 2024 benchmark (explicitly expired on 2024-12-31)
        add_doc(
            &mut idx,
            "https://bench.example/2024",
            "In 2024 benchmark tests, Acme Engine ranked #1 in throughput.",
            "bench.example",
            true,
            1_770_000_000_000,
            Some("2024-12-31"),
        );
        // 4. Current (unexpired) correlational study
        add_doc(
            &mut idx,
            "https://macro-study.example/2026",
            "Higher power prices were correlated with lower data center spending across surveyed regions.",
            "macro-study.example",
            true,
            1_770_000_000_000,
            None,
        );

        let mut learn = ResolveLearnState::default();

        // Case A: "$99/mo" against "$99 (Annual Price)" table => Refuted (BillingPeriodMismatch)!
        let req_period = ResolveCheckRequest {
            answer: "Plan Pro costs $99/mo.".into(),
            ..ResolveCheckRequest::default()
        };
        let c_period =
            verify_answer_deterministic(&req_period, &idx, &[], &mut learn, 1_770_000_000_000);
        assert_eq!(c_period.state, ResolutionState::Refuted);
        assert_eq!(
            c_period.claims[0]
                .quantitative_check
                .as_ref()
                .map(|q| q.kind),
            Some(QuantitativeMismatchKind::BillingPeriodMismatch)
        );

        // Case B: "$99/yr" with ClearAndConvincing (>=2 independent roots) fails because mirror shares origin_root!
        let req_mirror = ResolveCheckRequest {
            answer: "Plan Pro costs $99/yr.".into(),
            proof_standard: ProofStandard::ClearAndConvincing,
            ..ResolveCheckRequest::default()
        };
        let c_mirror =
            verify_answer_deterministic(&req_mirror, &idx, &[], &mut learn, 1_770_000_000_000);
        assert_eq!(c_mirror.independent_support_roots.len(), 1);
        assert_eq!(c_mirror.state, ResolutionState::Insufficient);

        // Case C: "Plan Pro is the cheapest at $99/yr" => Refuted by sibling table row Plan Lite ($29/yr)!
        let req_comp = ResolveCheckRequest {
            answer: "Plan Pro is the cheapest plan at $99/yr.".into(),
            ..ResolveCheckRequest::default()
        };
        let c_comp =
            verify_answer_deterministic(&req_comp, &idx, &[], &mut learn, 1_770_000_000_000);
        assert_eq!(c_comp.state, ResolutionState::Refuted);

        // Case D: Stale 2024 benchmark cited for 2026 => Insufficient (temporal_valid = Some(false))!
        let req_stale = ResolveCheckRequest {
            answer: "Acme Engine still leads all platforms in 2026.".into(),
            effective_at_ms: Some(1_780_000_000_000),
            ..ResolveCheckRequest::default()
        };
        let c_stale =
            verify_answer_deterministic(&req_stale, &idx, &[], &mut learn, 1_780_000_000_000);
        assert!(!c_stale.state.is_supported_like());
        assert_eq!(c_stale.claims[0].temporal_valid, Some(false));

        // Case E: Causal assertion against correlational evidence => CausalUnidentifiable!
        let req_causal = ResolveCheckRequest {
            answer: "Higher power prices caused lower data center spending.".into(),
            ..ResolveCheckRequest::default()
        };
        let c_causal =
            verify_answer_deterministic(&req_causal, &idx, &[], &mut learn, 1_770_000_000_000);
        assert_eq!(c_causal.state, ResolutionState::CausalUnidentifiable);

        // Case F: Negative existential under OpenWorld => ScopeIncomplete (never Supported)!
        let req_neg = ResolveCheckRequest {
            answer: "There is no public record of Acme security incidents.".into(),
            domain_kind: ScopeDomainKind::OpenWorld,
            ..ResolveCheckRequest::default()
        };
        let c_neg = verify_answer_deterministic(&req_neg, &idx, &[], &mut learn, 1_770_000_000_000);
        assert_eq!(c_neg.state, ResolutionState::ScopeIncomplete);
    }

    #[test]
    fn counter_probes_surface_errata_and_exceptions_and_record_scope_audit() {
        let mut idx = ContextIndex::default();
        add_doc(
            &mut idx,
            "https://vendor-x.example/docs",
            "Orion Storage supports zero-downtime snapshots for developer clusters.",
            "vendor-x.example",
            true,
            1_770_000_000_000,
            None,
        );
        // Official errata notice using deprecation/correction wording without direct negation tokens
        add_doc(
            &mut idx,
            "https://vendor-x.example/errata",
            "Errata: Orion Storage snapshot tier deprecated and superseded by paid v2 add-on.",
            "vendor-x.example",
            true,
            1_770_000_010_000,
            None,
        );

        let mut learn = ResolveLearnState::default();
        let req = ResolveCheckRequest {
            answer: "Orion Storage supports zero-downtime snapshots.".into(),
            domain_kind: ScopeDomainKind::OpenWorld,
            ..ResolveCheckRequest::default()
        };
        let mut conclusion =
            verify_answer_deterministic(&req, &idx, &[], &mut learn, 1_770_000_020_000);
        assert_eq!(conclusion.state, ResolutionState::Supported);
        let plans = plan_counter_probes(&conclusion, &learn);
        assert!(!plans.is_empty());

        let audit = run_local_counter_probes(
            &mut conclusion,
            &plans,
            &idx,
            req.as_of_ms,
            req.include_unverified,
        );
        assert!(audit.probes_executed > 0);
        assert!(
            audit.counter_evidence_found > 0,
            "counter search must surface errata/deprecation notice missed by primary Check"
        );
        assert_eq!(conclusion.state, ResolutionState::Refuted);

        // When no counterevidence exists in OpenWorld, audit must explicitly declare absence is not proof!
        let mut clean_idx = ContextIndex::default();
        add_doc(
            &mut clean_idx,
            "https://vendor-y.example/docs",
            "Helios Cache supports TLS 1.3 encryption.",
            "vendor-y.example",
            true,
            1_770_000_000_000,
            None,
        );
        let clean_req = ResolveCheckRequest {
            answer: "Helios Cache supports TLS 1.3 encryption.".into(),
            domain_kind: ScopeDomainKind::OpenWorld,
            ..ResolveCheckRequest::default()
        };
        let mut clean_c =
            verify_answer_deterministic(&clean_req, &clean_idx, &[], &mut learn, 1_770_000_020_000);
        let clean_plans = plan_counter_probes(&clean_c, &learn);
        let clean_audit =
            run_local_counter_probes(&mut clean_c, &clean_plans, &clean_idx, None, false);
        assert_eq!(clean_audit.counter_evidence_found, 0);
        assert!(
            clean_audit
                .coverage_statement
                .contains("absence_is_not_proof")
        );

        // Verify ScopeDependency trigger predicate duality for Live negative-dependency matching
        let sup_req = ResolveCheckRequest {
            answer: "Plan Pro is the cheapest plan at $99/yr.".into(),
            source_ids: vec!["pricing-src".into()],
            ..ResolveCheckRequest::default()
        };
        let sup_c = verify_answer_deterministic(&sup_req, &idx, &[], &mut learn, 1_770_000_020_000);
        assert!(sup_c.scope.has_negative_or_universal_dependency);
        assert!(
            sup_c
                .scope
                .scope_predicates
                .iter()
                .any(|p| p == "scope_metric:price")
        );

        let mut competitor_idx = ContextIndex::default();
        add_doc(
            &mut competitor_idx,
            "https://vendor-z.example/pricing",
            "Plan Nano is priced at $19/yr for developers.",
            "vendor-z.example",
            true,
            1_770_000_030_000,
            None,
        );
        let new_u = competitor_idx.units.values().next().unwrap();
        let triggers = unit_scope_trigger_predicates("pricing-src", new_u);
        assert!(
            triggers
                .iter()
                .any(|p| sup_c.scope.scope_predicates.contains(p))
        );
    }
}
