//! Sync 采集充分性评估、质量对比与 Frontier URL 规则联合学习（C07 / C14 / C15 / C28 / R04）。

use std::collections::{BTreeMap, BTreeSet};

use pagesift_api::ApiError;
use pagesift_api::types::Document;
use pagesift_graph::{ClaimRecord, EvidenceAlignment};
use pagesift_net::Url;
use pagesift_plan::experience::{FamilyExperienceTable, RuleKind};
use pagesift_plan::strategy::{
    CaptureAssessment, CaptureGoal, CaptureSignals, evaluate_capture_sufficiency,
};
use pagesift_store::{DataSourceSpec, ResourceLifecycleState, ResourceRecord};
use serde_json::Value;

use crate::engine::text_weight;
use crate::frontier::{Learner, Rule};

pub fn is_barrier_doc(doc: &Document) -> bool {
    matches!(doc.metadata.status_code, 403 | 429)
        || doc.metadata.pagesift.as_ref().is_some_and(|p| {
            p.warnings.iter().any(|w| {
                w.starts_with("barrier:")
                    || w.contains("access_restricted")
                    || w.contains("bot_challenge")
            })
        })
}

pub fn has_embedded_json_signal(doc: &Document) -> bool {
    doc.metadata
        .pagesift
        .as_ref()
        .is_some_and(|p| !p.json_ld.is_empty())
        || doc.graph.as_ref().is_some_and(|g| {
            g.claims
                .iter()
                .any(|c| c.evidence.method.is_metadata_island())
        })
}

fn value_non_empty(v: &Value) -> bool {
    match v {
        Value::Null => false,
        Value::String(s) => !s.trim().is_empty(),
        Value::Array(a) => !a.is_empty(),
        Value::Object(o) => !o.is_empty(),
        _ => true,
    }
}

fn extract_field_values(doc: &Document) -> BTreeMap<String, Value> {
    let mut out = BTreeMap::new();
    let Some(val) = doc.extract.as_ref() else {
        return out;
    };
    match val {
        Value::Object(map) => {
            for (k, v) in map {
                if value_non_empty(v) {
                    out.insert(k.clone(), v.clone());
                }
            }
        }
        Value::Array(items) => {
            for item in items {
                if let Some(map) = item.as_object() {
                    for (k, v) in map {
                        if value_non_empty(v) {
                            out.entry(k.clone()).or_insert_with(|| v.clone());
                        }
                    }
                }
            }
        }
        _ => {}
    }
    out
}

fn value_tokens(v: &Value) -> Vec<String> {
    match v {
        Value::String(s) => s
            .split(|c: char| !c.is_alphanumeric())
            .filter(|t| !t.is_empty())
            .map(str::to_ascii_lowercase)
            .collect(),
        Value::Number(n) => vec![n.to_string().to_ascii_lowercase()],
        Value::Bool(b) => vec![b.to_string()],
        _ => Vec::new(),
    }
}

fn claim_matches_field(c: &ClaimRecord, req_field: &str) -> bool {
    let norm_req = req_field.trim().to_ascii_lowercase();
    if c.normalized_field.eq_ignore_ascii_case(&norm_req)
        || c.field_path.eq_ignore_ascii_case(&norm_req)
    {
        return true;
    }
    c.field_path
        .rsplit('.')
        .next()
        .is_some_and(|leaf| leaf.eq_ignore_ascii_case(&norm_req))
}

fn claim_is_value_grounded(claim: &ClaimRecord, doc: &Document) -> bool {
    if !claim.evidence.verified
        || matches!(claim.evidence.alignment, EvidenceAlignment::Unaligned)
        || !value_non_empty(&claim.raw_value)
    {
        return false;
    }
    let Some(excerpt) = claim
        .evidence
        .start
        .zip(claim.evidence.end)
        .and_then(|(s, e)| {
            doc.markdown
                .as_deref()
                .and_then(|md| md.get(s..e))
                .or(Some(claim.evidence.excerpt.as_str()))
        })
    else {
        return false;
    };
    let lower_ex = excerpt.to_ascii_lowercase();
    let tokens = value_tokens(&claim.raw_value);
    if tokens.is_empty() {
        return !excerpt.trim().is_empty();
    }
    tokens.iter().any(|tok| lower_ex.contains(tok))
}

/// 基于契约字段级证据对齐与基线一致性执行 Capture 充足性判定（C14 / C15）。
pub fn assess_document_capture(
    spec: &DataSourceSpec,
    rec: &ResourceRecord,
    doc: &Document,
    goal_base: &CaptureGoal,
) -> CaptureAssessment {
    let mut goal = goal_base.clone();
    let has_claims = doc.graph.as_ref().is_some_and(|g| !g.claims.is_empty());
    goal.expect_links_for_hub = matches!(
        rec.role,
        pagesift_store::ResourceRole::Pagination | pagesift_store::ResourceRole::FeedOrSitemap
    ) || (rec.role == pagesift_store::ResourceRole::Hub && !has_claims);
    let md_chars = doc.markdown.as_deref().map_or(0, str::len);
    let not_mod = doc
        .metadata
        .pagesift
        .as_ref()
        .is_some_and(|p| p.not_modified);
    let barrier = is_barrier_doc(doc);
    let contract_id = spec.contract_id();
    let baseline_matches_contract = rec.baseline_sufficient
        && rec.last_validator_contract_id.as_deref() == Some(contract_id.as_str());

    let extracted_map = extract_field_values(doc);
    let claims: &[ClaimRecord] = doc.graph.as_ref().map_or(&[], |g| &g.claims);

    let mut present_set: BTreeSet<String> = extracted_map.keys().cloned().collect();
    for req in &spec.required_fields {
        if !present_set.contains(req)
            && claims
                .iter()
                .any(|c| claim_matches_field(c, req) && value_non_empty(&c.raw_value))
        {
            present_set.insert(req.clone());
        }
    }
    let extracted_fields_present: Vec<String> = present_set.into_iter().collect();

    let mut grounded_fields_present = Vec::new();
    for req in &spec.required_fields {
        let grounded = claims.iter().any(|c| {
            claim_matches_field(c, req)
                && claim_is_value_grounded(c, doc)
                && extracted_map.get(req).is_none_or(|ev| {
                    let ev_toks = value_tokens(ev);
                    let cl_toks = value_tokens(&c.raw_value);
                    ev_toks.is_empty() || ev_toks.iter().any(|t| cl_toks.contains(t))
                })
        });
        if grounded {
            grounded_fields_present.push(req.clone());
        }
    }

    let grounded_evidence_count = claims
        .iter()
        .filter(|c| claim_is_value_grounded(c, doc))
        .count();
    let unaligned_claim_count = claims
        .iter()
        .filter(|c| {
            c.evidence.alignment == EvidenceAlignment::Unaligned
                || (spec
                    .required_fields
                    .iter()
                    .any(|r| claim_matches_field(c, r))
                    && !claim_is_value_grounded(c, doc))
        })
        .count();
    let outgoing_links_count = doc.links.as_ref().map_or(0, Vec::len);

    evaluate_capture_sufficiency(
        &goal,
        &CaptureSignals {
            status_code: doc.metadata.status_code,
            not_modified_304: not_mod,
            baseline_matches_contract,
            markdown_chars: md_chars,
            has_embedded_json: has_embedded_json_signal(doc),
            has_client_shell_markers: !not_mod
                && md_chars < goal.min_markdown_chars
                && outgoing_links_count == 0
                && claims.is_empty(),
            extracted_fields_present,
            grounded_fields_present,
            grounded_evidence_count,
            unaligned_claim_count,
            outgoing_links_count,
            is_barrier: barrier,
        },
    )
}

/// 多维质量向量比较决定是否采用 Browser 升级结果（C07）：绝不让劣质/拦截浏览器壳覆写健康 HTTP。
pub fn should_adopt_browser_escalation(
    http_doc: &Document,
    http_assess: &CaptureAssessment,
    browser_doc: &Document,
    browser_assess: &CaptureAssessment,
) -> (bool, bool) {
    if browser_doc.metadata.status_code >= 400 || is_barrier_doc(browser_doc) {
        return (false, false);
    }
    let h_weight = text_weight(http_doc.markdown.as_deref().unwrap_or_default());
    let b_weight = text_weight(browser_doc.markdown.as_deref().unwrap_or_default());
    let h_claims = http_doc.graph.as_ref().map_or(0, |g| g.claims.len());
    let b_claims = browser_doc.graph.as_ref().map_or(0, |g| g.claims.len());

    if browser_assess.sufficient && !http_assess.sufficient {
        return (true, true);
    }
    if http_assess.sufficient && !browser_assess.sufficient {
        return (false, false);
    }
    if browser_assess.ungrounded_fields.len() < http_assess.ungrounded_fields.len() {
        return (true, true);
    }
    if b_claims > h_claims {
        return (true, true);
    }
    let text_improved = (h_weight == 0 && b_weight > 0) || b_weight > h_weight.saturating_add(32);
    if !http_assess.sufficient && text_improved {
        return (true, true);
    }
    (text_improved && b_claims >= h_claims, text_improved)
}

pub fn frontier_rule_to_plan(rule: &Rule) -> Option<(String, RuleKind)> {
    match rule {
        Rule::DropName { host, name } => {
            Some((host.clone(), RuleKind::DropParam { name: name.clone() }))
        }
        Rule::DropPair { host, name, value } => Some((
            host.clone(),
            RuleKind::DropParamValue {
                name: name.clone(),
                value: value.clone(),
            },
        )),
        Rule::StripSuffix { host, suffix } => Some((
            host.clone(),
            RuleKind::StripPathSuffix {
                suffix: suffix.clone(),
            },
        )),
    }
}

pub fn plan_rule_to_frontier(host: &str, kind: &RuleKind) -> Rule {
    match kind {
        RuleKind::DropParam { name } => Rule::DropName {
            host: host.to_string(),
            name: name.clone(),
        },
        RuleKind::DropParamValue { name, value } => Rule::DropPair {
            host: host.to_string(),
            name: name.clone(),
            value: value.clone(),
        },
        RuleKind::StripPathSuffix { suffix } => Rule::StripSuffix {
            host: host.to_string(),
            suffix: suffix.clone(),
        },
    }
}

pub fn build_seeded_learner(
    inventory: &BTreeMap<String, ResourceRecord>,
    experience: &FamilyExperienceTable,
) -> Learner {
    let mut learner = Learner::default();
    for rule in experience.list_url_rules(None) {
        if rule.state == pagesift_plan::experience::RuleLifecycleState::Verified {
            learner.import_active_rule(plan_rule_to_frontier(&rule.host, &rule.kind));
        }
    }
    for rec in inventory.values() {
        if rec.state == ResourceLifecycleState::Active
            && let Some(sig) = rec.text_signature
            && let Ok(u) = Url::parse(&rec.url)
        {
            learner.seed_known_page(&u, sig);
        }
    }
    learner
}

pub fn sync_learner_into_experience(
    learner: &mut Learner,
    experience: &FamilyExperienceTable,
    url_str: &str,
    markdown: &str,
    now_ms: u64,
) {
    if markdown.trim().is_empty() {
        return;
    }
    let Ok(u) = Url::parse(url_str) else {
        return;
    };
    let _ = learner.observe(&u, markdown);
    for rule in learner.export_active_rules() {
        if let Some((host, kind)) = frontier_rule_to_plan(&rule) {
            experience.confirm_url_rule(&host, kind, now_ms);
        }
    }
}

/// 检查规则改写抓取是否发生 4xx/5xx 失败或 200 内容/Canonical 背离，并双侧撤销（R04）。
pub fn maybe_falsify_rewritten_rules(
    learner: &mut Learner,
    experience: &FamilyExperienceTable,
    original_url: &str,
    applied_rules: &[RuleKind],
    res: Result<&Document, &ApiError>,
    prev_rec: &ResourceRecord,
    now_ms: u64,
) -> bool {
    if applied_rules.is_empty() {
        return false;
    }
    let bad = match res {
        Err(_) => true,
        Ok(doc) => {
            doc.metadata.status_code >= 400
                || is_barrier_doc(doc)
                || doc.metadata.canonical_url.as_deref().is_some_and(|c| {
                    Url::parse(c).ok().and_then(|cu| {
                        Url::parse(original_url)
                            .ok()
                            .map(|ou| cu.path() != ou.path())
                    }) == Some(true)
                })
                || (prev_rec.baseline_sufficient
                    && doc.markdown.as_deref().is_some_and(|m| m.trim().is_empty()))
        }
    };
    if !bad {
        return false;
    }
    if let Ok(u) = Url::parse(original_url)
        && let Some(host) = u.host_str()
    {
        for kind in applied_rules {
            experience.falsify_url_rule(host, kind, now_ms);
            learner.revoke_rule(&plan_rule_to_frontier(host, kind));
        }
    }
    true
}
