//! PageSift 1.7 (`Resolve`) reproducible scientific evaluation, strong baselines, ablations,
//! Live incremental-vs-full validation, conservative calibration drift test, and performance suite.
//!
//! Run: `cargo run --profile release-fast -p pagesift-engine --example resolve_eval`

use std::collections::{BTreeMap, BTreeSet};
use std::sync::Arc;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::time::{Duration, Instant};

use axum::Router;
use axum::response::Html;
use axum::routing::get;
use pagesift_api::Engine;
use pagesift_engine::{Scraper, ScraperConfig};
use pagesift_extract::context::{ContextIndex, UnitBuildInput, extract_units_from_document};
use pagesift_extract::resolve::{
    ResolveLearnState, evaluate_calibration_metrics, plan_counter_probes, run_local_counter_probes,
    verify_answer_deterministic,
};
use pagesift_graph::context::AssetSpecStamp;
use pagesift_graph::resolve::{
    CounterProbeKind, ProofStandard, ResolutionState, ResolveCheckRequest, ResolveLearnFeedback,
    ScopeDomainKind,
};
use pagesift_graph::{DocNode, SourceKind, TemporalSemantics, sha256_hex};
use pagesift_stat::rng::Rng;
use pagesift_store::Store;
use serde::Serialize;
use serde_json::json;

const FIXED_SEED: u64 = 0x17_2026_1123;

#[derive(Serialize)]
struct ResolveEvalReport {
    version: &'static str,
    seed: u64,
    hardware: HardwareInfo,
    e1_claim_verification: Vec<SystemEvalRow>,
    e2_origin_collapse: OriginCollapseMetrics,
    e3_temporal_and_causal: TemporalCausalMetrics,
    e4_counter_and_scope: CounterScopeMetrics,
    e5_gap_acquisition: GapAcqMetrics,
    e6_live_reconciliation: LiveReconcileMetrics,
    e7_conservative_learn: LearnEvalMetrics,
    e8_runtime_perf: RuntimePerfMetrics,
    e9_honest_limitations: HonestLimitationsReport,
}

#[derive(Serialize)]
struct HardwareInfo {
    logical_cores: usize,
    os_arch: &'static str,
    peak_rss_mib: f64,
}

#[derive(Serialize)]
struct SystemEvalRow {
    system: &'static str,
    total_cases: usize,
    false_support_rate: f64,
    supported_precision: f64,
    supported_recall: f64,
    refuted_recall: f64,
    citation_accuracy: f64,
    abstention_rate: f64,
}

#[derive(Serialize)]
struct OriginCollapseMetrics {
    scenarios: usize,
    naive_no_collapse_false_support_rate: f64,
    resolve_collapsed_false_support_rate: f64,
    true_multi_source_recall: f64,
}

#[derive(Serialize)]
struct TemporalCausalMetrics {
    as_of_cases: usize,
    future_leakage_count: usize,
    stale_benchmark_cases: usize,
    baseline_stale_false_support_rate: f64,
    resolve_stale_false_support_rate: f64,
    causal_cases: usize,
    causal_overclaim_blocked_rate: f64,
    rct_identified_causal_recall: f64,
}

#[derive(Serialize)]
struct CounterScopeMetrics {
    adversarial_cases: usize,
    no_counter_false_support_rate: f64,
    with_counter_false_support_rate: f64,
    counterexample_recall: f64,
    open_world_negative_existential_false_support: usize,
    absence_not_proof_compliance_rate: f64,
}

#[derive(Serialize)]
struct GapAcqMetrics {
    permission_off_network_requests: usize,
    disallowed_host_network_requests: usize,
    authorized_network_requests: usize,
    resolved_before_gap: bool,
    resolved_after_gap: bool,
}

#[derive(Serialize)]
struct LiveReconcileMetrics {
    persisted_conclusions: usize,
    relocated_early_cutoff_false_triggers: usize,
    relocated_early_cutoff_saved_count: usize,
    substantive_impact_recall: f64,
    false_trigger_rate: f64,
    scope_new_competitor_impact_recall: f64,
    observation_lost_false_refutations: usize,
    aba_fallback_triggered: bool,
    incremental_recheck_micros: u64,
    full_recheck_micros: u64,
    incremental_speedup_ratio: f64,
    full_vs_incremental_agreement: f64,
}

#[derive(Serialize)]
struct LearnEvalMetrics {
    cold_start_abstained: bool,
    weak_signals_submitted: usize,
    weak_signals_accepted: usize,
    hard_samples_trained: usize,
    brier_before: f64,
    brier_after: f64,
    brier_ci_low: f64,
    brier_ci_high: f64,
    drift_detection_steps: usize,
    verifier_version_bump_reset: bool,
}

#[derive(Serialize)]
struct RuntimePerfMetrics {
    checks_measured: usize,
    qps: f64,
    p50_us: u64,
    p95_us: u64,
    p99_us: u64,
    peak_rss_mib: f64,
}

#[derive(Serialize)]
struct HonestLimitationsReport {
    paraphrase_cases_tested: usize,
    deterministic_honest_abstention_count: usize,
    deterministic_false_support_count: usize,
    note: &'static str,
}

struct EvalCase {
    answer: String,
    expected_group: &'static str, // "supported" | "refuted" | "abstain"
    cited_url_valid: bool,
}

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
        engine: "eval".into(),
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

fn read_peak_rss_mib() -> f64 {
    let Ok(status) = std::fs::read_to_string("/proc/self/status") else {
        return 0.0;
    };
    for line in status.lines() {
        if let Some(rest) = line.strip_prefix("VmHWM:") {
            let kb: f64 = rest
                .split_whitespace()
                .next()
                .and_then(|s| s.parse().ok())
                .unwrap_or(0.0);
            return (kb / 1024.0 * 100.0).round() / 100.0;
        }
    }
    0.0
}

fn keyword_overlap(a: &str, b: &str) -> f64 {
    let tok = |s: &str| -> BTreeSet<String> {
        s.to_ascii_lowercase()
            .split(|c: char| !c.is_alphanumeric())
            .filter(|w| w.len() >= 3)
            .map(str::to_owned)
            .collect()
    };
    let ta = tok(a);
    let tb = tok(b);
    if ta.is_empty() {
        return 0.0;
    }
    ta.intersection(&tb).count() as f64 / ta.len() as f64
}

/// Baseline-A: naive citation-presence + keyword-overlap checker (standard RAG verifier).
fn baseline_a_predict(answer: &str, idx: &ContextIndex) -> &'static str {
    let mut best = 0.0f64;
    for u in idx.units.values() {
        best = best.max(keyword_overlap(answer, &u.text));
    }
    if best >= 0.45 { "supported" } else { "abstain" }
}

/// Baseline-B: literal whole-substring attribution mirroring `agent/provenance.rs`.
fn baseline_b_predict(answer: &str, idx: &ContextIndex) -> &'static str {
    let numbers: Vec<&str> = answer
        .split_whitespace()
        .filter(|w| w.chars().any(|c| c.is_ascii_digit()))
        .map(|w| w.trim_matches(|c: char| !c.is_alphanumeric() && c != '$' && c != '.'))
        .filter(|w| !w.is_empty())
        .collect();
    for u in idx.units.values() {
        let low = u.text.to_ascii_lowercase();
        let num_hit = numbers.is_empty()
            || numbers
                .iter()
                .all(|n| low.contains(&n.to_ascii_lowercase()));
        if num_hit && keyword_overlap(answer, &u.text) >= 0.35 {
            return "supported";
        }
    }
    "abstain"
}

fn eval_e1() -> Vec<SystemEvalRow> {
    let mut idx = ContextIndex::default();
    let ts = 1_770_000_000_000u64;
    for i in 0..8 {
        let price = 49 + i * 10;
        let lite = 19 + i * 5;
        add_doc(
            &mut idx,
            &format!("https://vendor-{i}.example/pricing"),
            &format!(
                "# Vendor{i} Pricing\n\n| Plan | Annual Price |\n|---|---|\n| SuitePro{i} | ${price} |\n| SuiteLite{i} | ${lite} |\n\nSuitePro{i} includes SSO only for Enterprise accounts unless waived.\n"
            ),
            &format!("vendor-{i}.example"),
            true,
            ts,
            None,
        );
        add_doc(
            &mut idx,
            &format!("https://vendor-{i}.example/security"),
            &format!(
                "Vault{i} does not support unencrypted HTTP connections. Vault{i} enforces TLS 1.3 encryption at 128 MB max frame size."
            ),
            &format!("vendor-{i}.example"),
            true,
            ts,
            None,
        );
    }

    let mut cases: Vec<EvalCase> = Vec::new();
    for i in 0..8 {
        let price = 49 + i * 10;
        // 1. Grounded supported annual price
        cases.push(EvalCase {
            answer: format!("SuitePro{i} costs ${price}/yr [https://vendor-{i}.example/pricing]."),
            expected_group: "supported",
            cited_url_valid: true,
        });
        // 2. Grounded supported TLS 1.3 fact
        cases.push(EvalCase {
            answer: format!("Vault{i} enforces TLS 1.3 encryption at 128 MB max frame size."),
            expected_group: "supported",
            cited_url_valid: true,
        });
        // 3. Billing-period table trap ($price/mo vs Annual Price) => MUST be refuted!
        cases.push(EvalCase {
            answer: format!("SuitePro{i} costs ${price}/mo."),
            expected_group: "refuted",
            cited_url_valid: true,
        });
        // 4. Currency mismatch (EUR vs USD) => MUST be refuted!
        cases.push(EvalCase {
            answer: format!("SuitePro{i} costs €{price}/yr."),
            expected_group: "refuted",
            cited_url_valid: true,
        });
        // 5. Negation polarity flip ("Vault supports unencrypted HTTP") => MUST be refuted!
        cases.push(EvalCase {
            answer: format!("Vault{i} supports unencrypted HTTP connections."),
            expected_group: "refuted",
            cited_url_valid: true,
        });
        // 6. Qualifier omission ("SuitePro includes SSO" dropping "only for Enterprise unless waived") => abstain/qualified, not unconditional supported
        cases.push(EvalCase {
            answer: format!("SuitePro{i} includes SSO."),
            expected_group: "abstain",
            cited_url_valid: true,
        });
        // 7. Fabricated / inaccessible citation => MUST NOT be supported!
        cases.push(EvalCase {
            answer: format!(
                "PhantomProduct{i} costs $15/mo [https://fake-{i}.invalid/nonexistent]."
            ),
            expected_group: "abstain",
            cited_url_valid: false,
        });
        // 8. Superlative refuted by sibling row SuiteLite => MUST be refuted!
        cases.push(EvalCase {
            answer: format!("SuitePro{i} is the cheapest plan at ${price}/yr."),
            expected_group: "refuted",
            cited_url_valid: true,
        });
    }

    let score_system = |name: &'static str, mode: u8| -> SystemEvalRow {
        let mut tp_sup = 0usize;
        let mut fp_sup = 0usize;
        let mut fn_sup = 0usize;
        let mut non_sup_total = 0usize;
        let mut ref_total = 0usize;
        let mut ref_hit = 0usize;
        let mut abstained = 0usize;
        let mut cite_ok = 0usize;
        let mut learn = ResolveLearnState::default();

        for c in &cases {
            let (pred, cite_acc_ok) = match mode {
                0 => (baseline_a_predict(&c.answer, &idx), c.cited_url_valid),
                1 => (baseline_b_predict(&c.answer, &idx), c.cited_url_valid),
                _ => {
                    let req = ResolveCheckRequest {
                        answer: c.answer.clone(),
                        enable_counter_search: mode == 3,
                        ..ResolveCheckRequest::default()
                    };
                    let mut rec = verify_answer_deterministic(&req, &idx, &[], &mut learn, ts);
                    if mode == 3 {
                        let plans = plan_counter_probes(&rec, &learn);
                        let _ = run_local_counter_probes(&mut rec, &plans, &idx, None, false);
                    }
                    let pred = match rec.state {
                        ResolutionState::Supported => "supported",
                        ResolutionState::Refuted | ResolutionState::Conflicted => "refuted",
                        _ => "abstain",
                    };
                    let c_ok = if c.cited_url_valid {
                        true
                    } else {
                        !rec.claims.iter().any(|v| v.citation_accessible)
                    };
                    (pred, c_ok)
                }
            };
            if cite_acc_ok {
                cite_ok += 1;
            }
            if pred == "abstain" {
                abstained += 1;
            }
            match c.expected_group {
                "supported" => {
                    if pred == "supported" {
                        tp_sup += 1;
                    } else {
                        fn_sup += 1;
                    }
                }
                "refuted" => {
                    non_sup_total += 1;
                    ref_total += 1;
                    if pred == "supported" {
                        fp_sup += 1;
                    } else if pred == "refuted" {
                        ref_hit += 1;
                    }
                }
                _ => {
                    non_sup_total += 1;
                    if pred == "supported" {
                        fp_sup += 1;
                    }
                }
            }
        }

        let r4 = |x: f64| (x * 10_000.0).round() / 10_000.0;
        SystemEvalRow {
            system: name,
            total_cases: cases.len(),
            false_support_rate: r4(fp_sup as f64 / non_sup_total.max(1) as f64),
            supported_precision: r4(tp_sup as f64 / (tp_sup + fp_sup).max(1) as f64),
            supported_recall: r4(tp_sup as f64 / (tp_sup + fn_sup).max(1) as f64),
            refuted_recall: r4(ref_hit as f64 / ref_total.max(1) as f64),
            citation_accuracy: r4(cite_ok as f64 / cases.len().max(1) as f64),
            abstention_rate: r4(abstained as f64 / cases.len().max(1) as f64),
        }
    };

    vec![
        score_system("baseline_a_keyword_overlap", 0),
        score_system("baseline_b_literal_substring", 1),
        score_system("baseline_c_resolve_no_counter", 2),
        score_system("full_1_7_resolve", 3),
    ]
}

fn eval_e2() -> OriginCollapseMetrics {
    let ts = 1_770_000_000_000u64;
    let mut naive_fp = 0usize;
    let mut resolve_fp = 0usize;
    let mut true_multi_hit = 0usize;
    let n = 20usize;
    let mut learn = ResolveLearnState::default();

    for i in 0..n {
        // Mirror scenario: 3 articles repeating same vendor origin_root
        let mut mirror_idx = ContextIndex::default();
        add_doc(
            &mut mirror_idx,
            &format!("https://vendor-{i}.example/blog"),
            &format!("ApexChip{i} reaches 120 TOPS throughput."),
            &format!("vendor-{i}.example"),
            true,
            ts,
            None,
        );
        for m in 0..2 {
            add_doc(
                &mut mirror_idx,
                &format!("https://syndicate-{m}-{i}.example/reprint"),
                &format!("According to press release, ApexChip{i} reaches 120 TOPS throughput."),
                &format!("vendor-{i}.example"),
                false,
                ts,
                None,
            );
        }
        // Naive checker counts distinct URLs (>= 2) as ClearAndConvincing => false support!
        if mirror_idx.units.len() >= 2 {
            naive_fp += 1;
        }
        let req = ResolveCheckRequest {
            answer: format!("ApexChip{i} reaches 120 TOPS throughput."),
            proof_standard: ProofStandard::ClearAndConvincing,
            ..ResolveCheckRequest::default()
        };
        let c = verify_answer_deterministic(&req, &mirror_idx, &[], &mut learn, ts);
        if c.state == ResolutionState::Supported {
            resolve_fp += 1;
        }

        // True independent multi-source scenario: vendor + independent benchmark lab
        let mut indep_idx = mirror_idx;
        add_doc(
            &mut indep_idx,
            &format!("https://indep-lab-{i}.org/report"),
            &format!("Independent benchmark confirmed ApexChip{i} reaches 120 TOPS throughput."),
            &format!("indep-lab-{i}.org"),
            true,
            ts,
            None,
        );
        let c_indep = verify_answer_deterministic(&req, &indep_idx, &[], &mut learn, ts);
        if c_indep.state == ResolutionState::Supported {
            true_multi_hit += 1;
        }
    }

    OriginCollapseMetrics {
        scenarios: n,
        naive_no_collapse_false_support_rate: naive_fp as f64 / n as f64,
        resolve_collapsed_false_support_rate: resolve_fp as f64 / n as f64,
        true_multi_source_recall: true_multi_hit as f64 / n as f64,
    }
}

fn eval_e3() -> TemporalCausalMetrics {
    let mut learn = ResolveLearnState::default();
    let t_past = 1_700_000_000_000u64; // 2023-11
    let t_future = 1_780_000_000_000u64; // 2026-05

    // 1. asOf non-leakage (12 cases where evidence was observed AFTER asOf)
    let mut future_leakage_count = 0usize;
    for i in 0..12 {
        let mut idx = ContextIndex::default();
        add_doc(
            &mut idx,
            &format!("https://news-{i}.example/2026"),
            &format!("Helix{i} released version 4.0."),
            &format!("news-{i}.example"),
            true,
            t_future,
            None,
        );
        let req = ResolveCheckRequest {
            answer: format!("Helix{i} released version 4.0."),
            as_of_ms: Some(t_past),
            ..ResolveCheckRequest::default()
        };
        let c = verify_answer_deterministic(&req, &idx, &[], &mut learn, t_future);
        if c.state.is_supported_like() || !c.dependent_unit_ids.is_empty() {
            future_leakage_count += 1;
        }
    }

    // 2. Stale 2024 benchmark used for 2026 present-tense claim
    let mut baseline_stale_fp = 0usize;
    let mut resolve_stale_fp = 0usize;
    let stale_cases = 12usize;
    for i in 0..stale_cases {
        let mut idx = ContextIndex::default();
        add_doc(
            &mut idx,
            &format!("https://bench-{i}.example/2024"),
            &format!("In 2024 benchmark tests, FluxEngine{i} ranked #1 in latency."),
            &format!("bench-{i}.example"),
            true,
            t_future,
            Some("2024-12-31"),
        );
        let ans = format!(
            "In benchmark tests, FluxEngine{i} ranked #1 in latency and still leads in 2026."
        );
        if baseline_a_predict(&ans, &idx) == "supported" {
            baseline_stale_fp += 1;
        }
        let req = ResolveCheckRequest {
            answer: ans,
            effective_at_ms: Some(t_future),
            ..ResolveCheckRequest::default()
        };
        let c = verify_answer_deterministic(&req, &idx, &[], &mut learn, t_future);
        if c.state.is_supported_like() {
            resolve_stale_fp += 1;
        }
    }

    // 3. Causal conservatism: 12 correlational vs 12 RCT/controlled-experiment cases
    let causal_cases = 12usize;
    let mut causal_blocked = 0usize;
    let mut rct_supported = 0usize;
    for i in 0..causal_cases {
        let mut corr_idx = ContextIndex::default();
        add_doc(
            &mut corr_idx,
            &format!("https://obs-{i}.example/study"),
            &format!("Higher cache misses{i} were correlated with higher checkout latency."),
            &format!("obs-{i}.example"),
            true,
            t_future,
            None,
        );
        let req = ResolveCheckRequest {
            answer: format!("Higher cache misses{i} caused higher checkout latency."),
            ..ResolveCheckRequest::default()
        };
        let c_corr = verify_answer_deterministic(&req, &corr_idx, &[], &mut learn, t_future);
        if c_corr.state == ResolutionState::CausalUnidentifiable {
            causal_blocked += 1;
        }

        let mut rct_idx = ContextIndex::default();
        add_doc(
            &mut rct_idx,
            &format!("https://rct-{i}.example/study"),
            &format!(
                "A controlled experiment confirmed higher cache misses{i} had a direct causal effect increasing checkout latency."
            ),
            &format!("rct-{i}.example"),
            true,
            t_future,
            None,
        );
        let c_rct = verify_answer_deterministic(&req, &rct_idx, &[], &mut learn, t_future);
        if c_rct.state == ResolutionState::Supported {
            rct_supported += 1;
        }
    }

    TemporalCausalMetrics {
        as_of_cases: 12,
        future_leakage_count,
        stale_benchmark_cases: stale_cases,
        baseline_stale_false_support_rate: baseline_stale_fp as f64 / stale_cases as f64,
        resolve_stale_false_support_rate: resolve_stale_fp as f64 / stale_cases as f64,
        causal_cases,
        causal_overclaim_blocked_rate: causal_blocked as f64 / causal_cases as f64,
        rct_identified_causal_recall: rct_supported as f64 / causal_cases as f64,
    }
}

fn eval_e4() -> CounterScopeMetrics {
    let ts = 1_770_000_000_000u64;
    let mut learn = ResolveLearnState::default();
    let n = 20usize;
    let mut no_counter_fp = 0usize;
    let mut with_counter_fp = 0usize;
    let mut counter_recall_hits = 0usize;

    for i in 0..n {
        let mut idx = ContextIndex::default();
        add_doc(
            &mut idx,
            &format!("https://cloud-{i}.example/docs"),
            &format!("Nimbus{i} supports instant cross-region failover."),
            &format!("cloud-{i}.example"),
            true,
            ts,
            None,
        );
        add_doc(
            &mut idx,
            &format!("https://cloud-{i}.example/errata"),
            &format!("Errata: Nimbus{i} failover capability deprecated and superseded by paid v3."),
            &format!("cloud-{i}.example"),
            true,
            ts + 1_000,
            None,
        );
        let req = ResolveCheckRequest {
            answer: format!("Nimbus{i} supports instant cross-region failover."),
            ..ResolveCheckRequest::default()
        };
        let mut c = verify_answer_deterministic(&req, &idx, &[], &mut learn, ts + 2_000);
        if c.state == ResolutionState::Supported {
            no_counter_fp += 1;
        }
        let plans = plan_counter_probes(&c, &learn);
        let audit = run_local_counter_probes(&mut c, &plans, &idx, None, false);
        if audit.counter_evidence_found > 0 {
            counter_recall_hits += 1;
        }
        if c.state == ResolutionState::Supported {
            with_counter_fp += 1;
        }
    }

    // Open-world negative existential & absence-is-not-proof compliance
    let mut ow_fp = 0usize;
    let mut compliance_ok = 0usize;
    let ow_cases = 10usize;
    for i in 0..ow_cases {
        let mut clean = ContextIndex::default();
        add_doc(
            &mut clean,
            &format!("https://clean-{i}.example/about"),
            &format!("Kestrel{i} uses Rust memory safety."),
            &format!("clean-{i}.example"),
            true,
            ts,
            None,
        );
        let req_neg = ResolveCheckRequest {
            answer: format!("There is no public record of Kestrel{i} security breaches."),
            domain_kind: ScopeDomainKind::OpenWorld,
            ..ResolveCheckRequest::default()
        };
        let c_neg = verify_answer_deterministic(&req_neg, &clean, &[], &mut learn, ts);
        if c_neg.state == ResolutionState::Supported {
            ow_fp += 1;
        }

        let req_ok = ResolveCheckRequest {
            answer: format!("Kestrel{i} uses Rust memory safety."),
            domain_kind: ScopeDomainKind::OpenWorld,
            ..ResolveCheckRequest::default()
        };
        let mut c_ok = verify_answer_deterministic(&req_ok, &clean, &[], &mut learn, ts);
        let plans = plan_counter_probes(&c_ok, &learn);
        let audit = run_local_counter_probes(&mut c_ok, &plans, &clean, None, false);
        if audit.coverage_statement.contains("absence_is_not_proof") {
            compliance_ok += 1;
        }
    }

    CounterScopeMetrics {
        adversarial_cases: n,
        no_counter_false_support_rate: no_counter_fp as f64 / n as f64,
        with_counter_false_support_rate: with_counter_fp as f64 / n as f64,
        counterexample_recall: counter_recall_hits as f64 / n as f64,
        open_world_negative_existential_false_support: ow_fp,
        absence_not_proof_compliance_rate: compliance_ok as f64 / ow_cases as f64,
    }
}

async fn eval_e5_gap_and_e6_live() -> (GapAcqMetrics, LiveReconcileMetrics) {
    // E5: Local deterministic HTTP server for bounded Gap acquisition
    let hits = Arc::new(AtomicUsize::new(0));
    let hits_c = hits.clone();
    let app = Router::new().route(
        "/spec",
        get(move || {
            let h = hits_c.clone();
            async move {
                h.fetch_add(1, Ordering::SeqCst);
                Html("<html><body><h1>Spec</h1><p>Quasar Pro costs $89/mo.</p></body></html>")
            }
        }),
    );
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    let url = format!("http://{addr}/spec");
    let srv = tokio::spawn(async move {
        let _ = axum::serve(listener, app).await;
    });

    let dir = tempfile::tempdir().unwrap();
    let mut sc_cfg = pagesift_store::Config::new(dir.path());
    sc_cfg.background = false;
    let store = Store::open(sc_cfg).unwrap();
    let mut cfg = ScraperConfig::default();
    cfg.fetcher.env_proxy = false;
    cfg.fetcher.ssrf.allow_private_network = true;
    cfg.policy.rate.default_interval = Duration::from_millis(1);
    cfg.store = Some(store);
    let scraper = Scraper::new(cfg).unwrap();

    let r_off = Engine::resolve(
        &scraper,
        None,
        json!({
            "action": "check",
            "sourceId": "gap-eval",
            "answer": "Quasar Pro costs $89/mo.",
            "candidateUrls": [url],
            "allowNetworkAcquisition": false
        }),
    )
    .await
    .unwrap();
    let perm_off_reqs = hits.load(Ordering::SeqCst);
    let resolved_before = r_off["outcome"]["conclusion"]["state"] == "supported";

    let _ = Engine::resolve(
        &scraper,
        None,
        json!({
            "action": "check",
            "sourceId": "gap-eval",
            "answer": "Quasar Pro costs $89/mo.",
            "candidateUrls": [url],
            "allowedHosts": ["blocked.example"],
            "allowNetworkAcquisition": true,
            "maxAcquisitionRequests": 2
        }),
    )
    .await
    .unwrap();
    let disallowed_reqs = hits.load(Ordering::SeqCst);

    let r_on = Engine::resolve(
        &scraper,
        None,
        json!({
            "action": "check",
            "sourceId": "gap-eval",
            "answer": "Quasar Pro costs $89/mo.",
            "candidateUrls": [url],
            "allowedHosts": ["127.0.0.1"],
            "allowNetworkAcquisition": true,
            "maxAcquisitionRequests": 2
        }),
    )
    .await
    .unwrap();
    let auth_reqs = hits.load(Ordering::SeqCst);
    let resolved_after = r_on["outcome"]["conclusion"]["state"] == "supported";

    let e5 = GapAcqMetrics {
        permission_off_network_requests: perm_off_reqs,
        disallowed_host_network_requests: disallowed_reqs,
        authorized_network_requests: auth_reqs,
        resolved_before_gap: resolved_before,
        resolved_after_gap: resolved_after,
    };

    // E6: Live incremental reconciliation vs full revalidation over 60 persisted conclusions
    let n_docs = 58usize;
    let mut initial_docs = Vec::with_capacity(n_docs);
    for i in 0..n_docs {
        initial_docs.push(json!({
            "url": format!("https://svc-{i}.example/pricing"),
            "originRoot": format!("svc-{i}.example"),
            "markdown": format!("# Service {i}\n\nService{i} Pro costs ${}/yr.\n", 100 + i)
        }));
    }
    let _ = Engine::context(
        &scraper,
        None,
        json!({
            "action": "ingest",
            "sourceId": "live-eval",
            "documents": initial_docs
        }),
    )
    .await
    .unwrap();

    for i in 0..n_docs {
        let _ = Engine::resolve(
            &scraper,
            None,
            json!({
                "action": "check",
                "sourceId": "live-eval",
                "conclusionId": format!("c_{i}"),
                "answer": format!("Service{i} Pro costs ${}/yr.", 100 + i),
                "persist": true,
                "enableCounterSearch": false
            }),
        )
        .await
        .unwrap();
    }
    // Add 1 superlative + 1 negative-existential conclusion (total 60 conclusions!)
    let _ = Engine::resolve(
        &scraper,
        None,
        json!({
            "action": "check",
            "sourceId": "live-eval",
            "conclusionId": "c_superlative",
            "domainKind": "closed_observed_set",
            "answer": "Service0 Pro is the cheapest plan at $100/yr.",
            "persist": true
        }),
    )
    .await
    .unwrap();
    let _ = Engine::resolve(
        &scraper,
        None,
        json!({
            "action": "check",
            "sourceId": "live-eval",
            "conclusionId": "c_neg_exist",
            "domainKind": "closed_observed_set",
            "answer": "There is no public record of Service0 security outages.",
            "persist": true
        }),
    )
    .await
    .unwrap();
    let _ = Engine::resolve(
        &scraper,
        None,
        json!({ "action": "reconcile", "sourceIds": ["live-eval"] }),
    )
    .await
    .unwrap();

    // 1. RelocatedOnly on 5 docs (0..5) -> Salsa Early Cutoff must NOT invalidate them!
    let mut reloc_docs = Vec::new();
    for i in 0..5 {
        reloc_docs.push(json!({
            "url": format!("https://svc-{i}.example/pricing"),
            "originRoot": format!("svc-{i}.example"),
            "markdown": format!("# Banner\n\nUpdated navigation layout.\n\n# Service {i}\n\nService{i} Pro costs ${}/yr.\n", 100 + i)
        }));
    }
    let _ = Engine::context(
        &scraper,
        None,
        json!({ "action": "ingest", "sourceId": "live-eval", "documents": reloc_docs }),
    )
    .await
    .unwrap();
    let rec_reloc = Engine::resolve(
        &scraper,
        None,
        json!({ "action": "reconcile", "sourceIds": ["live-eval"] }),
    )
    .await
    .unwrap();
    let reloc_impacted: BTreeSet<String> = rec_reloc["report"]["impactedConclusionIds"]
        .as_array()
        .unwrap()
        .iter()
        .filter_map(|v| v.as_str().map(str::to_owned))
        .collect();
    let reloc_false_triggers = (0..5)
        .filter(|i| reloc_impacted.contains(&format!("c_{i}")))
        .count();
    let reloc_saved = 5 - reloc_false_triggers;

    // 2. Substantive edit on 3 docs (10..13) + new competitor + outage report -> measure incremental vs full!
    let mut mod_docs = Vec::new();
    for i in 10..13 {
        mod_docs.push(json!({
            "url": format!("https://svc-{i}.example/pricing"),
            "originRoot": format!("svc-{i}.example"),
            "markdown": format!("# Service {i}\n\nService{i} Pro costs ${}/yr.\n", 250 + i)
        }));
    }
    mod_docs.push(json!({
        "url": "https://svc-new.example/pricing",
        "originRoot": "svc-new.example",
        "markdown": "# New Budget Tier\n\nServiceNano costs $15/yr.\n"
    }));
    mod_docs.push(json!({
        "url": "https://status.svc0.example/outage",
        "originRoot": "status.svc0.example",
        "markdown": "# Outage Disclosure\n\nPublic record of Service0 security outages confirmed.\n"
    }));
    let _ = Engine::context(
        &scraper,
        None,
        json!({ "action": "ingest", "sourceId": "live-eval", "documents": mod_docs }),
    )
    .await
    .unwrap();
    let rec_mod = Engine::resolve(
        &scraper,
        None,
        json!({ "action": "reconcile", "sourceIds": ["live-eval"] }),
    )
    .await
    .unwrap();
    let impacted_set: BTreeSet<String> = rec_mod["report"]["impactedConclusionIds"]
        .as_array()
        .unwrap()
        .iter()
        .filter_map(|v| v.as_str().map(str::to_owned))
        .collect();
    let scope_set: BTreeSet<String> = rec_mod["report"]["scopeImpactedConclusionIds"]
        .as_array()
        .unwrap()
        .iter()
        .filter_map(|v| v.as_str().map(str::to_owned))
        .collect();

    let expected_direct = ["c_10", "c_11", "c_12"];
    let direct_hits = expected_direct
        .iter()
        .filter(|id| impacted_set.contains(**id))
        .count();
    let scope_hits = ["c_superlative", "c_neg_exist"]
        .iter()
        .filter(|id| scope_set.contains(**id))
        .count();
    let untouched_total = n_docs - 3;
    let false_triggers = (0..n_docs)
        .filter(|i| !(10..13).contains(i) && impacted_set.contains(&format!("c_{i}")))
        .count();

    let t_inc = Instant::now();
    let _ = Engine::resolve(
        &scraper,
        None,
        json!({ "action": "revalidate", "sourceIds": ["live-eval"], "mode": "incremental" }),
    )
    .await
    .unwrap();
    let inc_us = t_inc.elapsed().as_micros() as u64;

    let inc_snapshot = list_conclusion_states(&scraper).await;

    let t_full = Instant::now();
    let _ = Engine::resolve(
        &scraper,
        None,
        json!({ "action": "revalidate", "sourceIds": ["live-eval"], "mode": "full" }),
    )
    .await
    .unwrap();
    let full_us = t_full.elapsed().as_micros() as u64;
    let full_snapshot = list_conclusion_states(&scraper).await;

    let agree_count = full_snapshot
        .iter()
        .filter(|(k, v)| inc_snapshot.get(*k) == Some(*v))
        .count();

    // 3. ObservationLost check on doc 20 -> must NOT mark Refuted!
    let _ = Engine::context(
        &scraper,
        None,
        json!({
            "action": "ingest",
            "sourceId": "live-eval",
            "documents": [{
                "url": "https://svc-20.example/pricing",
                "originRoot": "svc-20.example",
                "markdown": "",
                "unreliable": true
            }]
        }),
    )
    .await
    .unwrap();
    let _ = Engine::resolve(
        &scraper,
        None,
        json!({ "action": "reconcile", "sourceIds": ["live-eval"] }),
    )
    .await
    .unwrap();
    let c20 = Engine::resolve(
        &scraper,
        None,
        json!({ "action": "get", "conclusionId": "c_20" }),
    )
    .await
    .unwrap();
    let obs_lost_false_ref = usize::from(c20["conclusion"]["state"] == "refuted");

    // 4. ABA check on a separate Loop source
    let _ = Engine::context(
        &scraper,
        None,
        json!({
            "action": "ingest",
            "sourceId": "aba-eval",
            "documents": [{
                "url": "https://aba.example/p",
                "markdown": "AbaProduct costs $10/yr."
            }]
        }),
    )
    .await
    .unwrap();
    let _ = Engine::resolve(
        &scraper,
        None,
        json!({
            "action": "check",
            "sourceId": "aba-eval",
            "conclusionId": "c_aba_eval",
            "answer": "AbaProduct costs $10/yr.",
            "persist": true
        }),
    )
    .await
    .unwrap();
    let _ = Engine::resolve(
        &scraper,
        None,
        json!({ "action": "reconcile", "sourceIds": ["aba-eval"] }),
    )
    .await
    .unwrap();
    let st_ref = scraper.store_ref().unwrap().clone();
    st_ref
        .blocking(|st| {
            let sync = st.sync();
            let spec = pagesift_store::DataSourceSpec {
                id: "aba-eval".into(),
                name: None,
                seeds: vec!["https://aba.example/p".into()],
                include_paths: Vec::new(),
                exclude_paths: Vec::new(),
                max_depth: 0,
                allow_subdomains: false,
                freshness: pagesift_store::FreshnessPolicy::default(),
                budget: pagesift_store::SyncBudget::default(),
                extract_schema: None,
                extract_prompt: None,
                required_fields: Vec::new(),
                allow_browser: false,
                created_at_ms: 1,
                updated_at_ms: 1,
                revision: 1,
                instance_id: String::new(),
            };
            sync.upsert_source(spec)?;
            let _ = sync.delete_source("aba-eval")?;
            let spec2 = pagesift_store::DataSourceSpec {
                id: "aba-eval".into(),
                name: None,
                seeds: vec!["https://aba.example/p".into()],
                include_paths: Vec::new(),
                exclude_paths: Vec::new(),
                max_depth: 0,
                allow_subdomains: false,
                freshness: pagesift_store::FreshnessPolicy::default(),
                budget: pagesift_store::SyncBudget::default(),
                extract_schema: None,
                extract_prompt: None,
                required_fields: Vec::new(),
                allow_browser: false,
                created_at_ms: 2,
                updated_at_ms: 2,
                revision: 1,
                instance_id: String::new(),
            };
            sync.upsert_source(spec2)?;
            Ok::<_, pagesift_store::Error>(())
        })
        .await
        .unwrap();
    let rec_aba = Engine::resolve(
        &scraper,
        None,
        json!({ "action": "reconcile", "sourceIds": ["aba-eval"] }),
    )
    .await
    .unwrap();
    let aba_ok = rec_aba["report"]["abaSupersededSources"]
        .as_array()
        .unwrap()
        .iter()
        .any(|v| v == "aba-eval");

    scraper.shutdown().await;
    srv.abort();

    let e6 = LiveReconcileMetrics {
        persisted_conclusions: n_docs + 2,
        relocated_early_cutoff_false_triggers: reloc_false_triggers,
        relocated_early_cutoff_saved_count: reloc_saved,
        substantive_impact_recall: direct_hits as f64 / expected_direct.len() as f64,
        false_trigger_rate: false_triggers as f64 / untouched_total.max(1) as f64,
        scope_new_competitor_impact_recall: scope_hits as f64 / 2.0,
        observation_lost_false_refutations: obs_lost_false_ref,
        aba_fallback_triggered: aba_ok,
        incremental_recheck_micros: inc_us,
        full_recheck_micros: full_us,
        incremental_speedup_ratio: (full_us as f64 / inc_us.max(1) as f64 * 100.0).round() / 100.0,
        full_vs_incremental_agreement: agree_count as f64 / full_snapshot.len().max(1) as f64,
    };

    (e5, e6)
}

async fn list_conclusion_states(scraper: &Scraper) -> BTreeMap<String, String> {
    let out = Engine::resolve(
        scraper,
        None,
        json!({ "action": "conclusions", "sourceId": "live-eval", "limit": 200 }),
    )
    .await
    .unwrap();
    let mut map = BTreeMap::new();
    for c in out["conclusions"].as_array().unwrap() {
        let id = c["conclusionId"].as_str().unwrap().to_owned();
        let st = c["state"].as_str().unwrap().to_owned();
        map.insert(id, st);
    }
    map
}

fn eval_e7_learn() -> LearnEvalMetrics {
    let mut learn = ResolveLearnState::default();
    let cold_abstain = learn
        .calibrated_probability("pricing", "Specific", 0.85, true)
        .is_none();

    // 40 weak signals MUST be 100% rejected
    let weak_n = 40usize;
    for _ in 0..weak_n {
        let _ = learn.observe_feedback(&ResolveLearnFeedback {
            domain: "pricing".into(),
            claim_kind: "Specific".into(),
            raw_score: 0.9,
            grounded_supported: true,
            is_hard_ground_truth: false,
            counter_probe_kind: None,
            counter_found: None,
            verifier_version: None,
        });
    }
    let weak_acc = learn.summary().total_hard_samples;

    // 80 hard ground-truth samples with deterministic overconfident raw scores (true prob < raw)
    let mut rng = Rng::seed(FIXED_SEED);
    let mut raw_scores = Vec::new();
    let mut labels = Vec::new();
    for i in 0..80 {
        let raw = if i % 2 == 0 { 0.85f32 } else { 0.45f32 };
        let true_p = if i % 2 == 0 { 0.62 } else { 0.15 };
        let y = rng.uniform() < true_p;
        learn.observe_feedback(&ResolveLearnFeedback {
            domain: "pricing".into(),
            claim_kind: "Specific".into(),
            raw_score: raw,
            grounded_supported: y,
            is_hard_ground_truth: true,
            counter_probe_kind: Some(CounterProbeKind::OfficialErrataOrUpdate),
            counter_found: Some(!y),
            verifier_version: None,
        });
        raw_scores.push(f64::from(raw));
        labels.push(y);
    }

    // Holdout evaluation set (60 samples from same distribution)
    let mut eval_raw = Vec::new();
    let mut eval_cal = Vec::new();
    let mut eval_y = Vec::new();
    for i in 0..60 {
        let raw = if i % 2 == 0 { 0.85f32 } else { 0.45f32 };
        let true_p = if i % 2 == 0 { 0.62 } else { 0.15 };
        let y = rng.uniform() < true_p;
        let cal = learn
            .calibrated_probability("pricing", "Specific", raw, true)
            .unwrap_or(raw);
        eval_raw.push(f64::from(raw));
        eval_cal.push(f64::from(cal));
        eval_y.push(y);
    }
    let b_before = evaluate_calibration_metrics(&eval_raw, &eval_y)
        .map(|(b, _, _)| b)
        .unwrap_or(0.0);
    let b_after = evaluate_calibration_metrics(&eval_cal, &eval_y)
        .map(|(b, _, _)| b)
        .unwrap_or(0.0);
    let raw_losses: Vec<f64> = eval_raw
        .iter()
        .zip(&eval_y)
        .map(|(&p, &y)| {
            let t = if y { 1.0 } else { 0.0 };
            (p - t).powi(2)
        })
        .collect();
    let cal_losses: Vec<f64> = eval_cal
        .iter()
        .zip(&eval_y)
        .map(|(&p, &y)| {
            let t = if y { 1.0 } else { 0.0 };
            (p - t).powi(2)
        })
        .collect();
    let brier_diffs: Vec<f64> = raw_losses
        .iter()
        .zip(&cal_losses)
        .map(|(&r, &c)| c - r)
        .collect();
    let mut boot_rng = Rng::seed(FIXED_SEED ^ 0x55AA);
    let delta = pagesift_stat::verdict::paired_bootstrap(
        &brier_diffs,
        pagesift_stat::Delta::DEFAULT,
        &mut boot_rng,
    );
    let (ci_lo, ci_hi) = (delta.lo, delta.hi);

    // Abrupt regime shift: all high scores become false -> measure Page-Hinkley detection steps!
    let prev_fallbacks = learn.drift_fallbacks;
    let mut drift_steps = 0usize;
    for step in 1..=60 {
        learn.observe_feedback(&ResolveLearnFeedback {
            domain: "pricing".into(),
            claim_kind: "Specific".into(),
            raw_score: 0.92,
            grounded_supported: false,
            is_hard_ground_truth: true,
            counter_probe_kind: None,
            counter_found: None,
            verifier_version: None,
        });
        if learn.drift_fallbacks > prev_fallbacks {
            drift_steps = step;
            break;
        }
    }

    learn.ensure_verifier_version("resolve-det-v2");
    let vv_reset = learn.buckets.is_empty();

    let r4 = |x: f64| (x * 10_000.0).round() / 10_000.0;
    LearnEvalMetrics {
        cold_start_abstained: cold_abstain,
        weak_signals_submitted: weak_n,
        weak_signals_accepted: weak_acc,
        hard_samples_trained: 80,
        brier_before: r4(b_before),
        brier_after: r4(b_after),
        brier_ci_low: r4(ci_lo),
        brier_ci_high: r4(ci_hi),
        drift_detection_steps: drift_steps,
        verifier_version_bump_reset: vv_reset,
    }
}

fn eval_e8_perf() -> RuntimePerfMetrics {
    let mut idx = ContextIndex::default();
    let ts = 1_770_000_000_000u64;
    for i in 0..40 {
        add_doc(
            &mut idx,
            &format!("https://perf-{i}.example/spec"),
            &format!(
                "# Spec {i}\n\n| Tier | Annual Price |\n|---|---|\n| Pro{i} | ${} |\n",
                50 + i
            ),
            &format!("perf-{i}.example"),
            true,
            ts,
            None,
        );
    }
    let mut learn = ResolveLearnState::default();
    let n = 400usize;
    let mut latencies = Vec::with_capacity(n);
    let t_all = Instant::now();
    for i in 0..n {
        let k = i % 40;
        let req = ResolveCheckRequest {
            answer: format!("Pro{k} costs ${}/yr.", 50 + k),
            ..ResolveCheckRequest::default()
        };
        let t0 = Instant::now();
        let mut c = verify_answer_deterministic(&req, &idx, &[], &mut learn, ts);
        let plans = plan_counter_probes(&c, &learn);
        let _ = run_local_counter_probes(&mut c, &plans, &idx, None, false);
        latencies.push(t0.elapsed().as_micros() as u64);
    }
    let total_secs = t_all.elapsed().as_secs_f64().max(1e-6);
    latencies.sort_unstable();
    let pct = |p: usize| latencies[(latencies.len() * p / 100).min(latencies.len() - 1)];

    RuntimePerfMetrics {
        checks_measured: n,
        qps: (n as f64 / total_secs * 10.0).round() / 10.0,
        p50_us: pct(50),
        p95_us: pct(95),
        p99_us: pct(99),
        peak_rss_mib: read_peak_rss_mib(),
    }
}

fn eval_e9_honest_limitations() -> HonestLimitationsReport {
    // Purely metaphorical / heavy synonym paraphrases where deterministic lexical overlap is low:
    // Resolve must honestly abstain (`Insufficient`), NEVER hallucinate `Supported`!
    let mut idx = ContextIndex::default();
    let ts = 1_770_000_000_000u64;
    let pairs = [
        (
            "ZephyrDB eliminates manual shard rebalancing via automated consistent-hash partitioning.",
            "ZephyrDB removes the operator burden of redistributing database slices across nodes.",
        ),
        (
            "Solaris API imposes a strict ceiling of 600 requests per minute on standard keys.",
            "Ordinary Solaris API credentials are throttled once throughput crosses ten calls every second.",
        ),
        (
            "Boreal Cloud encrypts customer object payloads at rest using AES-256-GCM.",
            "Stored blobs inside Boreal Cloud remain unreadable without symmetric cryptographic keys.",
        ),
    ];
    let mut learn = ResolveLearnState::default();
    let mut abstained = 0usize;
    let mut false_sup = 0usize;
    for (i, (doc_text, paraphrase)) in pairs.iter().enumerate() {
        add_doc(
            &mut idx,
            &format!("https://para-{i}.example/doc"),
            doc_text,
            &format!("para-{i}.example"),
            true,
            ts,
            None,
        );
        let req = ResolveCheckRequest {
            answer: (*paraphrase).to_owned(),
            ..ResolveCheckRequest::default()
        };
        let c = verify_answer_deterministic(&req, &idx, &[], &mut learn, ts);
        if c.state == ResolutionState::Insufficient {
            abstained += 1;
        } else if c.state == ResolutionState::Supported {
            false_sup += 1;
        }
    }
    HonestLimitationsReport {
        paraphrase_cases_tested: pairs.len(),
        deterministic_honest_abstention_count: abstained,
        deterministic_false_support_count: false_sup,
        note: "DeterministicOnly verifier conservatively returns Insufficient on heavy semantic paraphrases or cross-sentence multi-hop inference rather than guessing; use HybridAssisted (quote-grounded LLM) when recall on abstract paraphrases is required.",
    }
}

fn main() {
    let rt = tokio::runtime::Builder::new_multi_thread()
        .worker_threads(2)
        .enable_all()
        .build()
        .unwrap();

    let e1 = eval_e1();
    let e2 = eval_e2();
    let e3 = eval_e3();
    let e4 = eval_e4();
    let (e5, e6) = rt.block_on(eval_e5_gap_and_e6_live());
    let e7 = eval_e7_learn();
    let e8 = eval_e8_perf();
    let e9 = eval_e9_honest_limitations();

    let report = ResolveEvalReport {
        version: "1.7.0",
        seed: FIXED_SEED,
        hardware: HardwareInfo {
            logical_cores: std::thread::available_parallelism()
                .map(|n| n.get())
                .unwrap_or(1),
            os_arch: std::env::consts::ARCH,
            peak_rss_mib: read_peak_rss_mib(),
        },
        e1_claim_verification: e1,
        e2_origin_collapse: e2,
        e3_temporal_and_causal: e3,
        e4_counter_and_scope: e4,
        e5_gap_acquisition: e5,
        e6_live_reconciliation: e6,
        e7_conservative_learn: e7,
        e8_runtime_perf: e8,
        e9_honest_limitations: e9,
    };
    println!("{}", serde_json::to_string_pretty(&report).unwrap());
}
