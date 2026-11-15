//! Deterministic entity anchoring (`SubjectKey`), claim records, and multi-source corroboration.

use std::collections::{BTreeMap, BTreeSet};

use serde::{Deserialize, Serialize};
use serde_json::Value;

use crate::citation::CitationEdge;
use crate::evidence::{EvidenceSpan, RawFieldTrace};
use crate::hash::{host_of, normalize_resource_key, short_id};
use crate::independence::{SourceLineage, independent_roots_for_claim_group};
use crate::model::SourceKind;
use crate::norm::{NormalizedValue, normalize_field_key, normalize_field_value};
use crate::temporal::TemporalSemantics;

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
    let trace_refs: Vec<&RawFieldTrace> = traces.iter().collect();
    if let Some(key) = resolve_identity_key(configured_keys, json_ld, &trace_refs) {
        return key;
    }
    resolve_fallback_subject_key(canonical_url, resource_key)
}

/// Resolve per-trace entity anchors so multi-product/list pages isolate sibling entities while
/// single-entity pages keep cross-format claims unified.
pub fn resolve_entity_subject_keys(
    configured_keys: &[String],
    json_ld: &[Value],
    traces: &[RawFieldTrace],
    canonical_url: Option<&str>,
    resource_key: &str,
) -> (SubjectKey, Vec<SubjectKey>) {
    let trace_groups: Vec<TraceEntityGroup> = traces.iter().map(entity_group_of).collect();
    let mut json_ld_ids: BTreeSet<usize> = (0..json_ld.len()).collect();
    let mut microdata_ids = BTreeSet::new();
    let mut extract_ids = BTreeSet::new();
    let mut grouped: BTreeMap<TraceEntityGroup, Vec<&RawFieldTrace>> = BTreeMap::new();

    for (trace, group) in traces.iter().zip(trace_groups.iter()) {
        match *group {
            TraceEntityGroup::JsonLd(idx) => {
                json_ld_ids.insert(idx);
            }
            TraceEntityGroup::Microdata(idx) => {
                microdata_ids.insert(idx);
            }
            TraceEntityGroup::ExtractItem(idx) => {
                extract_ids.insert(idx);
            }
            TraceEntityGroup::Page => {}
        }
        grouped.entry(*group).or_default().push(trace);
    }

    let multi_json_ld = json_ld_ids.len() > 1;
    let multi_microdata = microdata_ids.len() > 1;
    let multi_extract = extract_ids.len() > 1;
    if !multi_json_ld && !multi_microdata && !multi_extract {
        let page_key = resolve_subject_key(
            configured_keys,
            json_ld,
            traces,
            canonical_url,
            resource_key,
        );
        let per_trace = vec![page_key.clone(); traces.len()];
        return (page_key, per_trace);
    }

    let mut resolved_groups: BTreeMap<TraceEntityGroup, SubjectKey> = BTreeMap::new();
    let mut primary_entity_keys: BTreeSet<String> = BTreeSet::new();

    let mut assign_group = |group: TraceEntityGroup, multi_in_fmt: bool| {
        let group_traces = grouped.get(&group).map(Vec::as_slice).unwrap_or(&[]);
        let json_slice: &[Value] = match group {
            TraceEntityGroup::JsonLd(idx) => {
                json_ld.get(idx).map(std::slice::from_ref).unwrap_or(&[])
            }
            _ => &[],
        };
        if let Some(key) = resolve_identity_key(configured_keys, json_slice, group_traces) {
            if !is_boilerplate_entity(json_slice, group_traces) {
                primary_entity_keys.insert(key.key.clone());
            }
            resolved_groups.insert(group, key);
        } else if multi_in_fmt {
            let tag = group.fragment_tag();
            let raw = format!("{resource_key}#{tag}");
            let key = SubjectKey {
                key: format!("resource:{raw}"),
                source: SubjectKeySource::ResourceKey,
                raw,
            };
            if !is_boilerplate_entity(json_slice, group_traces) {
                primary_entity_keys.insert(key.key.clone());
            }
            resolved_groups.insert(group, key);
        }
    };

    for idx in &json_ld_ids {
        assign_group(TraceEntityGroup::JsonLd(*idx), multi_json_ld);
    }
    for idx in &microdata_ids {
        assign_group(TraceEntityGroup::Microdata(*idx), multi_microdata);
    }
    for idx in &extract_ids {
        assign_group(TraceEntityGroup::ExtractItem(*idx), multi_extract);
    }

    let page_traces = grouped
        .get(&TraceEntityGroup::Page)
        .map(Vec::as_slice)
        .unwrap_or(&[]);
    let page_key = if primary_entity_keys.len() == 1 {
        let sole = primary_entity_keys.iter().next().unwrap();
        resolved_groups
            .values()
            .find(|k| &k.key == sole)
            .cloned()
            .unwrap_or_else(|| resolve_fallback_subject_key(canonical_url, resource_key))
    } else if let Some(key) = resolve_identity_key(configured_keys, &[], page_traces) {
        key
    } else {
        resolve_fallback_subject_key(canonical_url, resource_key)
    };

    let per_trace = trace_groups
        .into_iter()
        .map(|g| {
            resolved_groups
                .get(&g)
                .cloned()
                .unwrap_or_else(|| page_key.clone())
        })
        .collect();
    (page_key, per_trace)
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
enum TraceEntityGroup {
    JsonLd(usize),
    Microdata(usize),
    ExtractItem(usize),
    Page,
}

impl TraceEntityGroup {
    fn fragment_tag(self) -> String {
        match self {
            Self::JsonLd(idx) => format!("json-ld/{idx}"),
            Self::Microdata(idx) => format!("microdata/{idx}"),
            Self::ExtractItem(idx) => format!("item/{idx}"),
            Self::Page => "page".to_owned(),
        }
    }
}

fn entity_group_of(trace: &RawFieldTrace) -> TraceEntityGroup {
    if let Some(rest) = trace.field_path.strip_prefix("/json-ld/")
        && let Some(idx) = parse_leading_index(rest)
    {
        return TraceEntityGroup::JsonLd(idx);
    }
    if let Some(rest) = trace.field_path.strip_prefix("/microdata/")
        && let Some(idx) = parse_leading_index(rest)
    {
        return TraceEntityGroup::Microdata(idx);
    }
    if matches!(
        trace.method,
        crate::evidence::ExtractionMethod::CssSelector { .. }
            | crate::evidence::ExtractionMethod::XPathSelector { .. }
    ) && let Some(rest) = trace.field_path.strip_prefix('/')
        && let Some(idx) = parse_leading_index(rest)
    {
        return TraceEntityGroup::ExtractItem(idx);
    }
    TraceEntityGroup::Page
}

fn parse_leading_index(rest: &str) -> Option<usize> {
    let seg = rest.split('/').next()?;
    (!seg.is_empty() && seg.bytes().all(|b| b.is_ascii_digit()))
        .then(|| seg.parse().ok())
        .flatten()
}

fn is_boilerplate_entity(json_ld: &[Value], traces: &[&RawFieldTrace]) -> bool {
    const BOILERPLATE: &[&str] = &[
        "website",
        "webpage",
        "organization",
        "breadcrumblist",
        "searchaction",
        "sitenavigationelement",
    ];
    let has_boilerplate_type = |t: &str| {
        let lower = t.to_ascii_lowercase();
        BOILERPLATE
            .iter()
            .any(|b| lower == *b || lower.ends_with(&format!("/{b}")))
    };
    if let Some(obj) = json_ld.first().and_then(Value::as_object)
        && let Some(ty) = obj.get("@type").and_then(|v| {
            v.as_str()
                .or_else(|| v.as_array().and_then(|a| a.first()?.as_str()))
        })
    {
        return has_boilerplate_type(ty);
    }
    for trace in traces {
        if let crate::evidence::ExtractionMethod::JsonLd {
            entity_type: Some(ty),
            ..
        } = &trace.method
        {
            return has_boilerplate_type(ty);
        }
    }
    false
}

fn resolve_identity_key(
    configured_keys: &[String],
    json_ld: &[Value],
    traces: &[&RawFieldTrace],
) -> Option<SubjectKey> {
    for wanted in configured_keys {
        let wanted_norm = normalize_field_key(wanted);
        if let Some(val) = find_key_value(&wanted_norm, json_ld, traces) {
            return Some(SubjectKey {
                key: format!("{wanted_norm}:{}", val.trim().to_ascii_uppercase()),
                source: SubjectKeySource::ExplicitConfig,
                raw: val,
            });
        }
    }

    for obj in json_ld.iter().filter_map(Value::as_object) {
        if let Some(id) = obj.get("@id").and_then(Value::as_str)
            && !id.trim().is_empty()
            && !id.trim().starts_with("_:")
        {
            let norm = normalize_resource_key(id);
            return Some(SubjectKey {
                key: format!("id:{norm}"),
                source: SubjectKeySource::JsonLdId,
                raw: id.trim().to_owned(),
            });
        }
    }
    for trace in traces {
        if let crate::evidence::ExtractionMethod::JsonLd {
            entity_id: Some(id),
            ..
        } = &trace.method
            && !id.trim().is_empty()
            && !id.trim().starts_with("_:")
        {
            let norm = normalize_resource_key(id);
            return Some(SubjectKey {
                key: format!("id:{norm}"),
                source: SubjectKeySource::JsonLdId,
                raw: id.trim().to_owned(),
            });
        }
    }

    for code in BUILTIN_PRODUCT_KEYS {
        if let Some(val) = find_key_value(code, json_ld, traces) {
            return Some(SubjectKey {
                key: format!("{code}:{}", val.trim().to_ascii_uppercase()),
                source: SubjectKeySource::ProductCode,
                raw: val,
            });
        }
    }
    None
}

fn resolve_fallback_subject_key(canonical_url: Option<&str>, resource_key: &str) -> SubjectKey {
    if let Some(can) = canonical_url.filter(|c| !c.trim().is_empty())
        && crate::model::DocNode::is_trusted_canonical(can, resource_key)
    {
        let key = normalize_resource_key(can);
        return SubjectKey {
            key: format!("canonical:{key}"),
            source: SubjectKeySource::CanonicalCluster,
            raw: can.to_owned(),
        };
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
    traces: &[&RawFieldTrace],
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

pub const DEFAULT_CONTRACT_ID: &str = "default";

fn default_contract_id() -> String {
    DEFAULT_CONTRACT_ID.to_string()
}

fn is_default_contract(s: &str) -> bool {
    s.is_empty() || s == DEFAULT_CONTRACT_ID
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
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub temporal: Option<TemporalSemantics>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub canonical_url: Option<String>,
    pub evidence: EvidenceSpan,
    #[serde(
        default = "default_contract_id",
        skip_serializing_if = "is_default_contract"
    )]
    pub contract_id: String,
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
            temporal: None,
            canonical_url,
            evidence,
            contract_id: default_contract_id(),
        }
    }

    pub fn with_temporal(mut self, temporal: TemporalSemantics) -> Self {
        self.temporal = Some(temporal);
        self
    }

    pub fn with_contract(mut self, contract_id: impl Into<String>) -> Self {
        let c = contract_id.into();
        self.contract_id = if c.trim().is_empty() {
            default_contract_id()
        } else {
            c
        };
        self
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
    #[serde(default)]
    pub independent_sources: usize,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub origin_roots: Vec<String>,
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
    build_fact_clusters_with_lineage(claims, max_claims_per_bucket, &BTreeMap::new(), &[])
}

/// Aggregate claims into corroborated clusters while collapsing dependent/derived sources via `lineage`.
pub fn build_fact_clusters_with_lineage(
    claims: &[ClaimRecord],
    max_claims_per_bucket: usize,
    lineage: &BTreeMap<String, SourceLineage>,
    citations: &[CitationEdge],
) -> Vec<FactCluster> {
    build_fact_clusters_with_lineage_checked(claims, max_claims_per_bucket, lineage, citations).0
}

/// Variant of [`build_fact_clusters_with_lineage`] that also reports whether any bucket hit `max_claims_per_bucket`.
pub fn build_fact_clusters_with_lineage_checked(
    claims: &[ClaimRecord],
    max_claims_per_bucket: usize,
    lineage: &BTreeMap<String, SourceLineage>,
    citations: &[CitationEdge],
) -> (Vec<FactCluster>, bool) {
    let mut buckets: BTreeMap<(String, String, String), Vec<&ClaimRecord>> = BTreeMap::new();
    let mut truncated = false;
    for claim in claims {
        let key = (
            claim.subject_key.clone(),
            claim.normalized_field.clone(),
            claim.normalized_value.bucket_key(),
        );
        let slot = buckets.entry(key).or_default();
        if slot.len() < max_claims_per_bucket {
            slot.push(claim);
        } else {
            truncated = true;
        }
    }

    let clusters = buckets
        .into_iter()
        .map(|((subject_key, normalized_field, val_key), group)| {
            let distinct_sources: BTreeSet<&str> =
                group.iter().map(|c| c.doc_id.as_str()).collect();
            let origin_roots = independent_roots_for_claim_group(&group, lineage, citations);
            let independent_sources = origin_roots.len();
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
                independent_sources,
                origin_roots,
                distinct_hosts: distinct_hosts.len(),
                distinct_formats: distinct_formats.into_iter().collect(),
                claim_ids: group.iter().map(|c| c.claim_id.clone()).collect(),
                evidence: group.iter().map(|c| c.evidence.clone()).collect(),
            }
        })
        .collect();
    (clusters, truncated)
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
