//! Deterministic entity anchoring (`SubjectKey`), claim records, and multi-source corroboration.

use std::collections::{BTreeMap, BTreeSet};

use serde::{Deserialize, Serialize};
use serde_json::Value;

use crate::evidence::{EvidenceSpan, RawFieldTrace};
use crate::hash::{host_of, normalize_resource_key, short_id};
use crate::model::SourceKind;
use crate::norm::{NormalizedValue, normalize_field_key, normalize_field_value};

const BUILTIN_PRODUCT_KEYS: &[&str] = &["sku", "gtin", "gtin13", "isbn", "doi", "lei_code", "mpn"];

/// Provenance of the entity key anchoring claims from a document.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum SubjectKeySource {
    ExplicitConfig,
    JsonLdId,
    ProductCode,
    CanonicalCluster,
    ResourceKey,
}

/// Deterministic anchor grouping facts about the same underlying subject across URLs and formats.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct SubjectKey {
    pub key: String,
    pub source: SubjectKeySource,
    pub raw: String,
}

/// Resolve entity anchor deterministically: explicit `subject_keys` -> JSON-LD `@id`/codes -> `canonical_url` -> `resource_key`.
pub fn resolve_subject_key(
    configured_keys: &[String],
    json_ld: &[Value],
    traces: &[RawFieldTrace],
    canonical_url: Option<&str>,
    resource_key: &str,
) -> SubjectKey {
    for wanted in configured_keys {
        let wanted_norm = normalize_field_key(wanted);
        if let Some(val) = find_key_value(&wanted_norm, json_ld, traces) {
            return SubjectKey {
                key: format!("{wanted_norm}:{}", val.trim().to_ascii_uppercase()),
                source: SubjectKeySource::ExplicitConfig,
                raw: val,
            };
        }
    }

    for obj in json_ld.iter().filter_map(Value::as_object) {
        if let Some(id) = obj.get("@id").and_then(Value::as_str)
            && !id.trim().is_empty()
            && !id.trim().starts_with("_:")
        {
            let norm = normalize_resource_key(id);
            return SubjectKey {
                key: format!("id:{norm}"),
                source: SubjectKeySource::JsonLdId,
                raw: id.trim().to_owned(),
            };
        }
    }

    for code in BUILTIN_PRODUCT_KEYS {
        if let Some(val) = find_key_value(code, json_ld, traces) {
            return SubjectKey {
                key: format!("{code}:{}", val.trim().to_ascii_uppercase()),
                source: SubjectKeySource::ProductCode,
                raw: val,
            };
        }
    }

    if let Some(can) = canonical_url.filter(|c| !c.trim().is_empty()) {
        let can_host = host_of(can);
        let res_host = host_of(resource_key);
        if (!res_host.is_empty() && can_host == res_host)
            || (res_host.is_empty() && can_host.is_empty())
        {
            let key = normalize_resource_key(can);
            return SubjectKey {
                key: format!("canonical:{key}"),
                source: SubjectKeySource::CanonicalCluster,
                raw: can.to_owned(),
            };
        }
    }

    SubjectKey {
        key: format!("resource:{resource_key}"),
        source: SubjectKeySource::ResourceKey,
        raw: resource_key.to_owned(),
    }
}

fn find_key_value(
    wanted_norm: &str,
    json_ld: &[Value],
    traces: &[RawFieldTrace],
) -> Option<String> {
    for trace in traces {
        if normalize_field_key(&trace.field_name) == wanted_norm
            && let Some(s) = scalar_to_nonempty_string(&trace.value)
        {
            return Some(s);
        }
    }
    for item in json_ld {
        if let Some(s) = search_json_key(wanted_norm, item) {
            return Some(s);
        }
    }
    None
}

fn search_json_key(wanted_norm: &str, val: &Value) -> Option<String> {
    match val {
        Value::Object(map) => {
            for (k, v) in map {
                if normalize_field_key(k) == wanted_norm
                    && let Some(s) = scalar_to_nonempty_string(v)
                {
                    return Some(s);
                }
                if matches!(v, Value::Object(_) | Value::Array(_))
                    && let Some(s) = search_json_key(wanted_norm, v)
                {
                    return Some(s);
                }
            }
            None
        }
        Value::Array(arr) => arr
            .iter()
            .find_map(|item| search_json_key(wanted_norm, item)),
        _ => None,
    }
}

fn scalar_to_nonempty_string(v: &Value) -> Option<String> {
    let s = match v {
        Value::String(s) => s.trim().to_owned(),
        Value::Number(n) => n.to_string(),
        _ => return None,
    };
    (!s.is_empty()).then_some(s)
}

/// Normalized claim extracted from one document along with its grounded evidence span.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ClaimRecord {
    pub claim_id: String,
    pub subject_key: String,
    pub resource_key: String,
    pub field_path: String,
    pub normalized_field: String,
    pub normalized_value: NormalizedValue,
    pub raw_value: Value,
    pub doc_id: String,
    pub source_url: String,
    pub host: String,
    pub source_kind: SourceKind,
    pub engine: String,
    pub observed_at_ms: u64,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub canonical_url: Option<String>,
    pub evidence: EvidenceSpan,
}

impl ClaimRecord {
    #[allow(clippy::too_many_arguments)]
    pub fn from_trace(
        subject_key: &str,
        resource_key: &str,
        doc_id: &str,
        source_url: &str,
        source_kind: SourceKind,
        engine: &str,
        observed_at_ms: u64,
        canonical_url: Option<String>,
        trace: &RawFieldTrace,
        evidence: EvidenceSpan,
    ) -> Self {
        let normalized_field = normalize_field_key(&trace.field_name);
        let normalized_value = normalize_field_value(&trace.field_name, &trace.value);
        let claim_id = short_id(
            "clm",
            &[
                subject_key,
                doc_id,
                &trace.field_path,
                &normalized_value.bucket_key(),
            ],
        );
        Self {
            claim_id,
            subject_key: subject_key.to_owned(),
            resource_key: resource_key.to_owned(),
            field_path: trace.field_path.clone(),
            normalized_field,
            normalized_value,
            raw_value: trace.value.clone(),
            doc_id: doc_id.to_owned(),
            source_url: source_url.to_owned(),
            host: host_of(source_url),
            source_kind,
            engine: engine.to_owned(),
            observed_at_ms,
            canonical_url,
            evidence,
        }
    }
}

/// Corroborated agreement cluster where one or more sources assert the same normalized fact.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct FactCluster {
    pub cluster_id: String,
    pub subject_key: String,
    pub normalized_field: String,
    pub value: NormalizedValue,
    pub support_count: usize,
    pub distinct_sources: usize,
    pub distinct_hosts: usize,
    pub distinct_formats: Vec<String>,
    pub claim_ids: Vec<String>,
    pub evidence: Vec<EvidenceSpan>,
}

/// Aggregate claims into corroborated `(subject_key, normalized_field, canonical_value)` clusters.
pub fn build_fact_clusters(
    claims: &[ClaimRecord],
    max_claims_per_bucket: usize,
) -> Vec<FactCluster> {
    let mut buckets: BTreeMap<(String, String, String), Vec<&ClaimRecord>> = BTreeMap::new();
    for claim in claims {
        let key = (
            claim.subject_key.clone(),
            claim.normalized_field.clone(),
            claim.normalized_value.bucket_key(),
        );
        let slot = buckets.entry(key).or_default();
        if slot.len() < max_claims_per_bucket {
            slot.push(claim);
        }
    }

    buckets
        .into_iter()
        .map(|((subject_key, normalized_field, val_key), group)| {
            let distinct_sources: BTreeSet<&str> =
                group.iter().map(|c| c.doc_id.as_str()).collect();
            let distinct_hosts: BTreeSet<&str> = group
                .iter()
                .map(|c| c.host.as_str())
                .filter(|h| !h.is_empty())
                .collect();
            let distinct_formats: BTreeSet<String> =
                group.iter().map(|c| c.source_kind.format_label()).collect();
            FactCluster {
                cluster_id: short_id("fct", &[&subject_key, &normalized_field, &val_key]),
                subject_key,
                normalized_field,
                value: group[0].normalized_value.clone(),
                support_count: group.len(),
                distinct_sources: distinct_sources.len(),
                distinct_hosts: distinct_hosts.len(),
                distinct_formats: distinct_formats.into_iter().collect(),
                claim_ids: group.iter().map(|c| c.claim_id.clone()).collect(),
                evidence: group.iter().map(|c| c.evidence.clone()).collect(),
            }
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::evidence::{EvidenceAlignment, ExtractionMethod};
    use serde_json::json;

    #[test]
    fn resolves_subject_key_priority_and_corroborates_cross_format() {
        let json_ld = vec![json!({
            "@id": "https://example.com/products/pro",
            "sku": "PRO-100",
            "price": "$49/mo"
        })];
        let sk = resolve_subject_key(&[], &json_ld, &[], None, "https://example.com/a");
        assert_eq!(sk.source, SubjectKeySource::JsonLdId);

        let sk_explicit = resolve_subject_key(
            &["sku".into()],
            &json_ld,
            &[],
            None,
            "https://example.com/a",
        );
        assert_eq!(sk_explicit.source, SubjectKeySource::ExplicitConfig);
        assert_eq!(sk_explicit.key, "sku:PRO-100");

        let mk = |doc_id: &str, url: &str, kind: SourceKind, raw: &str| {
            let trace = RawFieldTrace {
                field_path: "/price".into(),
                field_name: "price".into(),
                value: json!(raw),
                raw_quote: raw.into(),
                attr_name: None,
                method: ExtractionMethod::CssSelector {
                    selector: ".price".into(),
                    relocated_from: None,
                    step: None,
                },
                source_url: None,
                visible_in_dom: true,
            };
            let span = EvidenceSpan {
                doc_id: doc_id.into(),
                url: url.into(),
                block_id: Some(format!("{doc_id}#b0")),
                chunk_idx: None,
                start: Some(0),
                end: Some(10),
                excerpt: raw.into(),
                confidence: 1.0,
                alignment: EvidenceAlignment::ExactBlock,
                verified: true,
                field_path: "/price".into(),
                method: trace.method.clone(),
            };
            ClaimRecord::from_trace(
                "sku:PRO-100",
                url,
                doc_id,
                url,
                kind,
                "http",
                100,
                None,
                &trace,
                span,
            )
        };
        let c1 = mk("d1", "https://example.com/p", SourceKind::Html, "$49/mo");
        let c2 = mk(
            "d2",
            "https://example.com/spec.pdf",
            SourceKind::Pdf,
            "USD 49.00 per month",
        );
        let clusters = build_fact_clusters(&[c1, c2], 64);
        assert_eq!(clusters.len(), 1);
        assert_eq!(clusters[0].distinct_sources, 2);
        assert_eq!(clusters[0].distinct_formats, ["html", "pdf"]);
    }
}
