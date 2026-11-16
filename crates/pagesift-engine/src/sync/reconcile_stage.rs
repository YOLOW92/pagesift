//! Candidate failure reconciliation and storage staging helpers for sync ticks.

use pagesift_api::{ApiError, ErrorCode};
use pagesift_graph::VersionRecord;
use pagesift_store::{VersionBundleRequest, VersionCommitRequest};

use super::super::inventory::new_discovered_record;
use super::*;

fn is_site_barrier_code(code: ErrorCode) -> bool {
    matches!(
        code,
        ErrorCode::AccessBarrier
            | ErrorCode::RobotsDisallowed
            | ErrorCode::RateLimited
            | ErrorCode::HostCooldown
    )
}

#[allow(clippy::too_many_arguments)]
pub(in crate::sync) fn reconcile_candidate_failure(
    spec: &DataSourceSpec,
    cand: &ScheduledCandidate,
    strategy_used: AcquisitionStrategy,
    step_cost_ms: f64,
    obs_time_ms: u64,
    error: &ApiError,
    inventory: &mut BTreeMap<String, ResourceRecord>,
    experience: &FamilyExperienceTable,
    graph_store: &GraphStore<'_>,
    prepared_bundles: &mut Vec<PreparedVersionBundle>,
    stats: &mut SyncTickStats,
) -> Result<(), ApiError> {
    let is_barrier = is_site_barrier_code(error.code);
    if is_barrier {
        stats.access_restricted_count += 1;
    } else {
        stats.transient_failures += 1;
    }
    let fam_name = family(&cand.record.url);
    // 失败请求未获取有效内容：http_sufficient 与 browser_gain 必须保持 censored (None)；
    // 仅结构化访问受限错误（AccessBarrier/RobotsDisallowed/RateLimited/HostCooldown）更新 barrier_rate（P1-03）。
    if is_barrier {
        experience.record_observation(
            &fam_name,
            &FamilyObservation {
                observed_at_ms: obs_time_ms,
                http_sufficient: None,
                browser_gain: None,
                conditional_hit: None,
                embedded_json_observed: None,
                substantive_change: None, // Censored on error (C16)
                yielded_claims_or_links: None,
                encountered_barrier: Some(true),
                cost_ms: step_cost_ms,
                used_browser: strategy_used == AcquisitionStrategy::DirectBrowser,
                browser_failed: (strategy_used == AcquisitionStrategy::DirectBrowser)
                    .then_some(true),
                ..Default::default()
            },
        );
    }

    let fam_stats = experience.get_family(&fam_name);
    let contract_id = spec.contract_id();
    let rec_key = cand.record.resource_key.clone();
    if let Some(rec_mut) = inventory.get_mut(&rec_key) {
        apply_observation_to_record(
            spec,
            rec_mut,
            &InventoryObservation {
                now_ms: obs_time_ms,
                status_code: 0,
                not_modified_304: false,
                is_barrier_or_error: is_barrier,
                had_transport_error: true,
                final_url: rec_mut.url.clone(),
                canonical_url: None,
                permanent_redirect_target: None,
                etag: None,
                last_modified: None,
                contract_id: contract_id.clone(),
                capture_sufficient: false,
                has_embedded_json: false,
                text_signature: None,
                content_hash: None,
                doc_id: None,
                title: None,
                claim_count: 0,
                substantive_changed: false,
                strategy_used: strategy_used.as_str().to_string(),
                ..Default::default()
            },
        );
        rec_mut.next_due_at_ms =
            compute_next_due_at_ms(spec, rec_mut, fam_stats.as_ref(), obs_time_ms, false);
    }

    let obs_outcome = ObservationOutcome::TransientFetchError {
        error: error.message.clone(),
    };
    stage_bundle_for_candidate(
        graph_store,
        prepared_bundles,
        &rec_key,
        &contract_id,
        &obs_outcome,
        None,
        None,
        &Document::default(),
        obs_time_ms,
    )
}

#[allow(clippy::too_many_arguments)]
pub(super) fn stage_bundle_for_candidate(
    graph_store: &GraphStore<'_>,
    prepared_bundles: &mut Vec<PreparedVersionBundle>,
    rec_key: &str,
    contract_id: &str,
    outcome: &ObservationOutcome,
    slice: Option<&pagesift_graph::GraphSlice>,
    staged: Option<&StagedGraphCommit>,
    doc: &Document,
    obs_time_ms: u64,
) -> Result<(), ApiError> {
    let authoritative = crate::graph::stage::is_authoritative_outcome(outcome);
    let mut bundle_req = VersionBundleRequest {
        version: None,
        url_or_key: rec_key,
        contract_id,
        observation_outcome: outcome,
        claims: &[],
        observed_at_ms: obs_time_ms,
        doc_id: None,
        doc_node: None,
        provenance: &[],
        citations: &[],
    };

    if authoritative && let (Some(slice), Some(st)) = (slice, staged) {
        let md_str = st
            .markdown
            .as_deref()
            .or(doc.markdown.as_deref())
            .unwrap_or_default();
        let curr_record = VersionRecord {
            resource_key: st.node.resource_key.clone(),
            doc_id: st.node.doc_id.clone(),
            url: st.node.final_url.clone(),
            canonical_url: st.node.canonical_url.clone(),
            observed_at_ms: st.node.observed_at_ms,
            content_hash: st.node.content_hash.clone(),
            text_hash: st.node.text_hash.clone(),
            status_code: st.node.status_code,
            etag: st.node.etag.clone(),
            last_modified: st.node.last_modified.clone(),
        };
        let (snapshot_bytes, snapshot_unavail) = if st.not_modified {
            if let Some(b304) = st.matched_304_md.as_deref() {
                (Some(b304.as_bytes()), None)
            } else {
                (
                    None,
                    Some(
                        st.not_modified_unavail_reason
                            .as_deref()
                            .unwrap_or("validator_mismatch"),
                    ),
                )
            }
        } else if !md_str.is_empty() {
            (Some(md_str.as_bytes()), None)
        } else {
            (None, Some("empty_markdown"))
        };
        let prior_doc_in_tick = prepared_bundles
            .iter()
            .rev()
            .find(|b| b.version_resource_key() == Some(curr_record.resource_key.as_str()))
            .and_then(|b| b.version_doc_id());
        if prior_doc_in_tick != Some(curr_record.doc_id.as_str()) {
            let expected_prev_doc_id = prior_doc_in_tick
                .or_else(|| st.previous_version.as_ref().map(|p| p.doc_id.as_str()));
            bundle_req.version = Some(VersionCommitRequest {
                expected_prev_doc_id,
                alias_keys: &st.alias_keys,
                record: &curr_record,
                edge: slice.version.as_ref(),
                snapshot_bytes,
                snapshot_unavailable_reason: snapshot_unavail,
            });
        }
        bundle_req.claims = &slice.claims;
        bundle_req.observed_at_ms = slice.observed_at_ms;
        bundle_req.doc_id = Some(&slice.doc_id);
        bundle_req.doc_node = Some(&st.node);
        bundle_req.provenance = &slice.provenance;
        bundle_req.citations = &slice.citations;
        let prep = graph_store
            .prepare_version_bundle(&bundle_req)
            .map_err(|e| {
                pagesift_api::ApiError::new(
                    pagesift_api::ErrorCode::Internal,
                    format!("prepare bundle: {e}"),
                )
            })?;
        prepared_bundles.push(prep);
    } else {
        let prep = graph_store
            .prepare_version_bundle(&bundle_req)
            .map_err(|e| {
                pagesift_api::ApiError::new(
                    pagesift_api::ErrorCode::Internal,
                    format!("prepare bundle: {e}"),
                )
            })?;
        prepared_bundles.push(prep);
    }
    Ok(())
}

pub(super) fn insert_discovered_resource(
    inventory: &mut BTreeMap<String, ResourceRecord>,
    norm_url: String,
    parent_url: Option<String>,
    depth: usize,
    importance_weight: f64,
    now_ms: u64,
) {
    let key = DocNode::derive_resource_key(None, &norm_url);
    inventory.entry(key).or_insert_with(|| {
        new_discovered_record(
            norm_url,
            parent_url,
            depth,
            false,
            importance_weight,
            now_ms,
        )
    });
}

#[cfg(test)]
mod tests {
    use super::*;
    use pagesift_plan::strategy::{AdaptDecision, CaptureAssessment};
    use pagesift_store::{
        FreshnessPolicy, ResourceLifecycleState, ResourceRole, Store, SyncBudget,
    };

    fn test_spec() -> DataSourceSpec {
        DataSourceSpec {
            id: "s-stage".to_string(),
            instance_id: String::new(),
            name: None,
            seeds: vec!["https://example.com/docs/".to_string()],
            include_paths: Vec::new(),
            exclude_paths: Vec::new(),
            max_depth: 2,
            allow_subdomains: false,
            freshness: FreshnessPolicy::default(),
            budget: SyncBudget::default(),
            extract_schema: None,
            extract_prompt: None,
            required_fields: Vec::new(),
            allow_browser: true,
            revision: 1,
            created_at_ms: 1000,
            updated_at_ms: 1000,
        }
    }

    fn test_cand(url: &str) -> ScheduledCandidate {
        let key = DocNode::derive_resource_key(None, url);
        ScheduledCandidate {
            record: ResourceRecord {
                resource_key: key,
                url: url.to_string(),
                canonical_url: None,
                migrated_to: None,
                parent_url: None,
                depth: 1,
                role: ResourceRole::Detail,
                state: ResourceLifecycleState::Active,
                discovered_at_ms: 1000,
                last_checked_at_ms: Some(1500),
                last_substantive_change_at_ms: Some(1500),
                next_due_at_ms: 2000,
                etag: Some("\"v1\"".to_string()),
                last_modified: None,
                content_hash: None,
                latest_doc_id: Some("doc-prev".to_string()),
                last_status_code: Some(200),
                consecutive_failures: 0,
                consecutive_404s: 0,
                importance_weight: 1.0,
                last_strategy: None,
                title: None,
                claim_count: 1,
                last_validator_contract_id: None,
                last_authoritative_status: Some(200),
                baseline_sufficient: true,
                has_embedded_json: false,
                text_signature: None,
                confirmed_gone_at_ms: None,
                learn: pagesift_store::ResourceLearnStats::default(),
            },
            decision: AdaptDecision {
                family: family(url),
                strategy: AcquisitionStrategy::LightweightHttp,
                priority_score: 1.0,
                expected_cost_ms: 50.0,
                expected_value: 1.0,
                shadow_price: 1.0,
                reuse_source: None,
                exploration_probe: false,
                reasons: Vec::new(),
            },
        }
    }

    #[test]
    fn p1_03_failure_censors_stats_for_transport_and_browser_errors() {
        let dir = tempfile::tempdir().unwrap();
        let store = Store::open_dir(dir.path()).unwrap();
        let gs = store.graph();
        let spec = test_spec();
        let cand = test_cand("https://example.com/docs/a");
        let fam_name = family(&cand.record.url);

        for (code, strat, msg) in [
            (
                ErrorCode::DnsFailure,
                AcquisitionStrategy::LightweightHttp,
                "dns lookup failed (contains word barrier cleanly ignored)",
            ),
            (
                ErrorCode::Timeout,
                AcquisitionStrategy::DirectBrowser,
                "browser render timeout",
            ),
            (
                ErrorCode::Busy,
                AcquisitionStrategy::DirectBrowser,
                "no idle browser",
            ),
        ] {
            let mut inv = BTreeMap::from([(cand.record.resource_key.clone(), cand.record.clone())]);
            let exp = FamilyExperienceTable::new();
            let mut bundles = Vec::new();
            let mut stats = SyncTickStats::default();
            reconcile_candidate_failure(
                &spec,
                &cand,
                strat,
                9_999.0,
                3000,
                &ApiError::new(code, msg),
                &mut inv,
                &exp,
                &gs,
                &mut bundles,
                &mut stats,
            )
            .unwrap();
            assert_eq!(stats.transient_failures, 1);
            assert_eq!(stats.access_restricted_count, 0);
            assert!(
                exp.get_family(&fam_name).is_none(),
                "error {code:?} must not pollute family stats"
            );
        }

        // Real AccessBarrier increments barrier_rate ONLY while leaving http_sufficient / browser_gain untouched.
        let mut inv = BTreeMap::from([(cand.record.resource_key.clone(), cand.record.clone())]);
        let exp = FamilyExperienceTable::new();
        let mut bundles = Vec::new();
        let mut stats = SyncTickStats::default();
        reconcile_candidate_failure(
            &spec,
            &cand,
            AcquisitionStrategy::DirectBrowser,
            250.0,
            3000,
            &ApiError::new(ErrorCode::AccessBarrier, "cloudflare challenge"),
            &mut inv,
            &exp,
            &gs,
            &mut bundles,
            &mut stats,
        )
        .unwrap();
        assert_eq!(stats.access_restricted_count, 1);
        assert_eq!(stats.transient_failures, 0);
        let st = exp.get_family(&fam_name).unwrap();
        assert_eq!(st.http_sufficient.trials(), 0.0);
        assert_eq!(st.browser_gain.trials(), 0.0);
        assert_eq!(st.barrier_rate.trials(), 1.0);
        assert!(st.barrier_rate.mean() > 0.5);
    }

    #[test]
    fn p1_03_capture_rewrite_and_browser_escalation_censoring() {
        let cand = test_cand("https://example.com/docs/a?utm_source=x");
        let exp = FamilyExperienceTable::new();
        let mut learner = crate::frontier::Learner::default();
        let rules = vec![pagesift_plan::experience::RuleKind::DropParam {
            name: "utm_source".to_string(),
        }];

        // Network/timeout/5xx/429 must NOT falsify learned URL rewrite rules.
        for code in [
            ErrorCode::Timeout,
            ErrorCode::NetworkError,
            ErrorCode::AccessBarrier,
            ErrorCode::RateLimited,
        ] {
            let err = ApiError::new(code, "transient");
            assert!(!crate::sync::capture::maybe_falsify_rewritten_rules(
                &mut learner,
                &exp,
                &cand.record.url,
                &rules,
                Err(&err),
                &cand.record,
                5000,
            ));
        }

        // 503 or 429 Ok(doc) must NOT falsify rules either, while 404 DOES.
        let mut doc503 = Document::default();
        doc503.metadata.status_code = 503;
        assert!(!crate::sync::capture::maybe_falsify_rewritten_rules(
            &mut learner,
            &exp,
            &cand.record.url,
            &rules,
            Ok(&doc503),
            &cand.record,
            5000,
        ));
        let mut doc404 = Document::default();
        doc404.metadata.status_code = 404;
        assert!(crate::sync::capture::maybe_falsify_rewritten_rules(
            &mut learner,
            &exp,
            &cand.record.url,
            &rules,
            Ok(&doc404),
            &cand.record,
            5000,
        ));

        // Browser escalation hitting 429/503 returns (false, None) so browser_gain isn't poisoned.
        let http_doc = Document {
            markdown: Some("thin shell".to_string()),
            ..Default::default()
        };
        let mut b_doc = Document::default();
        b_doc.metadata.status_code = 429;
        let assess_bad = CaptureAssessment {
            sufficient: false,
            needs_browser_escalation: true,
            confidence: 0.0,
            missing_fields: Vec::new(),
            ungrounded_fields: Vec::new(),
            reasons: Vec::new(),
        };
        let (adopt, gain) = crate::sync::capture::should_adopt_browser_escalation(
            &http_doc,
            &assess_bad,
            &b_doc,
            &assess_bad,
        );
        assert!(!adopt);
        assert_eq!(gain, None);
    }
}
