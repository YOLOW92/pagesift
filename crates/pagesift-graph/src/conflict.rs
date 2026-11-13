//! Explicit cross-source conflict and discrepancy detection without silent data loss.

use std::collections::{BTreeMap, BTreeSet};

use serde::{Deserialize, Serialize};
use serde_json::Value;

use crate::corroborate::ClaimRecord;
use crate::evidence::EvidenceSpan;
use crate::hash::{normalize_resource_key, short_id};
use crate::norm::{NormalizedValue, ValueComparison, compare_normalized};

/// Taxonomy of cross-source or cross-version claim disagreements.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum ConflictKind {
    ValueMismatch,
    UnitOrCurrencyMismatch,
    EngineDiscrepancy,
    TemporalStaleness,
    CanonicalDivergence,
    MultiplicityConflict,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum ConflictSeverity {
    Low,
    Medium,
    High,
}

/// One competing value candidate with all supporting sources and block evidence intact.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ConflictVariant {
    pub value: NormalizedValue,
    pub raw_values: Vec<Value>,
    pub support_count: usize,
    pub latest_observed_at_ms: u64,
    pub sources: Vec<String>,
    pub formats: Vec<String>,
    pub engines: Vec<String>,
    pub evidence: Vec<EvidenceSpan>,
}

/// Non-authoritative heuristic suggestion pointing at the strongest candidate variant.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct PreferredHint {
    pub variant_index: usize,
    pub reason: String,
}

/// Explicit record of conflicting claims under the same `(subject_key, normalized_field)`.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ConflictRecord {
    pub conflict_id: String,
    pub subject_key: String,
    pub field_path: String,
    pub normalized_field: String,
    pub kind: ConflictKind,
    pub severity: ConflictSeverity,
    pub variants: Vec<ConflictVariant>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub preferred_hint: Option<PreferredHint>,
}

/// Detect and classify all disagreements across normalized `ClaimRecord` entries.
pub fn detect_conflicts(claims: &[ClaimRecord]) -> Vec<ConflictRecord> {
    let mut by_slot: BTreeMap<(String, String), Vec<&ClaimRecord>> = BTreeMap::new();
    for claim in claims {
        by_slot
            .entry((claim.subject_key.clone(), claim.normalized_field.clone()))
            .or_default()
            .push(claim);
    }

    let mut conflicts = Vec::new();
    for ((subject_key, normalized_field), slot_claims) in by_slot {
        if let Some(rec) = build_conflict(&subject_key, &normalized_field, &slot_claims) {
            conflicts.push(rec);
        }
    }
    conflicts
}

fn build_conflict(
    subject_key: &str,
    normalized_field: &str,
    claims: &[&ClaimRecord],
) -> Option<ConflictRecord> {
    let mut by_value: BTreeMap<String, Vec<&ClaimRecord>> = BTreeMap::new();
    for &claim in claims {
        by_value
            .entry(claim.normalized_value.bucket_key())
            .or_default()
            .push(claim);
    }
    if by_value.len() <= 1 {
        return None;
    }

    let groups: Vec<Vec<&ClaimRecord>> = by_value.into_values().collect();
    let kind = classify_conflict(&groups);
    let severity = severity_for(kind, claims);

    let variants: Vec<ConflictVariant> = groups.iter().map(|g| to_variant(g)).collect();
    let preferred_hint = pick_preferred_hint(&groups, &variants);
    let field_path = claims[0].field_path.clone();

    Some(ConflictRecord {
        conflict_id: short_id("cnf", &[subject_key, normalized_field]),
        subject_key: subject_key.to_owned(),
        field_path,
        normalized_field: normalized_field.to_owned(),
        kind,
        severity,
        variants,
        preferred_hint,
    })
}

fn classify_conflict(groups: &[Vec<&ClaimRecord>]) -> ConflictKind {
    for i in 0..groups.len() {
        for j in (i + 1)..groups.len() {
            if compare_normalized(
                &groups[i][0].normalized_value,
                &groups[j][0].normalized_value,
            ) == ValueComparison::UnitOrCurrencyMismatch
            {
                return ConflictKind::UnitOrCurrencyMismatch;
            }
        }
    }

    if has_engine_discrepancy(groups) {
        return ConflictKind::EngineDiscrepancy;
    }
    if has_canonical_divergence(groups) {
        return ConflictKind::CanonicalDivergence;
    }
    if has_temporal_staleness(groups) {
        return ConflictKind::TemporalStaleness;
    }
    let distinct_docs: BTreeSet<&str> =
        groups.iter().flatten().map(|c| c.doc_id.as_str()).collect();
    if distinct_docs.len() > 1 {
        ConflictKind::ValueMismatch
    } else {
        ConflictKind::MultiplicityConflict
    }
}

fn has_engine_discrepancy(groups: &[Vec<&ClaimRecord>]) -> bool {
    for i in 0..groups.len() {
        for j in (i + 1)..groups.len() {
            for a in &groups[i] {
                for b in &groups[j] {
                    if a.resource_key == b.resource_key && a.engine != b.engine {
                        return true;
                    }
                }
            }
        }
    }
    false
}

fn has_canonical_divergence(groups: &[Vec<&ClaimRecord>]) -> bool {
    let mut has_canonical_hit = false;
    let mut has_alias_hit = false;
    for group in groups {
        for c in group {
            if let Some(can) = &c.canonical_url {
                let can_key = normalize_resource_key(can);
                if c.resource_key == can_key {
                    has_canonical_hit = true;
                } else {
                    has_alias_hit = true;
                }
            }
        }
    }
    has_canonical_hit && has_alias_hit
}

fn has_temporal_staleness(groups: &[Vec<&ClaimRecord>]) -> bool {
    let min_latest = groups
        .iter()
        .map(|g| g.iter().map(|c| c.observed_at_ms).max().unwrap_or(0))
        .min()
        .unwrap_or(0);
    let max_earliest = groups
        .iter()
        .map(|g| g.iter().map(|c| c.observed_at_ms).min().unwrap_or(0))
        .max()
        .unwrap_or(0);
    max_earliest > min_latest
}

fn severity_for(kind: ConflictKind, claims: &[&ClaimRecord]) -> ConflictSeverity {
    let hosts: BTreeSet<&str> = claims
        .iter()
        .map(|c| c.host.as_str())
        .filter(|h| !h.is_empty())
        .collect();
    match kind {
        ConflictKind::UnitOrCurrencyMismatch | ConflictKind::EngineDiscrepancy => {
            ConflictSeverity::High
        }
        ConflictKind::ValueMismatch => {
            if hosts.len() > 1 {
                ConflictSeverity::High
            } else {
                ConflictSeverity::Medium
            }
        }
        ConflictKind::CanonicalDivergence | ConflictKind::TemporalStaleness => {
            ConflictSeverity::Medium
        }
        ConflictKind::MultiplicityConflict => ConflictSeverity::Low,
    }
}

fn to_variant(group: &[&ClaimRecord]) -> ConflictVariant {
    let mut raw_values = Vec::new();
    for c in group {
        if !raw_values.contains(&c.raw_value) {
            raw_values.push(c.raw_value.clone());
        }
    }
    let sources: BTreeSet<String> = group.iter().map(|c| c.source_url.clone()).collect();
    let formats: BTreeSet<String> = group.iter().map(|c| c.source_kind.format_label()).collect();
    let engines: BTreeSet<String> = group.iter().map(|c| c.engine.clone()).collect();
    let latest_observed_at_ms = group.iter().map(|c| c.observed_at_ms).max().unwrap_or(0);
    ConflictVariant {
        value: group[0].normalized_value.clone(),
        raw_values,
        support_count: group.len(),
        latest_observed_at_ms,
        sources: sources.into_iter().collect(),
        formats: formats.into_iter().collect(),
        engines: engines.into_iter().collect(),
        evidence: group.iter().map(|c| c.evidence.clone()).collect(),
    }
}

fn pick_preferred_hint(
    groups: &[Vec<&ClaimRecord>],
    variants: &[ConflictVariant],
) -> Option<PreferredHint> {
    let mut best: Option<(usize, (usize, usize, u64, usize))> = None;
    for (idx, (group, var)) in groups.iter().zip(variants.iter()).enumerate() {
        let verified = group.iter().filter(|c| c.evidence.verified).count();
        let canonical_hits = group
            .iter()
            .filter(|c| {
                c.canonical_url
                    .as_ref()
                    .is_some_and(|u| normalize_resource_key(u) == c.resource_key)
            })
            .count();
        let score = (
            verified,
            canonical_hits,
            var.latest_observed_at_ms,
            var.support_count,
        );
        if best.is_none_or(|(_, best_score)| score > best_score) {
            best = Some((idx, score));
        }
    }
    let (idx, (verified, canonical_hits, _, _)) = best?;
    let mut reasons = Vec::new();
    if verified > 0 {
        reasons.push("verified visible evidence");
    }
    if canonical_hits > 0 {
        reasons.push("canonical URL source");
    }
    reasons.push("highest recency/support");
    Some(PreferredHint {
        variant_index: idx,
        reason: reasons.join(" + "),
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::evidence::{EvidenceAlignment, ExtractionMethod};
    use crate::model::SourceKind;
    use serde_json::json;

    fn mk_claim(
        doc_id: &str,
        url: &str,
        canonical: Option<&str>,
        engine: &str,
        ts: u64,
        val: &str,
        verified: bool,
    ) -> ClaimRecord {
        let trace = crate::evidence::RawFieldTrace {
            field_path: "/price".into(),
            field_name: "price".into(),
            value: json!(val),
            raw_quote: val.into(),
            attr_name: None,
            method: ExtractionMethod::CssSelector {
                selector: ".price".into(),
                relocated_from: None,
                step: None,
            },
            source_url: None,
            visible_in_dom: verified,
        };
        let span = EvidenceSpan {
            doc_id: doc_id.into(),
            url: url.into(),
            block_id: verified.then(|| format!("{doc_id}#b0")),
            chunk_idx: None,
            start: None,
            end: None,
            excerpt: val.into(),
            confidence: if verified { 1.0 } else { 0.85 },
            alignment: if verified {
                EvidenceAlignment::ExactBlock
            } else {
                EvidenceAlignment::MetadataIsland
            },
            verified,
            field_path: "/price".into(),
            method: trace.method.clone(),
        };
        ClaimRecord::from_trace(
            "sku:PRO",
            &normalize_resource_key(url),
            doc_id,
            url,
            SourceKind::Html,
            engine,
            ts,
            canonical.map(str::to_owned),
            &trace,
            span,
        )
    }

    #[test]
    fn classifies_engine_period_and_temporal_conflicts_preserving_all_variants() {
        let a = mk_claim(
            "d1",
            "https://ex.com/pro",
            None,
            "http",
            100,
            "$49/mo",
            false,
        );
        let b = mk_claim(
            "d2",
            "https://ex.com/pro",
            None,
            "browser",
            200,
            "$59/mo",
            true,
        );
        let engine_conf = detect_conflicts(&[a, b]);
        assert_eq!(engine_conf[0].kind, ConflictKind::EngineDiscrepancy);
        assert_eq!(engine_conf[0].variants.len(), 2);
        assert_eq!(
            engine_conf[0]
                .preferred_hint
                .as_ref()
                .unwrap()
                .variant_index,
            1
        );

        let m = mk_claim(
            "d1",
            "https://ex.com/pro",
            None,
            "http",
            100,
            "$49/mo",
            true,
        );
        let y = mk_claim(
            "d2",
            "https://docs.ex.com/pricing",
            None,
            "http",
            100,
            "$490/yr",
            true,
        );
        let unit_conf = detect_conflicts(&[m, y]);
        assert_eq!(unit_conf[0].kind, ConflictKind::UnitOrCurrencyMismatch);
        assert_eq!(unit_conf[0].variants.len(), 2);
    }
}
