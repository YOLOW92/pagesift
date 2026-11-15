use super::*;
use crate::experience::FamilyObservation;

#[test]
fn strategy_prefers_conditional_http_when_validators_exist() {
    let input = StrategyInput {
        url: "https://example.com/pricing".to_string(),
        has_validators: true,
        has_embedded_json_hint: false,
        is_hub_or_seed: false,
        importance_weight: 1.0,
        staleness_ratio: 1.2,
        remaining_budget_fraction: 0.8,
        allow_browser: true,
    };
    let dec = select_acquisition_strategy(&input, None);
    assert_eq!(dec.strategy, AcquisitionStrategy::ConditionalHttp);
}

#[test]
fn strategy_escalates_to_direct_browser_when_family_learns_http_insufficient() {
    let mut stats = FamilyStats::new("https://spa.example.com/app");
    for i in 0..6 {
        stats.observe(&FamilyObservation {
            observed_at_ms: 1000 + i * 100,
            http_sufficient: Some(false),
            browser_gain: Some(true),
            conditional_hit: None,
            embedded_json_observed: Some(false),
            substantive_change: None,
            yielded_claims_or_links: Some(true),
            encountered_barrier: Some(false),
            cost_ms: 600.0,
        });
    }
    let input = StrategyInput {
        url: "https://spa.example.com/app/dashboard".to_string(),
        has_validators: false,
        has_embedded_json_hint: false,
        is_hub_or_seed: false,
        importance_weight: 1.0,
        staleness_ratio: 1.0,
        remaining_budget_fraction: 0.9,
        allow_browser: true,
    };
    let dec = select_acquisition_strategy(&input, Some(&stats));
    assert_eq!(dec.strategy, AcquisitionStrategy::DirectBrowser);
}

#[test]
fn strategy_selects_embedded_json_from_learned_family_history() {
    let mut stats = FamilyStats::new("https://shop.example.com/p");
    for i in 0..4 {
        stats.observe(&FamilyObservation {
            observed_at_ms: 100 + i * 50,
            http_sufficient: Some(true),
            browser_gain: None,
            conditional_hit: None,
            embedded_json_observed: Some(true),
            substantive_change: Some((600.0, false)),
            yielded_claims_or_links: Some(true),
            encountered_barrier: Some(false),
            cost_ms: 35.0,
        });
    }
    let input = StrategyInput {
        url: "https://shop.example.com/p/42".to_string(),
        has_validators: false,
        has_embedded_json_hint: false,
        is_hub_or_seed: false,
        importance_weight: 1.0,
        staleness_ratio: 1.0,
        remaining_budget_fraction: 0.8,
        allow_browser: true,
    };
    let dec = select_acquisition_strategy(&input, Some(&stats));
    assert_eq!(dec.strategy, AcquisitionStrategy::HttpWithEmbeddedJson);
}

#[test]
fn strategy_avoids_permanent_starvation_for_very_stale_high_barrier_family() {
    let mut stats = FamilyStats::new("https://strict.example.com/item");
    for i in 0..6 {
        stats.observe(&FamilyObservation {
            observed_at_ms: 100 + i * 100,
            http_sufficient: None,
            browser_gain: None,
            conditional_hit: None,
            embedded_json_observed: None,
            substantive_change: None,
            yielded_claims_or_links: None,
            encountered_barrier: Some(true),
            cost_ms: 40.0,
        });
    }
    let normal_stale = StrategyInput {
        url: "https://strict.example.com/item/1".to_string(),
        has_validators: false,
        has_embedded_json_hint: false,
        is_hub_or_seed: false,
        importance_weight: 1.0,
        staleness_ratio: 1.2,
        remaining_budget_fraction: 0.2,
        allow_browser: true,
    };
    assert_eq!(
        select_acquisition_strategy(&normal_stale, Some(&stats)).strategy,
        AcquisitionStrategy::DeferOrSkip
    );

    let overdue = StrategyInput {
        staleness_ratio: 3.5,
        ..normal_stale
    };
    assert_ne!(
        select_acquisition_strategy(&overdue, Some(&stats)).strategy,
        AcquisitionStrategy::DeferOrSkip
    );
}

#[test]
fn capture_sufficiency_enforces_per_field_grounded_evidence_c14() {
    let goal = CaptureGoal {
        min_markdown_chars: 50,
        required_fields: vec!["price".to_string(), "sku".to_string()],
        require_evidence_grounded: true,
        expect_links_for_hub: false,
    };

    // 1. 缺字段
    let missing_res = evaluate_capture_sufficiency(
        &goal,
        &CaptureSignals {
            status_code: 200,
            markdown_chars: 120,
            has_client_shell_markers: true,
            extracted_fields_present: vec!["price".to_string()],
            grounded_fields_present: vec!["price".to_string()],
            grounded_evidence_count: 1,
            outgoing_links_count: 2,
            ..Default::default()
        },
    );
    assert!(!missing_res.sufficient);
    assert!(missing_res.needs_browser_escalation);
    assert_eq!(missing_res.missing_fields, vec!["sku".to_string()]);

    // 2. 字段全提取，但 zero evidence（C14 最小反例）
    let zero_ev = evaluate_capture_sufficiency(
        &goal,
        &CaptureSignals {
            status_code: 200,
            markdown_chars: 120,
            extracted_fields_present: vec!["price".to_string(), "sku".to_string()],
            grounded_fields_present: Vec::new(),
            grounded_evidence_count: 0,
            unaligned_claim_count: 0,
            ..Default::default()
        },
    );
    assert!(!zero_ev.sufficient);
    assert_eq!(zero_ev.ungrounded_fields, vec!["price", "sku"]);

    // 3. 仅有无关证据（author 有证据，但 price/sku 没有）
    let unrelated_ev = evaluate_capture_sufficiency(
        &goal,
        &CaptureSignals {
            status_code: 200,
            markdown_chars: 120,
            extracted_fields_present: vec!["price".to_string(), "sku".to_string()],
            grounded_fields_present: vec!["author".to_string()],
            grounded_evidence_count: 2,
            ..Default::default()
        },
    );
    assert!(!unrelated_ev.sufficient);
    assert_eq!(unrelated_ev.ungrounded_fields, vec!["price", "sku"]);

    // 4. 部分字段 grounded，仍不足
    let partial_ev = evaluate_capture_sufficiency(
        &goal,
        &CaptureSignals {
            status_code: 200,
            markdown_chars: 120,
            extracted_fields_present: vec!["price".to_string(), "sku".to_string()],
            grounded_fields_present: vec!["price".to_string()],
            grounded_evidence_count: 1,
            ..Default::default()
        },
    );
    assert!(!partial_ev.sufficient);
    assert_eq!(partial_ev.ungrounded_fields, vec!["sku".to_string()]);

    // 5. 值与 evidence 不一致（unaligned_claim_count > 0）
    let misaligned = evaluate_capture_sufficiency(
        &goal,
        &CaptureSignals {
            status_code: 200,
            markdown_chars: 120,
            extracted_fields_present: vec!["price".to_string(), "sku".to_string()],
            grounded_fields_present: vec!["price".to_string(), "sku".to_string()],
            grounded_evidence_count: 2,
            unaligned_claim_count: 1,
            ..Default::default()
        },
    );
    assert!(!misaligned.sufficient);

    // 6. 304 若未匹配当前契约 baseline 不算充足
    let mismatched_304 = evaluate_capture_sufficiency(
        &goal,
        &CaptureSignals {
            status_code: 304,
            not_modified_304: true,
            baseline_matches_contract: false,
            ..Default::default()
        },
    );
    assert!(!mismatched_304.sufficient);

    // 7. 全部字段提取且逐一接地对齐 -> sufficient
    let ok = evaluate_capture_sufficiency(
        &goal,
        &CaptureSignals {
            status_code: 200,
            markdown_chars: 120,
            extracted_fields_present: vec!["price".to_string(), "sku".to_string()],
            grounded_fields_present: vec!["price".to_string(), "sku".to_string()],
            grounded_evidence_count: 2,
            unaligned_claim_count: 0,
            ..Default::default()
        },
    );
    assert!(ok.sufficient);
    assert!(ok.ungrounded_fields.is_empty());
}

#[test]
fn coverage_status_distinguishes_unchecked_and_access_restricted() {
    // 发现 100 条新链但未检查 -> 不能报告 CompleteWithinScope
    let unchecked = evaluate_coverage_status(&CoverageInput {
        discovered_total: 101,
        authoritative_successes: 1,
        attempted_total: 1,
        unchecked_total: 100,
        ..Default::default()
    });
    assert_eq!(unchecked.status, CoverageStatus::PartialWithGaps);

    // 全部 403/429（尝试 3 次，权威成功 0 次）-> AccessRestricted
    let restricted = evaluate_coverage_status(&CoverageInput {
        discovered_total: 3,
        authoritative_successes: 0,
        attempted_total: 3,
        access_restricted_count: 3,
        ..Default::default()
    });
    assert_eq!(restricted.status, CoverageStatus::AccessRestricted);
}
