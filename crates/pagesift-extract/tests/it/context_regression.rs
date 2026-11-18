// wall-clock-file: diagnostic hot-path benchmark printout only; tests never assert wall-clock bounds
use std::collections::BTreeSet;
use std::time::Instant;

use pagesift_extract::context::dpp::{DppCandidate, select_bounded_dpp};
use pagesift_extract::context::tokenize_unified::light_english_stem;
use pagesift_extract::context::uot::{SparseCostEdge, solve_bounded_uot};
use pagesift_extract::context::{
    ContextIndex, ContextLearnState, DriftGuardKind, UnitBuildInput, analyze_text,
    compile_context_pack, extract_units_from_document, reconcile_resource_units,
};
use pagesift_graph::context::{
    AssetSpecStamp, ContextPackRequest, ContextSearchRequest, KnowledgeUnit, PackOrdering,
    PackSelector, TableStructureMeta, UnitChangeKind, UnitKind, UnitLineage, UnitLineageKind,
    UnitLocation, UnitMatchMode, UnitStatus, ValidityStatus,
};
use pagesift_graph::{
    ClaimRecord, ConflictKind, ConflictRecord, ConflictSeverity, ConflictVariant, DocNode,
    EvidenceAlignment, EvidenceSpan, ExtractionMethod, MoneyPeriod, NormalizedValue,
    ObservationOutcome, SourceKind, SubjectKey, SubjectKeySource, TemporalSemantics,
    parse_timestamp_ms,
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
        title: Some("Context Test Doc".to_owned()),
    }
}

#[test]
fn utf8_multibyte_leading_tokens_contractions_and_stemming_are_safe_and_accurate() {
    // Regression for byte-slice panic `&tok[1..]` when token starts with multibyte UTF-8 scalar
    let tricky = "中文aB ¥100a €50Beta 🚀v2Flag αBeta データaB don't doesn’t cannot 除く";
    let feats = analyze_text(tricky);
    assert!(feats.has_negation_or_qualifier);
    assert!(feats.has_units_or_currency);
    assert!(feats.exact_symbols.contains("中文ab"));
    assert!(feats.exact_symbols.contains("€50beta"));
    assert!(feats.exact_symbols.contains("🚀v2flag"));

    // Stemmer must not mangle `-ss` / `-us` / `-is` singular words (`process` vs `processes`)
    assert_eq!(light_english_stem("process"), "process");
    assert_eq!(light_english_stem("processes"), "process");
    assert_eq!(light_english_stem("status"), "status");
    assert_eq!(light_english_stem("analysis"), "analysis");

    // Query coverage for plural/camelCase terms must reach 1.0 when document matches all concepts!
    let mut idx = ContextIndex::new(AssetSpecStamp::default());
    let doc = make_doc("d_cov", "https://api.example/quotas", 1_770_000_000_000);
    let units = extract_units_from_document(&UnitBuildInput::simple(
        &doc,
        "Gateway enforces BurstRps ceiling per request across tenants.",
    ));
    idx.apply_updates(&units, &[], &AssetSpecStamp::default());

    let mut learn = ContextLearnState::default();
    let rep = idx.search(
        &ContextSearchRequest {
            query: "BurstRps requests".to_owned(),
            ..Default::default()
        },
        &[],
        &[],
        Vec::new(),
        &mut learn,
    );
    assert_eq!(rep.hits.len(), 1);
    assert!(
        (rep.dimensions.coverage - 1.0).abs() < 1e-5,
        "expected full concept coverage 1.0, got {}",
        rep.dimensions.coverage
    );
}

#[test]
fn gfm_escaped_pipes_empty_headers_slice_provenance_and_code_fence_pages() {
    let md = "```markdown\n<!-- page 99 -->\n| fake | table\n```\n\n\
              <!-- page 2 -->\n\n\
              | | Param \\| Alias | Limit |\n\
              | --- | --- | --- |\n\
              | TierA | `str \\| null` | 10 reqs |\n\
              | TierB | `int \\| null` | 20 reqs |\n";
    let doc = make_doc("d_tbl", "https://docs.example/ref", 1_770_000_000_000);
    let units = extract_units_from_document(&UnitBuildInput::simple(&doc, md));

    let tbl = units.iter().find(|u| u.kind == UnitKind::Table).unwrap();
    // Must ignore `<!-- page 99 -->` inside fenced code block and pick real `<!-- page 2 -->`!
    assert_eq!(tbl.location.page_number, Some(2));
    let meta = tbl.location.table_meta.as_ref().unwrap();
    assert_eq!(meta.col_count, 3);
    assert_eq!(meta.headers, vec!["", "Param | Alias", "Limit"]);

    // Unclosed code fence with CJK/Emoji must never panic or corrupt offsets
    let unclosed = "# Intro\n\n~~~python\ndef run():\n    return '你好🚀'\n<!-- page 77 -->";
    let u_unclosed = extract_units_from_document(&UnitBuildInput::simple(&doc, unclosed));
    let code_u = u_unclosed
        .iter()
        .find(|u| u.kind == UnitKind::CodeBlock)
        .unwrap();
    assert_eq!(code_u.location.code_language.as_deref(), Some("python"));
    assert_eq!(
        &unclosed[code_u.location.byte_start..code_u.location.byte_end],
        code_u.text
    );

    // Large table row-slicing must assign non-overlapping per-slice byte coordinates so a ClaimRecord
    // on row 1 is NOT falsely attached to later slices!
    let mut big_tbl = String::from("| SKU | Description | Monthly Price |\n| --- | --- | --- |\n");
    for r in 0..16 {
        big_tbl.push_str(&format!(
            "| SKU-{:03} | Enterprise dedicated cluster node group {} with redundant NVMe | ${} |\n",
            r + 1,
            r + 1,
            100 + r
        ));
    }
    let row1_pos = big_tbl.find("SKU-001").unwrap();
    let claim_row1 = ClaimRecord {
        claim_id: "clm_sku_001".to_owned(),
        doc_id: "d_big".to_owned(),
        resource_key: "https://docs.example/matrix".to_owned(),
        source_url: "https://docs.example/matrix".to_owned(),
        canonical_url: None,
        host: "docs.example".to_owned(),
        source_kind: SourceKind::Html,
        engine: "http".to_owned(),
        subject_key: "sku:001".to_owned(),
        contract_id: "default".to_owned(),
        field_path: "/sku001/price".to_owned(),
        normalized_field: "price".to_owned(),
        raw_value: serde_json::json!("$100"),
        normalized_value: NormalizedValue::Text {
            canonical: "$100".to_owned(),
            raw: "$100".to_owned(),
        },
        observed_at_ms: 1_770_000_000_000,
        temporal: None,
        evidence: EvidenceSpan {
            doc_id: "d_big".to_owned(),
            url: "https://docs.example/matrix".to_owned(),
            block_id: None,
            chunk_idx: None,
            start: Some(row1_pos),
            end: Some(row1_pos + 7),
            excerpt: "SKU-001".to_owned(),
            confidence: 1.0,
            alignment: EvidenceAlignment::ExactBlock,
            verified: true,
            field_path: "/sku001/price".to_owned(),
            method: ExtractionMethod::JsonLd {
                entity_type: None,
                entity_id: None,
            },
        },
    };
    let doc_big = make_doc("d_big", "https://docs.example/matrix", 1_770_000_000_000);
    let sliced_units = extract_units_from_document(&UnitBuildInput {
        doc_node: &doc_big,
        markdown: &big_tbl,
        subject_key: None,
        temporal: None,
        blocks: &[],
        claims: &[claim_row1],
        evidence: &[],
        origin_root: "docs.example",
        origin_independent: true,
        max_table_unit_tokens: 80,
    });
    assert!(sliced_units.len() >= 2);
    assert_eq!(sliced_units[0].claim_ids, vec!["clm_sku_001".to_owned()]);
    for later in &sliced_units[1..] {
        assert!(
            later.claim_ids.is_empty(),
            "row-1 claim leaked into later table slice: {:?}",
            later.claim_ids
        );
        assert!(later.location.byte_start > sliced_units[0].location.byte_start);
    }
}

#[test]
fn uot_and_dpp_handle_nan_inf_degenerate_inputs_and_replay_deterministically() {
    assert!(solve_bounded_uot(&[], &[1.0], &[]).is_empty());
    assert!(
        solve_bounded_uot(
            &[0.0, f64::NAN],
            &[1.0, f64::INFINITY],
            &[SparseCostEdge {
                old_idx: 0,
                new_idx: 0,
                cost: 0.1
            }]
        )
        .is_empty()
    );
    // Duplicate edges and +inf sentinel edges must be deduplicated / filtered cleanly
    let links = solve_bounded_uot(
        &[2.0],
        &[2.0],
        &[
            SparseCostEdge {
                old_idx: 0,
                new_idx: 0,
                cost: 0.25,
            },
            SparseCostEdge {
                old_idx: 0,
                new_idx: 0,
                cost: 0.10,
            },
            SparseCostEdge {
                old_idx: 0,
                new_idx: 0,
                cost: 1e9,
            },
        ],
    );
    assert_eq!(links.len(), 1);
    assert!(links[0].mass.is_finite() && links[0].mass > 0.0);

    // DPP with unnormalized vectors, collinear duplicates, zero vectors, NaN quality, and exact ties
    let s_big = [10.0f32, 20.0, 0.0, f32::NAN];
    let s_zero = [0.0f32; 4];
    let cands = [
        DppCandidate {
            quality: f64::NAN,
            cost_tokens: 10,
            sketch: &s_zero,
            origin_root: "a.example",
        },
        DppCandidate {
            quality: 0.90,
            cost_tokens: 20,
            sketch: &s_big,
            origin_root: "b.example",
        },
        DppCandidate {
            quality: 0.90,
            cost_tokens: 20,
            sketch: &s_big,
            origin_root: "b.example",
        },
        DppCandidate {
            quality: 0.90,
            cost_tokens: 20,
            sketch: &s_zero,
            origin_root: "c.example",
        },
    ];
    let run1 = select_bounded_dpp(&cands, 45);
    for _ in 0..10 {
        assert_eq!(select_bounded_dpp(&cands, 45), run1);
    }
    // Tie between #1 and #2 (identical vector/root) vs #3 (independent root): #1 beats #2 by index tie-break,
    // then #3 beats duplicate #2 by orthogonal log-det diversity!
    assert!(run1.contains(&1) && run1.contains(&3));

    // ContextLearnState must ignore NaN feedback without poisoning calibration
    let mut learn = ContextLearnState::with_drift_guard(DriftGuardKind::BernoulliBocpd);
    assert!(!learn.observe_feedback(f32::NAN, true, true));
    let (cal, _) = learn.calibrate(f32::NAN, true);
    assert!(cal.is_finite() && (0.0..=1.0).contains(&cal));
}

#[test]
fn unit_reconcile_guards_alpha_skus_iso_dates_versions_table_rows_and_large_windows() {
    let url = "https://catalog.example/plans";
    let d1 = make_doc("d1", url, 1_000);
    let d2 = make_doc("d2", url, 2_000);

    // 1. Alpha-only SKU (`SKU-ALPHA` vs `SKU-BETA`), version (`v1.5` vs `v1.6`), ISO date (`2025-01-01` vs `2026-01-01`),
    //    and same-header distinct-primary-key table rows (`Starter` vs `Business`) MUST NOT falsely inherit!
    let v1 = "## 2025 Pricing\n\nEffective 2025-01-01, SKU-ALPHA under runtime v1.5 costs $49 monthly.\n\n\
              | Plan | Seats | Monthly |\n| --- | --- | --- |\n| Starter | 10 | $49 |\n";
    let v2 = "## 2026 Pricing\n\nEffective 2026-01-01, SKU-BETA under runtime v1.6 costs $49 monthly.\n\n\
              | Plan | Seats | Monthly |\n| --- | --- | --- |\n| Business | 10 | $49 |\n";
    let u1 = extract_units_from_document(&UnitBuildInput::simple(&d1, v1));
    let u2 = extract_units_from_document(&UnitBuildInput::simple(&d2, v2));
    let old_ids: BTreeSet<&str> = u1.iter().map(|u| u.unit_id.as_str()).collect();

    let rec = reconcile_resource_units(
        &u1,
        u2,
        &ObservationOutcome::VerifiedContent,
        false,
        2_000,
        UnitMatchMode::BoundedUot,
    );
    for nu in &rec.live_units {
        assert!(
            !old_ids.contains(nu.unit_id.as_str()),
            "false unit_id inheritance across distinct entity/year/table-row: {:?}",
            nu.text
        );
        assert_eq!(nu.lineage.relation, UnitLineageKind::Created);
    }

    // 2. Asymmetric >64 unmatched units (`70` old vs `60` new): tail unit #69 rewritten to #59 must still match!
    let mut big_v1 = String::new();
    let mut big_v2 = String::new();
    for i in 0..70 {
        big_v1.push_str(&format!(
            "Paragraph {i} describes unique telemetry counter metric_alpha_{i} for node health.\n\n"
        ));
    }
    for i in 10..70 {
        big_v2.push_str(&format!(
            "Paragraph {i} describes updated telemetry counter metric_alpha_{i} for cluster node health.\n\n"
        ));
    }
    let bu1 = extract_units_from_document(&UnitBuildInput::simple(&d1, &big_v1));
    let bu2 = extract_units_from_document(&UnitBuildInput::simple(&d2, &big_v2));
    assert_eq!(bu1.len(), 70);
    assert_eq!(bu2.len(), 60);
    let tail_id_v1 = bu1[69].unit_id.clone();

    let rec_big = reconcile_resource_units(
        &bu1,
        bu2,
        &ObservationOutcome::VerifiedContent,
        false,
        2_000,
        UnitMatchMode::BoundedUot,
    );
    assert_eq!(
        rec_big.live_units[59].unit_id, tail_id_v1,
        "tail unit across asymmetric >64 UOT window lost identity!"
    );
}

#[test]
fn search_enforces_exact_gregorian_years_unknown_validity_and_mirror_non_inflation() {
    let mut idx = ContextIndex::new(AssetSpecStamp::default());
    let mut learn = ContextLearnState::default();

    // Unit A: mentions `2024` in text, no explicit `validFrom`
    let d_2024 = make_doc("d2024", "https://primary.example/2024", 1_735_600_000_000);
    let u_2024 = extract_units_from_document(&UnitBuildInput {
        doc_node: &d_2024,
        markdown: "In 2024, standard storage rate was $20 per TiB.",
        subject_key: None,
        temporal: None,
        blocks: &[],
        claims: &[],
        evidence: &[],
        origin_root: "primary.example",
        origin_independent: true,
        max_table_unit_tokens: 512,
    });

    // Unit B: undated evergreen note (`UnknownValidity`, malformed `valid_from: "TBD"` must NOT fake `ValidAtTarget`!)
    let d_unknown = make_doc("d_unk", "https://primary.example/faq", 1_735_600_000_000);
    let bad_temp = TemporalSemantics {
        observed_at_ms: d_unknown.observed_at_ms,
        published_at: None,
        published_at_ms: None,
        modified_at: None,
        modified_at_ms: None,
        valid_from: Some("TBD".to_owned()),
        valid_until: None,
    };
    let u_unk = extract_units_from_document(&UnitBuildInput {
        doc_node: &d_unknown,
        markdown: "Standard storage rate excludes cross-region egress fees.",
        subject_key: None,
        temporal: Some(&bad_temp),
        blocks: &[],
        claims: &[],
        evidence: &[],
        origin_root: "primary.example",
        origin_independent: true,
        max_table_unit_tokens: 512,
    });

    // Unit C: syndicated mirror on a DIFFERENT host (`repost-hub.net`), explicitly `origin_independent: false`!
    let d_mirror = make_doc("d_mir", "https://repost-hub.net/copy", 1_735_600_000_000);
    let u_mirror = extract_units_from_document(&UnitBuildInput {
        doc_node: &d_mirror,
        markdown: "In 2024, standard storage rate was $20 per TiB.",
        subject_key: None,
        temporal: None,
        blocks: &[],
        claims: &[],
        evidence: &[],
        origin_root: "repost-hub.net",
        origin_independent: false,
        max_table_unit_tokens: 512,
    });

    // Unit D: unverified unit that is referenced by a conflict — must NOT leak when `include_unverified == false`!
    let d_unv = make_doc("d_unv", "https://other-audit.org/draft", 1_735_600_000_000);
    let mut u_unv = extract_units_from_document(&UnitBuildInput {
        doc_node: &d_unv,
        markdown: "Draft storage rate was $25 per TiB.",
        subject_key: None,
        temporal: None,
        blocks: &[],
        claims: &[],
        evidence: &[],
        origin_root: "other-audit.org",
        origin_independent: true,
        max_table_unit_tokens: 512,
    });
    u_unv[0].status = UnitStatus::Unverified {
        since_ms: 1_735_600_100_000,
        reason: "truncated".to_owned(),
    };

    let mut all = Vec::new();
    all.extend(u_2024);
    all.extend(u_unk);
    all.extend(u_mirror);
    all.extend(u_unv);
    idx.apply_updates(&all, &[], &AssetSpecStamp::default());

    // Exact Gregorian boundary: `2024-12-31T22:00:00Z` IS still 2024 (old `31_557_600_000` divisor misclassified it as 2025!)
    let dec31_2024 = parse_timestamp_ms("2024-12-31T22:00:00Z").unwrap();
    let unv_conflict = ConflictRecord {
        conflict_id: "cf_unv".to_owned(),
        subject_key: "https://primary.example/2024".to_owned(),
        field_path: "/rate".to_owned(),
        normalized_field: "rate".to_owned(),
        kind: ConflictKind::ValueMismatch,
        severity: ConflictSeverity::Medium,
        variants: vec![
            ConflictVariant {
                value: NormalizedValue::Text {
                    canonical: "$20".to_owned(),
                    raw: "$20".to_owned(),
                },
                raw_values: vec![serde_json::json!("$20")],
                support_count: 1,
                independent_source_count: 1,
                latest_observed_at_ms: 1_735_600_000_000,
                sources: vec!["https://primary.example/2024".to_owned()],
                formats: vec!["html".to_owned()],
                engines: vec!["http".to_owned()],
                evidence: vec![EvidenceSpan {
                    doc_id: "d2024".to_owned(),
                    url: "https://primary.example/2024".to_owned(),
                    block_id: None,
                    chunk_idx: None,
                    start: None,
                    end: None,
                    excerpt: "$20".to_owned(),
                    confidence: 1.0,
                    alignment: EvidenceAlignment::ExactBlock,
                    verified: true,
                    field_path: "/rate".to_owned(),
                    method: ExtractionMethod::JsonLd {
                        entity_type: None,
                        entity_id: None,
                    },
                }],
            },
            ConflictVariant {
                value: NormalizedValue::Text {
                    canonical: "$25".to_owned(),
                    raw: "$25".to_owned(),
                },
                raw_values: vec![serde_json::json!("$25")],
                support_count: 1,
                independent_source_count: 1,
                latest_observed_at_ms: 1_735_600_000_000,
                sources: vec!["https://other-audit.org/draft".to_owned()],
                formats: vec!["html".to_owned()],
                engines: vec!["http".to_owned()],
                evidence: vec![EvidenceSpan {
                    doc_id: "d_unv".to_owned(),
                    url: "https://other-audit.org/draft".to_owned(),
                    block_id: None,
                    chunk_idx: None,
                    start: None,
                    end: None,
                    excerpt: "$25".to_owned(),
                    confidence: 1.0,
                    alignment: EvidenceAlignment::ExactBlock,
                    verified: false,
                    field_path: "/rate".to_owned(),
                    method: ExtractionMethod::JsonLd {
                        entity_type: None,
                        entity_id: None,
                    },
                }],
            },
        ],
        preferred_hint: None,
    };

    let rep = idx.search(
        &ContextSearchRequest {
            query: "standard storage rate".to_owned(),
            effective_at_ms: Some(dec31_2024),
            include_unverified: false,
            include_conflicts: true,
            ..Default::default()
        },
        &[unv_conflict],
        &[],
        Vec::new(),
        &mut learn,
    );

    // 1. `d2024` must be `ValidAtTarget` on 2024-12-31T22:00:00Z!
    let hit_2024 = rep
        .hits
        .iter()
        .find(|h| h.unit.location.doc_id == "d2024")
        .expect("d2024 must be valid on 2024-12-31T22:00:00Z");
    assert_eq!(hit_2024.validity_status, ValidityStatus::ValidAtTarget);

    // 2. `d_unk` (with unparseable `valid_from: "TBD"`) must stay `UnknownValidity`, NEVER `ValidAtTarget`!
    let hit_unk = rep
        .hits
        .iter()
        .find(|h| h.unit.location.doc_id == "d_unk")
        .unwrap();
    assert_eq!(hit_unk.validity_status, ValidityStatus::UnknownValidity);

    // 3. `d_mirror` (`origin_independent == false`) must NOT inflate `independent_source_count` beyond 1 (`primary.example`)!
    assert_eq!(rep.independent_source_count, 1);

    // 4. `d_unv` (`Unverified`) must NOT be co-retrieved when `include_unverified == false`!
    assert!(rep.hits.iter().all(|h| h.unit.location.doc_id != "d_unv"));
}

#[test]
fn pack_deduplicates_shared_conflict_counterparts_slices_pipeless_tables_and_obeys_hard_budget() {
    let mut idx = ContextIndex::new(AssetSpecStamp::default());
    let mut learn = ContextLearnState::default();

    // Long CJK heading path (header line alone consumes ~38 tokens, >2x old hardcoded 16-token estimate!)
    let d_a = make_doc("doc_a", "https://vendor-a.example/p", 1_770_000_000_000);
    let d_b = make_doc("doc_bench", "https://bench.org/audit", 1_770_000_000_000);
    let d_c = make_doc("doc_c", "https://vendor-c.example/p", 1_770_000_000_000);

    let md_a = "# 企业级云平台采购指南 > 2026年度计费标准与服务等级协议说明\n\nVendor A claims cluster rate is $100 monthly unless reserved.\n";
    let md_b = "# 独立第三方基准审计报告 > 2026年度实际结算核查结果\n\nBenchmark audit measured cluster rate at $130 monthly.\n";
    let md_c = "# 渠道合作商报价说明 > 2026年度补充协议细则\n\nPartner C claims cluster rate is $115 monthly.\n";

    let mut units = Vec::new();
    units.extend(extract_units_from_document(&UnitBuildInput {
        doc_node: &d_a,
        markdown: md_a,
        subject_key: Some(&SubjectKey {
            key: "svc:cluster".to_owned(),
            source: SubjectKeySource::ExplicitConfig,
            raw: "svc:cluster".to_owned(),
        }),
        temporal: None,
        blocks: &[],
        claims: &[],
        evidence: &[],
        origin_root: "vendor-a.example",
        origin_independent: true,
        max_table_unit_tokens: 512,
    }));
    units.extend(extract_units_from_document(&UnitBuildInput {
        doc_node: &d_b,
        markdown: md_b,
        subject_key: Some(&SubjectKey {
            key: "svc:cluster".to_owned(),
            source: SubjectKeySource::ExplicitConfig,
            raw: "svc:cluster".to_owned(),
        }),
        temporal: None,
        blocks: &[],
        claims: &[],
        evidence: &[],
        origin_root: "bench.org",
        origin_independent: true,
        max_table_unit_tokens: 512,
    }));
    units.extend(extract_units_from_document(&UnitBuildInput {
        doc_node: &d_c,
        markdown: md_c,
        subject_key: Some(&SubjectKey {
            key: "svc:cluster".to_owned(),
            source: SubjectKeySource::ExplicitConfig,
            raw: "svc:cluster".to_owned(),
        }),
        temporal: None,
        blocks: &[],
        claims: &[],
        evidence: &[],
        origin_root: "vendor-c.example",
        origin_independent: true,
        max_table_unit_tokens: 512,
    }));
    idx.apply_updates(&units, &[], &AssetSpecStamp::default());

    let make_var = |doc_id: &str, url: &str, raw: &str| ConflictVariant {
        value: NormalizedValue::Text {
            canonical: raw.to_owned(),
            raw: raw.to_owned(),
        },
        raw_values: vec![serde_json::json!(raw)],
        support_count: 1,
        independent_source_count: 1,
        latest_observed_at_ms: 1_770_000_000_000,
        sources: vec![url.to_owned()],
        formats: vec!["html".to_owned()],
        engines: vec!["http".to_owned()],
        evidence: vec![EvidenceSpan {
            doc_id: doc_id.to_owned(),
            url: url.to_owned(),
            block_id: None,
            chunk_idx: None,
            start: None,
            end: None,
            excerpt: raw.to_owned(),
            confidence: 1.0,
            alignment: EvidenceAlignment::ExactBlock,
            verified: true,
            field_path: "/rate".to_owned(),
            method: ExtractionMethod::JsonLd {
                entity_type: None,
                entity_id: None,
            },
        }],
    };

    // Two distinct conflicts sharing the SAME opposing unit (`doc_bench`)!
    let conflicts = vec![
        ConflictRecord {
            conflict_id: "cf_ab".to_owned(),
            subject_key: "svc:cluster".to_owned(),
            field_path: "/rate".to_owned(),
            normalized_field: "rate".to_owned(),
            kind: ConflictKind::ValueMismatch,
            severity: ConflictSeverity::High,
            variants: vec![
                make_var("doc_a", "https://vendor-a.example/p", "$100"),
                make_var("doc_bench", "https://bench.org/audit", "$130"),
            ],
            preferred_hint: None,
        },
        ConflictRecord {
            conflict_id: "cf_cb".to_owned(),
            subject_key: "svc:cluster".to_owned(),
            field_path: "/rate_partner".to_owned(),
            normalized_field: "rate_partner".to_owned(),
            kind: ConflictKind::ValueMismatch,
            severity: ConflictSeverity::High,
            variants: vec![
                make_var("doc_c", "https://vendor-c.example/p", "$115"),
                make_var("doc_bench", "https://bench.org/audit", "$130"),
            ],
            preferred_hint: None,
        },
    ];

    let rep = idx.search(
        &ContextSearchRequest {
            query: "cluster rate monthly".to_owned(),
            subject_key: Some("svc:cluster".to_owned()),
            include_conflicts: true,
            ..Default::default()
        },
        &conflicts,
        &[],
        Vec::new(),
        &mut learn,
    );

    // 1. Generous budget: all 3 unique conflict units (`$100`, `$130`, `$115`) must appear ONCE (no duplicate `doc_bench`!)
    for sel in [
        PackSelector::SubmodularClosure,
        PackSelector::BoundedDpp,
        PackSelector::MmrGreedy,
        PackSelector::TopKGreedy,
    ] {
        let pack = compile_context_pack(
            &ContextPackRequest {
                search: ContextSearchRequest {
                    query: "cluster rate monthly".to_owned(),
                    ..Default::default()
                },
                budget_tokens: 320,
                selector: sel,
                ..Default::default()
            },
            &rep,
            5,
        );
        assert!(
            pack.total_tokens <= pack.effective_budget_tokens
                && pack.effective_budget_tokens <= 320,
            "selector {sel:?} exceeded budget: total={} effective={}",
            pack.total_tokens,
            pack.effective_budget_tokens
        );
        assert!(pack.must_keep_preserved);
        assert!(pack.compiled_context.contains("$100"));
        assert!(pack.compiled_context.contains("$130"));
        assert!(pack.compiled_context.contains("$115"));
        let unique_uids: BTreeSet<&str> =
            pack.sections.iter().map(|s| s.unit_id.as_str()).collect();
        assert_eq!(
            unique_uids.len(),
            pack.sections.len(),
            "duplicate unit emitted in sections for {sel:?}!"
        );
        assert_eq!(pack.provenance_manifest.len(), pack.sections.len());
    }

    // 2. Tight budget where 1 conflict pair fits (~110 tok) but deduplicated union of both mandatory conflicts (~148 tok) does NOT:
    //    MUST NOT exceed hard budget AND MUST honestly report `budget_insufficient` + `!must_keep_preserved`!
    let tight_pack = compile_context_pack(
        &ContextPackRequest {
            search: ContextSearchRequest {
                query: "cluster rate monthly".to_owned(),
                ..Default::default()
            },
            budget_tokens: 120,
            token_safety_margin: 0.0,
            selector: PackSelector::SubmodularClosure,
            ..Default::default()
        },
        &rep,
        5,
    );
    assert!(
        tight_pack.total_tokens <= 120,
        "breached hard token budget: {} > 120",
        tight_pack.total_tokens
    );
    assert!(tight_pack.budget_insufficient);
    assert!(!tight_pack.must_keep_preserved);

    // Rust `Default` and JSON serde defaults on `ContextSearchRequest` must match identically!
    let serde_def: ContextSearchRequest = serde_json::from_str(r#"{"query":""}"#).unwrap();
    assert_eq!(ContextSearchRequest::default(), serde_def);

    // 3. Pipeless GFM table (`Col A | Col B` without leading `|`) must slice cleanly under tight budget!
    let mut pipeless_md = String::from("Tier | Quota | Notes\n--- | --- | ---\n");
    for i in 0..20 {
        pipeless_md.push_str(&format!(
            "Plan{i} | {}00 rpm | Standard tier row {i}\n",
            i + 1
        ));
    }
    pipeless_md.push_str("Enterprise | 9000 rpm | Excludes burst unless pre-warmed\n");
    let d_pipe = make_doc("d_pipe", "https://docs.example/pipeless", 1_770_000_000_000);
    let u_pipe = extract_units_from_document(&UnitBuildInput::simple(&d_pipe, &pipeless_md));
    let mut idx_pipe = ContextIndex::new(AssetSpecStamp::default());
    idx_pipe.apply_updates(&u_pipe, &[], &AssetSpecStamp::default());
    let rep_pipe = idx_pipe.search(
        &ContextSearchRequest {
            query: "Enterprise 9000 rpm".to_owned(),
            ..Default::default()
        },
        &[],
        &[],
        Vec::new(),
        &mut learn,
    );
    let pack_pipe = compile_context_pack(
        &ContextPackRequest {
            search: ContextSearchRequest {
                query: "Enterprise 9000 rpm".to_owned(),
                ..Default::default()
            },
            budget_tokens: 90,
            token_safety_margin: 0.0,
            ..Default::default()
        },
        &rep_pipe,
        1,
    );
    assert!(pack_pipe.total_tokens <= 90);
    assert!(pack_pipe.compiled_context.contains("Tier | Quota | Notes"));
    assert!(pack_pipe.compiled_context.contains("Enterprise | 9000 rpm"));
}

#[test]
fn persisted_drift_ood_sketch_shared_dpp_header_coalesced_index_and_3way_split_guards() {
    // 1. Persisted serde roundtrips + mid-stream `drift_guard` switch MUST preserve drift run-length!
    let mut state = ContextLearnState::default();
    for _ in 0..30 {
        assert!(!state.observe_feedback(0.92, true, true));
        let bytes = serde_json::to_vec(&state).unwrap();
        state = serde_json::from_slice(&bytes).unwrap();
    }
    let (cal_before, used_iso) = state.calibrate(0.92, true);
    assert!(used_iso && cal_before > 0.85);

    let mut drifted_after_roundtrips = false;
    for _ in 0..20 {
        if state.observe_feedback(0.90, false, true) {
            drifted_after_roundtrips = true;
            break;
        }
        let bytes = serde_json::to_vec(&state).unwrap();
        state = serde_json::from_slice(&bytes).unwrap();
    }
    assert!(
        drifted_after_roundtrips && state.drift_fallbacks >= 1,
        "CUSUM failed to detect drift across serde persistence roundtrips!"
    );
    assert_eq!(state.calibrate(0.92, true), (0.50, false));

    // Mid-stream switch from `CusumPageHinkley` -> `BernoulliBocpd` without losing history
    let mut switched = ContextLearnState::with_drift_guard(DriftGuardKind::CusumPageHinkley);
    for _ in 0..30 {
        assert!(!switched.observe_feedback(0.91, true, true));
    }
    switched.drift_guard = DriftGuardKind::BernoulliBocpd;
    let mut bocpd_triggered = false;
    for _ in 0..15 {
        if switched.observe_feedback(0.89, false, true) {
            bocpd_triggered = true;
            break;
        }
    }
    assert!(
        bocpd_triggered && switched.drift_fallbacks >= 1,
        "Switching drift_guard mid-stream lost historical run-length!"
    );

    // 2. Signed feature hashing MUST stay zero-mean unbiased on OOD queries (zero false recall)!
    let f_recipe = analyze_text(
        "Preheat Dutch oven to 240 Celsius, fold fermented sourdough levain with unbleached wheat flour and sea salt.",
    );
    assert!(
        f_recipe.sketch.iter().any(|&v| v < 0.0),
        "Rademacher sketch must retain negative coordinates!"
    );
    let mut ood_idx = ContextIndex::new(AssetSpecStamp::default());
    let bakery_paragraphs = [
        "Preheat Dutch oven to 240 Celsius and bake sourdoughcountry loaf for thirty minutes until crust caramelizes.",
        "Laminate chilledcultured butter into croissant dough across three letter folds before overnight proofing.",
        "Whisk Madagascar vanilla bean paste into heavy pastry cream with egg yolks and caster sugar.",
        "Monthly invoice settlement requires wire remittance within thirty calendar days of statement issuance.",
        "Warehouse pallet handling fee applies to oversized freight shipments exceeding standard dock clearance.",
        "Annual office lease renewal includes municipal property tax escalation and lobby maintenance charges.",
    ];
    let mut bakery_units = Vec::new();
    for (i, para) in bakery_paragraphs.iter().enumerate() {
        let d = make_doc(
            &format!("d_ood_{i}"),
            &format!("https://bakery.example/p/{i}"),
            1_770_000_000_000,
        );
        bakery_units.extend(extract_units_from_document(&UnitBuildInput::simple(
            &d, para,
        )));
    }
    ood_idx.apply_updates(&bakery_units, &[], &AssetSpecStamp::default());
    let mut ood_learn = ContextLearnState::default();
    for ood_q in [
        "quantum superconducting tokamak plasma magnetics confinement",
        "neutrino oscillation chromodynamics quark gluon lattice",
    ] {
        let rep_ood = ood_idx.search(
            &ContextSearchRequest {
                query: ood_q.to_owned(),
                enable_semantic_sketch: true,
                ..Default::default()
            },
            &[],
            &[],
            Vec::new(),
            &mut ood_learn,
        );
        assert!(
            rep_ood.hits.is_empty(),
            "OOD query `{ood_q}` falsely recalled unrelated units via sketch bias: {:?}",
            rep_ood
                .hits
                .iter()
                .map(|h| &h.unit.text)
                .collect::<Vec<_>>()
        );
    }

    // 3. `BoundedDpp` with shared `row_count == 0` table header + 2 headerless data-row blocks:
    //    MUST NOT treat sibling rows as collinear clones of header AND MUST NOT double-charge header tokens!
    let make_tbl_unit = |idx_ord: usize,
                         b_start: usize,
                         b_end: usize,
                         txt: &str,
                         headers: Vec<String>,
                         rows: usize| {
        let loc = UnitLocation {
            doc_id: "d_shared_hdr".to_owned(),
            resource_key: "https://telemetry.example/slo".to_owned(),
            source_url: "https://telemetry.example/slo".to_owned(),
            source_kind: SourceKind::Html,
            heading_path: vec!["SLO Matrix".to_owned()],
            block_ids: vec![format!("blk_{idx_ord}")],
            byte_start: b_start,
            byte_end: b_end,
            page_number: None,
            code_language: None,
            table_meta: Some(TableStructureMeta {
                caption: None,
                headers,
                row_count: rows,
                col_count: 2,
                is_slice: idx_ord > 0,
            }),
        };
        let feats = analyze_text(txt);
        let chash = format!("uch_tbl_{idx_ord}");
        let uid = KnowledgeUnit::mint_unit_id(
            &loc.resource_key,
            UnitKind::Table,
            "SLO Matrix",
            &chash,
            idx_ord,
        );
        let rev_id = KnowledgeUnit::compute_revision_id(&uid, &chash, &[], None);
        KnowledgeUnit {
            unit_id: uid,
            revision_id: rev_id,
            content_hash: chash,
            kind: UnitKind::Table,
            text: txt.to_owned(),
            tokens: (txt.len() / 4).max(1),
            subject_key: None,
            entity_anchors: feats.entity_anchors,
            has_negation_or_qualifier: feats.has_negation_or_qualifier,
            has_units_or_currency: feats.has_units_or_currency,
            location: loc,
            lineage: UnitLineage::created(),
            status: UnitStatus::Active,
            temporal: None,
            origin_root: "telemetry.example".to_owned(),
            origin_independent: true,
            claim_ids: Vec::new(),
            evidence_spans: Vec::new(),
            observed_at_ms: 1_770_000_000_000,
        }
    };

    let u_hdr = make_tbl_unit(
        0,
        0,
        40,
        "| Metric | Ceiling |\n| --- | --- |",
        vec!["Metric".to_owned(), "Ceiling".to_owned()],
        0,
    );
    let u_row1 = make_tbl_unit(
        1,
        42,
        100,
        "| LatencyP99 | 45ms regional gateway target |",
        Vec::new(),
        1,
    );
    let u_row2 = make_tbl_unit(
        2,
        102,
        160,
        "| ErrorBudget | 99.95% availability target |",
        Vec::new(),
        1,
    );
    let mut hdr_idx_store = ContextIndex::new(AssetSpecStamp::default());
    hdr_idx_store.apply_updates(
        &[u_hdr.clone(), u_row1.clone(), u_row2.clone()],
        &[],
        &AssetSpecStamp::default(),
    );
    let rep_tbl = hdr_idx_store.search(
        &ContextSearchRequest {
            query: "Metric LatencyP99 ErrorBudget target".to_owned(),
            ..Default::default()
        },
        &[],
        &[],
        Vec::new(),
        &mut ood_learn,
    );
    assert_eq!(rep_tbl.hits.len(), 3);

    // Measure exact 3-section union token cost via generous pack first
    let full_dpp = compile_context_pack(
        &ContextPackRequest {
            search: ContextSearchRequest {
                query: "Metric LatencyP99 ErrorBudget target".to_owned(),
                ..Default::default()
            },
            budget_tokens: 400,
            token_safety_margin: 0.0,
            selector: PackSelector::BoundedDpp,
            ordering: PackOrdering::DocumentOrder,
            ..Default::default()
        },
        &rep_tbl,
        1,
    );
    assert_eq!(
        full_dpp.sections.len(),
        3,
        "BoundedDpp falsely dropped a data row sharing header: {:?}",
        full_dpp
            .sections
            .iter()
            .map(|s| &s.rendered_text)
            .collect::<Vec<_>>()
    );
    let hdr_sec_tokens = full_dpp.sections[0].tokens;
    let sum_sec_tokens: usize = full_dpp.sections.iter().map(|s| s.tokens).sum();
    // Tight budget fits `1 * hdr + row1 + row2` (`full_dpp.total_tokens`), MUST NOT fit `2 * hdr + row1 + row2`!
    let tight_dpp_budget = full_dpp.total_tokens.max(sum_sec_tokens) + 1;
    assert!(tight_dpp_budget < sum_sec_tokens + hdr_sec_tokens);

    for sel in [
        PackSelector::BoundedDpp,
        PackSelector::MmrGreedy,
        PackSelector::SubmodularClosure,
    ] {
        let tight_dpp = compile_context_pack(
            &ContextPackRequest {
                search: ContextSearchRequest {
                    query: "Metric LatencyP99 ErrorBudget target".to_owned(),
                    ..Default::default()
                },
                budget_tokens: tight_dpp_budget,
                token_safety_margin: 0.0,
                selector: sel,
                ordering: PackOrdering::DocumentOrder,
                ..Default::default()
            },
            &rep_tbl,
            1,
        );
        assert_eq!(
            tight_dpp.sections.len(),
            3,
            "selector {sel:?} double-charged shared header or treated sibling rows as collinear (sections={})",
            tight_dpp.sections.len()
        );
        assert!(tight_dpp.compiled_context.contains("LatencyP99"));
        assert!(tight_dpp.compiled_context.contains("ErrorBudget"));
        assert!(tight_dpp.total_tokens <= tight_dpp_budget);
    }

    // 4. `ContextIndex::apply_updates` duplicate `unit_id` batch coalescing + dead-status filtering!
    let d_dup = make_doc("d_dup", "https://docs.example/dup", 1_770_000_000_000);
    let base_units = extract_units_from_document(&UnitBuildInput::simple(
        &d_dup,
        "Legacy baseline phantom_alpha --legacy-alpha config.",
    ));
    let u0 = base_units[0].clone();

    let mut u_same_rev = u0.clone();
    u_same_rev.location.heading_path = vec!["Renamed Heading".to_owned()];

    let mut u_modified = u0.clone();
    u_modified.text = "Updated replacement fresh_omega --modern-omega config.".to_owned();
    u_modified.content_hash = "uch_modified_fresh".to_owned();
    u_modified.revision_id = KnowledgeUnit::compute_revision_id(
        &u_modified.unit_id,
        &u_modified.content_hash,
        &[],
        None,
    );

    let mut dup_idx = ContextIndex::new(AssetSpecStamp::default());
    dup_idx.apply_updates(std::slice::from_ref(&u0), &[], &AssetSpecStamp::default());
    // Pass `[u_same_rev, u_modified]` in a single batch: last write (`u_modified`) MUST win without stale lex cache!
    dup_idx.apply_updates(
        &[u_same_rev, u_modified.clone()],
        &[],
        &AssetSpecStamp::default(),
    );

    for stale_q in ["phantom_alpha", "--legacy-alpha"] {
        let rep_old = dup_idx.search(
            &ContextSearchRequest {
                query: stale_q.to_owned(),
                enable_semantic_sketch: false,
                ..Default::default()
            },
            &[],
            &[],
            Vec::new(),
            &mut ood_learn,
        );
        assert!(
            rep_old.hits.is_empty(),
            "Ghost term `{stale_q}` survived duplicate-ID batch update!"
        );
    }
    for fresh_q in ["fresh_omega", "--modern-omega"] {
        let rep_new = dup_idx.search(
            &ContextSearchRequest {
                query: fresh_q.to_owned(),
                enable_semantic_sketch: false,
                ..Default::default()
            },
            &[],
            &[],
            Vec::new(),
            &mut ood_learn,
        );
        assert_eq!(rep_new.hits.len(), 1);
    }

    // Non-searchable terminal states (`Withdrawn` / `Superseded` / `Expired`) in `upserts` must be retired/excluded!
    let mut u_withdrawn = u_modified.clone();
    u_withdrawn.status = UnitStatus::Withdrawn {
        at_ms: 1_770_000_100_000,
        reason: "http_410".to_owned(),
    };
    let mut u_expired = extract_units_from_document(&UnitBuildInput::simple(
        &make_doc("d_exp", "https://docs.example/exp", 1_770_000_000_000),
        "Dead expired obsolete_beta rule.",
    ))[0]
        .clone();
    u_expired.status = UnitStatus::Expired {
        valid_until: "2025-12-31T23:59:59Z".to_owned(),
    };
    dup_idx.apply_updates(&[u_withdrawn, u_expired], &[], &AssetSpecStamp::default());
    for dead_q in ["fresh_omega", "--modern-omega", "obsolete_beta"] {
        let rep_dead = dup_idx.search(
            &ContextSearchRequest {
                query: dead_q.to_owned(),
                include_unverified: true,
                enable_semantic_sketch: false,
                ..Default::default()
            },
            &[],
            &[],
            Vec::new(),
            &mut ood_learn,
        );
        assert!(
            rep_dead.hits.is_empty(),
            "Dead status unit leaked into index for `{dead_q}`!"
        );
    }

    // 5. 3-way split/merge clone guard (`k >= 3` branches where pair `(1, 2)` is a clone)!
    let d_s1 = make_doc("ds1", "https://docs.example/split3", 1_000);
    let d_s2 = make_doc("ds2", "https://docs.example/split3", 2_000);
    let md_parent = "# Quotas\n\nGateway enforces 120 requests per minute per tenant and rejects payloads exceeding 16 MiB compressed size with HTTP 413 status.\n";
    let md_3children_with_clone = "# Quotas\n\n\
        Gateway enforces 120 requests per minute per tenant across active keys.\n\n\
        Proxy rejects payloads exceeding 16 MiB compressed size with HTTP 413 status.\n\n\
        Router rejects payloads exceeding 16 MiB compressed size with HTTP 413 status.\n";
    let u_parent = extract_units_from_document(&UnitBuildInput::simple(&d_s1, md_parent));
    let u_3children =
        extract_units_from_document(&UnitBuildInput::simple(&d_s2, md_3children_with_clone));
    let rec_3way = reconcile_resource_units(
        &u_parent,
        u_3children,
        &ObservationOutcome::VerifiedContent,
        false,
        2_000,
        UnitMatchMode::BoundedUot,
    );
    let split_count = rec_3way
        .live_units
        .iter()
        .filter(|u| u.lineage.relation == UnitLineageKind::SplitFrom)
        .count();
    assert!(
        split_count < 3,
        "3-way branch containing duplicate clones `[A, B_copy1, B_copy2]` was falsely classified as 3-way SplitFrom!"
    );
    assert!(
        !rec_3way
            .transitions
            .iter()
            .any(|t| t.change_kind == UnitChangeKind::Split && t.child_unit_ids.len() >= 3),
        "3-way Split transition falsely emitted despite cloned sibling pair (1, 2)!"
    );
}

#[test]
#[ignore = "manual hot-path microbenchmark for before/after comparison"]
fn bench_context_hotpaths() {
    // 1. `analyze_text` mixed English + code symbols + CJK corpus
    let sample_md = "# Enterprise API Quotas & Rate Limits (2026)\n\n\
        Configure `--max-requests-per-tick`, `BurstRps`, and `ERR_RATE_LIMIT_429` in `pagesift.toml`.\n\
        Enterprise seat license costs $49 per month unless annual commitment applies (excludes regional VAT).\n\
        配额上限与退订条款：企业版支持 30 天内无条件退订，单租户最大并发 128 MiB 负载。\n";
    for _ in 0..50 {
        std::hint::black_box(analyze_text(sample_md));
    }
    let iters_tok = 2_000usize;
    let t0 = Instant::now();
    for _ in 0..iters_tok {
        std::hint::black_box(analyze_text(std::hint::black_box(sample_md)));
    }
    let analyze_us = t0.elapsed().as_secs_f64() * 1e6 / (iters_tok as f64);

    // 2. `reconcile_resource_units` (BoundedUot across 24-section document evolution)
    let mut v1 = String::new();
    let mut v2 = String::from("> Q4 Notice: Scheduled maintenance window.\n\n");
    for s in 0..24 {
        v1.push_str(&format!(
            "## Section {s} — Subsystem Module\n\nModule SKU-{:03} enforces {} requests per minute with 64 MiB buffer unless overridden.\n\n",
            100 + s,
            120 + s * 10
        ));
        let idx = if s < 12 { 11 - s } else { s };
        if idx % 3 == 0 {
            v2.push_str(&format!(
                "## Section {idx} — Subsystem Module\n\nModule SKU-{:03} enforces {} requests per minute with 64 MiB memory buffer unless overridden by policy.\n\n",
                100 + idx,
                120 + idx * 10
            ));
        } else {
            v2.push_str(&format!(
                "## Section {idx} — Subsystem Module\n\nModule SKU-{:03} enforces {} requests per minute with 64 MiB buffer unless overridden.\n\n",
                100 + idx,
                120 + idx * 10
            ));
        }
    }
    let d1 = make_doc("d1", "https://docs.example/spec", 1_770_000_000_000);
    let d2 = make_doc("d2", "https://docs.example/spec", 1_770_000_010_000);
    let u1 = extract_units_from_document(&UnitBuildInput::simple(&d1, &v1));
    let u2 = extract_units_from_document(&UnitBuildInput::simple(&d2, &v2));
    for _ in 0..20 {
        std::hint::black_box(reconcile_resource_units(
            &u1,
            u2.clone(),
            &ObservationOutcome::VerifiedContent,
            false,
            1_770_000_010_000,
            UnitMatchMode::BoundedUot,
        ));
    }
    let iters_match = 300usize;
    let mut match_elapsed = 0.0f64;
    for _ in 0..iters_match {
        let fresh = u2.clone();
        let tm = Instant::now();
        let out = reconcile_resource_units(
            &u1,
            fresh,
            &ObservationOutcome::VerifiedContent,
            false,
            1_770_000_010_000,
            UnitMatchMode::BoundedUot,
        );
        match_elapsed += tm.elapsed().as_secs_f64();
        std::hint::black_box(out);
    }
    let match_us = match_elapsed * 1e6 / (iters_match as f64);

    // 3. `ContextIndex::search` + `compile_context_pack` over 240 units across 40 docs
    let spec = AssetSpecStamp::default();
    let mut index = ContextIndex::new(spec.clone());
    let mut all_units = Vec::new();
    for doc_i in 0..40 {
        let url = format!("https://source-{}.example/docs/pricing", doc_i % 8);
        let did = format!("doc_{doc_i}");
        let node = make_doc(&did, &url, 1_770_000_000_000 + (doc_i as u64) * 1_000);
        let t_sem = TemporalSemantics {
            observed_at_ms: node.observed_at_ms,
            published_at: None,
            published_at_ms: None,
            modified_at: None,
            modified_at_ms: None,
            valid_from: Some("2026-01-01T00:00:00Z".to_owned()),
            valid_until: Some("2026-12-31T23:59:59Z".to_owned()),
        };
        let md = format!(
            "# Enterprise Service Plan {doc_i} (2026)\n\n\
             Configure `--max-requests` and `BurstRps` quota for cluster {doc_i}.\n\n\
             | Tier | Monthly Rate | MaxPayloadMiB |\n\
             | --- | --- | --- |\n\
             | Pro | ${} | 64 |\n\
             | Enterprise | ${} | 256 |\n\n\
             Enterprise rate excludes regional VAT unless billed annually.\n",
            49 + (doc_i % 5),
            149 + (doc_i % 7)
        );
        let root = format!("source-{}.example", doc_i % 8);
        let units = extract_units_from_document(&UnitBuildInput {
            doc_node: &node,
            markdown: &md,
            subject_key: None,
            temporal: Some(&t_sem),
            blocks: &[],
            claims: &[],
            evidence: &[],
            origin_root: &root,
            origin_independent: doc_i % 4 != 3,
            max_table_unit_tokens: 512,
        });
        all_units.extend(units);
    }
    index.apply_updates(&all_units, &[], &spec);

    let conflict = ConflictRecord {
        conflict_id: "cf_bench".to_owned(),
        subject_key: "https://source-0.example/docs/pricing".to_owned(),
        field_path: "/price".to_owned(),
        normalized_field: "price".to_owned(),
        kind: ConflictKind::ValueMismatch,
        severity: ConflictSeverity::High,
        variants: vec![
            ConflictVariant {
                value: NormalizedValue::Money {
                    amount_e4: 1_490_000,
                    currency: "USD".to_owned(),
                    period: MoneyPeriod::Monthly,
                    raw: "$149".to_owned(),
                },
                raw_values: vec![serde_json::json!("$149")],
                support_count: 1,
                independent_source_count: 1,
                latest_observed_at_ms: 1_770_000_000_000,
                sources: vec!["https://source-0.example/docs/pricing".to_owned()],
                formats: vec!["html".to_owned()],
                engines: vec!["http".to_owned()],
                evidence: vec![EvidenceSpan {
                    doc_id: "doc_0".to_owned(),
                    url: "https://source-0.example/docs/pricing".to_owned(),
                    block_id: None,
                    chunk_idx: None,
                    start: None,
                    end: None,
                    excerpt: "$149".to_owned(),
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
                    amount_e4: 1_500_000,
                    currency: "USD".to_owned(),
                    period: MoneyPeriod::Monthly,
                    raw: "$150".to_owned(),
                },
                raw_values: vec![serde_json::json!("$150")],
                support_count: 1,
                independent_source_count: 1,
                latest_observed_at_ms: 1_770_000_001_000,
                sources: vec!["https://source-1.example/docs/pricing".to_owned()],
                formats: vec!["html".to_owned()],
                engines: vec!["http".to_owned()],
                evidence: vec![EvidenceSpan {
                    doc_id: "doc_1".to_owned(),
                    url: "https://source-1.example/docs/pricing".to_owned(),
                    block_id: None,
                    chunk_idx: None,
                    start: None,
                    end: None,
                    excerpt: "$150".to_owned(),
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

    let sreq = ContextSearchRequest {
        query: "Enterprise monthly rate --max-requests BurstRps MaxPayloadMiB unless".to_owned(),
        effective_at_ms: parse_timestamp_ms("2026-06-15T00:00:00Z"),
        limit: 16,
        ..Default::default()
    };
    let preq = ContextPackRequest {
        search: sreq.clone(),
        budget_tokens: 384,
        selector: PackSelector::SubmodularClosure,
        ordering: PackOrdering::EdgeSalience,
        ..Default::default()
    };
    let mut learn = ContextLearnState::default();
    for _ in 0..20 {
        let rep = index.search(
            &sreq,
            std::slice::from_ref(&conflict),
            &[],
            Vec::new(),
            &mut learn,
        );
        std::hint::black_box(compile_context_pack(&preq, &rep, 0));
    }

    let iters_sp = 500usize;
    let ts0 = Instant::now();
    let mut last_rep = None;
    for _ in 0..iters_sp {
        let rep = index.search(
            &sreq,
            std::slice::from_ref(&conflict),
            &[],
            Vec::new(),
            &mut learn,
        );
        last_rep = Some(rep);
    }
    let search_us = ts0.elapsed().as_secs_f64() * 1e6 / (iters_sp as f64);
    let rep = last_rep.unwrap();

    let tp0 = Instant::now();
    for _ in 0..iters_sp {
        std::hint::black_box(compile_context_pack(&preq, &rep, 0));
    }
    let pack_submod_us = tp0.elapsed().as_secs_f64() * 1e6 / (iters_sp as f64);

    let preq_dpp = ContextPackRequest {
        selector: PackSelector::BoundedDpp,
        ..preq
    };
    let td0 = Instant::now();
    for _ in 0..iters_sp {
        std::hint::black_box(compile_context_pack(&preq_dpp, &rep, 0));
    }
    let pack_dpp_us = td0.elapsed().as_secs_f64() * 1e6 / (iters_sp as f64);

    println!(
        "HOTPATH_BENCH analyze_us={analyze_us:.2} match_uot_us={match_us:.2} search_240u_us={search_us:.2} pack_submod_us={pack_submod_us:.2} pack_dpp_us={pack_dpp_us:.2}"
    );
}
