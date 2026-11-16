//! Sync 采集充分性评估、质量对比与 Frontier URL 规则联合学习（C07 / C14 / C15 / C28 / R04）。

use std::collections::{BTreeMap, BTreeSet};

use pagesift_api::types::Document;
use pagesift_api::{ApiError, ErrorCode};
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

/// 观测可靠性诊断（G1 Observe：识别非 UTF-8 不确定回退编码、体截断、图截断与无效 304）。
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct CaptureReliability {
    pub uncertain_decode: bool,
    pub truncated: bool,
    pub invalid_304: bool,
}

impl CaptureReliability {
    pub fn is_unreliable(self) -> bool {
        self.uncertain_decode || self.truncated || self.invalid_304
    }
}

pub fn assess_capture_reliability(doc: &Document) -> CaptureReliability {
    let Some(ps) = doc.metadata.pagesift.as_ref() else {
        return CaptureReliability::default();
    };
    let mut out = CaptureReliability::default();
    for w in &ps.warnings {
        if w.starts_with("decode:") && !w.contains("valid UTF-8; decoded as UTF-8 instead") {
            out.uncertain_decode = true;
        }
        if w.starts_with("body_truncated") || w.starts_with("graph_truncated") {
            out.truncated = true;
        }
        if w.starts_with("not_modified:") {
            out.invalid_304 = true;
        }
    }
    out
}

pub fn has_embedded_json_signal(doc: &Document) -> bool {
    doc.metadata.pagesift.as_ref().is_some_and(|p| {
        !p.json_ld.is_empty()
            || p.plan.as_ref().is_some_and(|steps| {
                steps
                    .steps
                    .iter()
                    .any(|s| s.action == "embedded-json" && s.chosen)
            })
    }) || doc.graph.as_ref().is_some_and(|g| {
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
    let md_chars = doc
        .markdown
        .as_deref()
        .map_or(0, |m| m.chars().filter(|c| !c.is_whitespace()).count());
    let pagesift_meta = doc.metadata.pagesift.as_ref();
    let reliability = assess_capture_reliability(doc);
    let not_mod = pagesift_meta.is_some_and(|p| p.not_modified) && !reliability.invalid_304;
    let has_empty_warning =
        pagesift_meta.is_some_and(|p| p.warnings.iter().any(|w| w.starts_with("empty_content:")));
    let has_thin_warning =
        pagesift_meta.is_some_and(|p| p.warnings.iter().any(|w| w.starts_with("thin_content:")));
    let js_shell_hint = pagesift_meta.is_some_and(|p| p.js_shell_hint);
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

    let has_client_shell_markers = !not_mod
        && claims.is_empty()
        && extracted_fields_present.is_empty()
        && (has_empty_warning
            || has_thin_warning
            || js_shell_hint
            || md_chars == 0
            || (md_chars < goal.min_markdown_chars && outgoing_links_count == 0));

    evaluate_capture_sufficiency(
        &goal,
        &CaptureSignals {
            status_code: doc.metadata.status_code,
            not_modified_304: not_mod,
            baseline_matches_contract,
            markdown_chars: md_chars,
            has_embedded_json: has_embedded_json_signal(doc),
            has_client_shell_markers,
            extracted_fields_present,
            grounded_fields_present,
            grounded_evidence_count,
            unaligned_claim_count,
            outgoing_links_count,
            is_barrier: barrier,
        },
    )
}

/// 多维质量向量比较决定是否采用 Browser 升级结果（C07）：
/// - 绝不让不可靠（不确定解码/截断/304/4xx/5xx/拦截）的浏览器结果替换健康 HTTP；
/// - 当 HTTP 本身不可靠而 Browser 干净完整时允许采用 Browser 救回本次采集，但必须将 `browser_gain` 删失（`None`）以防误判 HTTP 能力；
///
/// 返回 `(是否采用, 合法可观测的 browser_gain)`。
pub fn should_adopt_browser_escalation(
    http_doc: &Document,
    http_assess: &CaptureAssessment,
    browser_doc: &Document,
    browser_assess: &CaptureAssessment,
) -> (bool, Option<bool>) {
    let b_not_mod = browser_doc
        .metadata
        .pagesift
        .as_ref()
        .is_some_and(|p| p.not_modified);
    if browser_doc.metadata.status_code >= 400 || is_barrier_doc(browser_doc) || b_not_mod {
        return (false, None);
    }
    let b_unreliable = assess_capture_reliability(browser_doc).is_unreliable();
    if b_unreliable {
        return (false, None);
    }
    let h_unreliable = assess_capture_reliability(http_doc).is_unreliable();
    let h_weight = text_weight(http_doc.markdown.as_deref().unwrap_or_default());
    let b_weight = text_weight(browser_doc.markdown.as_deref().unwrap_or_default());
    let h_claims = http_doc.graph.as_ref().map_or(0, |g| g.claims.len());
    let b_claims = browser_doc.graph.as_ref().map_or(0, |g| g.claims.len());

    if h_unreliable {
        let rescue = browser_assess.sufficient
            || b_claims > h_claims
            || b_weight > h_weight.saturating_add(16);
        return (rescue, None);
    }

    if browser_assess.sufficient && !http_assess.sufficient {
        return (true, Some(true));
    }
    if http_assess.sufficient && !browser_assess.sufficient {
        return (false, Some(false));
    }
    if browser_assess.ungrounded_fields.len() < http_assess.ungrounded_fields.len() {
        return (true, Some(true));
    }
    if b_claims > h_claims {
        return (true, Some(true));
    }
    let text_improved = (h_weight == 0 && b_weight > 0) || b_weight > h_weight.saturating_add(32);
    if !http_assess.sufficient && text_improved {
        return (true, Some(true));
    }
    (text_improved && b_claims >= h_claims, Some(text_improved))
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

/// 检查规则改写抓取是否发生 404/410 等确定无效或 200 内容/Canonical 背离，并双侧撤销（R04）。
/// 传输错误、超时、限流、拦截或 5xx 瞬态失败不得误伤已有 URL 改写规则（P1-03）。
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
        Err(err) => matches!(err.code, ErrorCode::NotFound | ErrorCode::InvalidRequest),
        Ok(doc) => {
            let status = doc.metadata.status_code;
            let not_modified_304 = doc
                .metadata
                .pagesift
                .as_ref()
                .is_some_and(|p| p.not_modified);
            let client_path_invalid =
                (400..500).contains(&status) && !matches!(status, 403 | 408 | 429);
            let canonical_diverged = !is_barrier_doc(doc)
                && status < 400
                && doc.metadata.canonical_url.as_deref().is_some_and(|c| {
                    Url::parse(c).ok().and_then(|cu| {
                        Url::parse(original_url)
                            .ok()
                            .map(|ou| cu.path() != ou.path())
                    }) == Some(true)
                });
            let collapsed = !is_barrier_doc(doc)
                && status < 400
                && !not_modified_304
                && prev_rec.baseline_sufficient
                && doc.markdown.as_deref().is_some_and(|m| m.trim().is_empty());
            client_path_invalid || canonical_diverged || collapsed
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

pub(super) fn prepare_rewritten_fetch_url(
    spec: &DataSourceSpec,
    experience: &FamilyExperienceTable,
    raw_url: &str,
    now_ms_val: u64,
) -> (String, Vec<RuleKind>) {
    let mut fetch_url = raw_url.to_string();
    let mut applied_rules = Vec::new();
    if let Ok(mut parsed_u) = Url::parse(&fetch_url) {
        applied_rules = experience.rewrite_url(&mut parsed_u);
        if !applied_rules.is_empty() {
            let cand_rewritten = parsed_u.to_string();
            if super::inventory::normalize_in_scope_url(spec, &cand_rewritten).is_some() {
                fetch_url = cand_rewritten;
            } else {
                // URL 重写越出 Scope 时立即废止改写，保留合法原 URL（C21）
                if let Ok(orig_u) = Url::parse(raw_url)
                    && let Some(h) = orig_u.host_str()
                {
                    for r in &applied_rules {
                        experience.falsify_url_rule(h, r, now_ms_val);
                    }
                }
                applied_rules.clear();
            }
        }
    }
    (fetch_url, applied_rules)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn browser_adoption_checks_independent_reliability() {
        let ok_assess = CaptureAssessment {
            sufficient: true,
            needs_browser_escalation: false,
            confidence: 1.0,
            missing_fields: Vec::new(),
            ungrounded_fields: Vec::new(),
            reasons: Vec::new(),
        };
        let poor_assess = CaptureAssessment {
            sufficient: false,
            needs_browser_escalation: true,
            confidence: 0.0,
            missing_fields: Vec::new(),
            ungrounded_fields: Vec::new(),
            reasons: Vec::new(),
        };

        let clean_http = Document {
            markdown: Some("Clean HTTP full catalog text with sufficient prose details.".into()),
            ..Default::default()
        };
        let mut unreliable_browser = Document {
            markdown: Some("Clean looking text but truncated".into()),
            ..Default::default()
        };
        unreliable_browser.metadata.pagesift = Some(pagesift_api::types::PagesiftMeta {
            warnings: vec!["body_truncated".into()],
            ..Default::default()
        });
        let (adopt_bad_b, gain_bad_b) = should_adopt_browser_escalation(
            &clean_http,
            &ok_assess,
            &unreliable_browser,
            &ok_assess,
        );
        assert!(!adopt_bad_b);
        assert_eq!(gain_bad_b, None);

        let mut bad_http = Document {
            markdown: Some("Garbled".into()),
            ..Default::default()
        };
        bad_http.metadata.pagesift = Some(pagesift_api::types::PagesiftMeta {
            warnings: vec!["decode:windows-1252".into()],
            ..Default::default()
        });
        let (rescue, rescue_gain) =
            should_adopt_browser_escalation(&bad_http, &poor_assess, &clean_http, &ok_assess);
        assert!(rescue);
        assert_eq!(rescue_gain, None);
    }
}
