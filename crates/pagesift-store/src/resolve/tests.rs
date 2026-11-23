use std::collections::BTreeSet;

use pagesift_graph::resolve::{
    ConclusionRecord, EpistemicKind, EvidenceBelnap, ProofStandard, RESOLVE_SCHEMA_VERSION,
    ResolutionState, ResolveEventKind, ScopeDependency, ScopeDomainKind,
};

use crate::{Config, Store};

#[allow(clippy::too_many_arguments)]
fn sample_conclusion(
    id: &str,
    answer: &str,
    units: Vec<&str>,
    sources: Vec<&str>,
    scopes: Vec<&str>,
    parents: Vec<&str>,
    state: ResolutionState,
    checked_at_ms: u64,
) -> ConclusionRecord {
    ConclusionRecord {
        schema_version: RESOLVE_SCHEMA_VERSION,
        conclusion_id: id.to_owned(),
        version: 1,
        content_digest: format!("dg-{id}-{checked_at_ms}"),
        epistemic_kind: EpistemicKind::Conclusion,
        domain: Some("pricing".to_owned()),
        query: Some("pricing check".to_owned()),
        answer_text: answer.to_owned(),
        belnap: EvidenceBelnap::SUPPORTED,
        state,
        proof_standard: ProofStandard::Preponderance,
        claims: Vec::new(),
        scope: ScopeDependency {
            domain_kind: ScopeDomainKind::OpenWorld,
            source_ids: sources.into_iter().map(str::to_owned).collect(),
            subject_keys: Vec::new(),
            observed_entities: Vec::new(),
            scope_predicates: scopes.into_iter().map(str::to_owned).collect(),
            has_negative_or_universal_dependency: false,
            as_of_ms: None,
            effective_at_ms: None,
        },
        assumptions: Vec::new(),
        exceptions: Vec::new(),
        parent_conclusion_ids: parents.into_iter().map(str::to_owned).collect(),
        dependent_unit_ids: units.into_iter().map(str::to_owned).collect(),
        dependent_resource_keys: vec!["https://vendor.example/pricing".to_owned()],
        origin_roots: vec!["vendor.example".to_owned()],
        independent_support_roots: vec!["vendor.example".to_owned()],
        independent_refute_roots: Vec::new(),
        provenance: None,
        cut_set_analysis: None,
        unresolved_gaps: Vec::new(),
        counter_audit: None,
        corrections: Vec::new(),
        stale_reason: None,
        verifier_version: "resolve-det-v1".to_owned(),
        model_id: None,
        created_at_ms: checked_at_ms,
        updated_at_ms: checked_at_ms,
        checked_at_ms,
    }
}

#[test]
fn resolve_store_commits_revisions_indexes_cycles_tenant_isolation_and_gc_immunity() {
    let dir = tempfile::tempdir().unwrap();
    let mut cfg = Config::new(dir.path());
    cfg.max_bytes = Some(1024);
    let store = Store::open(cfg).unwrap();

    let rs_a = store.resolve_for(Some("acct:tenant-a"));
    let rs_b = store.resolve_for(Some("acct:tenant-b"));

    let c1 = sample_conclusion(
        "rcon_1",
        "Product A costs $49/mo.",
        vec!["unt_a1"],
        vec!["kb"],
        vec!["source:kb", "term:price"],
        vec![],
        ResolutionState::Supported,
        1_000,
    );
    let out1 = rs_a.commit_conclusion(c1, None, "initial check").unwrap();
    assert!(out1.created);
    assert_eq!(out1.version, 1);

    let c2 = sample_conclusion(
        "rcon_2",
        "Derived summary from rcon_1.",
        vec!["unt_b1"],
        vec!["kb"],
        vec![],
        vec!["rcon_1"],
        ResolutionState::Supported,
        1_500,
    );
    rs_a.commit_conclusion(c2, None, "derived check").unwrap();

    // Transitive cycle rejection: rcon_1 cannot depend on rcon_2 which depends on rcon_1!
    let mut cyclic = rs_a.get_conclusion("rcon_1").unwrap().unwrap();
    cyclic.parent_conclusion_ids = vec!["rcon_2".to_owned()];
    let err = rs_a
        .commit_conclusion(cyclic, None, "illegal cycle")
        .unwrap_err();
    assert!(
        err.to_string().contains("resolve_cycle_detected"),
        "expected cycle rejection, got {err}"
    );

    // Update rcon_1 to v2 (Refuted) and verify immutable history + point-in-time replay.
    let mut v2 = rs_a.get_conclusion("rcon_1").unwrap().unwrap();
    v2.state = ResolutionState::Refuted;
    v2.checked_at_ms = 2_000;
    let out2 = rs_a
        .commit_conclusion(v2, None, "price changed to $59/mo")
        .unwrap();
    assert!(!out2.created);
    assert_eq!(out2.version, 2);

    let hist = rs_a.list_history("rcon_1", 10).unwrap();
    assert_eq!(hist.len(), 2);
    assert_eq!(hist[0].version, 2);
    assert_eq!(hist[1].version, 1);
    assert_eq!(
        rs_a.get_conclusion_as_of("rcon_1", 1_200)
            .unwrap()
            .unwrap()
            .state,
        ResolutionState::Supported
    );
    assert!(rs_a.get_conclusion_as_of("rcon_1", 500).unwrap().is_none());

    // Reverse lookup + transitive downstream expansion.
    let direct = rs_a.find_by_units(&["unt_a1".to_owned()]).unwrap();
    assert_eq!(direct, BTreeSet::from(["rcon_1".to_owned()]));
    let impacted = rs_a.expand_downstream_conclusions(&direct).unwrap();
    assert_eq!(
        impacted,
        BTreeSet::from(["rcon_1".to_owned(), "rcon_2".to_owned()])
    );

    // Scope predicate lookup (negative/superlative dependency).
    let scope_hits = rs_a
        .find_by_scope_predicates(&["term:price".to_owned()])
        .unwrap();
    assert_eq!(scope_hits, BTreeSet::from(["rcon_1".to_owned()]));

    // Tenant B must see zero records from Tenant A; existing_tenants must discover Tenant A!
    assert!(rs_b.get_conclusion("rcon_1").unwrap().is_none());
    let tenants = store
        .existing_tenants(&["acct:tenant-a".to_owned(), "acct:tenant-b".to_owned()])
        .unwrap();
    assert!(tenants.contains("acct:tenant-a"));
    assert!(!tenants.contains("acct:tenant-b"));

    // GC immunity + reopen durability.
    let _ = store.gc().unwrap();
    drop(store);
    let reopened = Store::open_dir(dir.path()).unwrap();
    let rs_reopen = reopened.resolve_for(Some("acct:tenant-a"));
    let loaded = rs_reopen.get_conclusion("rcon_1").unwrap().unwrap();
    assert_eq!(loaded.version, 2);
    assert_eq!(loaded.state, ResolutionState::Refuted);
    let events = rs_reopen.list_events(None, 10).unwrap();
    assert_eq!(events.len(), 3);
    assert_eq!(events[2].kind, ResolveEventKind::ConclusionCorrected);
}
