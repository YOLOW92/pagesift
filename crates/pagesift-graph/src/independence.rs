//! Source independence and derivation lineage analysis.
//!
//! Distinguishes genuinely independent supporting sources from aliases, content mirrors,
//! archive/attachment derivatives, and secondary pages citing a primary source.

use std::collections::{BTreeMap, BTreeSet};

use serde::{Deserialize, Serialize};

use crate::citation::{CitationEdge, LinkRole};
use crate::corroborate::ClaimRecord;
use crate::hash::{host_of, normalize_resource_key};
use crate::model::DocNode;
use crate::provenance::{ProvenanceEdge, RedirectKind};

const MAX_LINEAGE_HOPS: usize = 64; // [策略] 来源派生链溯源最大跳数

/// How a document or claim source relates to its underlying evidentiary origin.
#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "camelCase")]
pub enum OriginRelation {
    /// No derivation from another known source in the graph was observed.
    Independent,
    /// Same-host canonical alias pointing to `canonical_key`.
    CanonicalAlias { canonical_key: String },
    /// Same-host permanent redirect (`301`/`308`) pointing to `target_key`.
    PermanentRedirect { target_key: String },
    /// Content duplicate (`DuplicateOf`) mirroring `primary_key`.
    DuplicateMirror { primary_key: String },
    /// Extracted attachment, archive member, email part, or embedded asset of `parent_key`.
    DerivedArtifact {
        parent_key: String,
        relation: String,
    },
    /// Secondary page that explicitly cites `cited_key` while repeating the same claim.
    SecondaryCitation { cited_key: String },
}

impl OriginRelation {
    pub fn is_independent(&self) -> bool {
        matches!(self, Self::Independent)
    }
}

/// Computed origin lineage for one `resource_key`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct SourceLineage {
    pub resource_key: String,
    pub origin_root_key: String,
    pub relation: OriginRelation,
    pub independent: bool,
}

/// Build a map from `resource_key` to its primary origin root using provenance edges.
pub fn compute_source_lineage(
    nodes: &[DocNode],
    provenance: &[ProvenanceEdge],
) -> BTreeMap<String, SourceLineage> {
    let mut parent_map: BTreeMap<String, (String, OriginRelation)> = BTreeMap::new();

    let doc_id_to_key: BTreeMap<&str, String> = nodes
        .iter()
        .map(|n| (n.doc_id.as_str(), n.resource_key.clone()))
        .collect();

    for edge in provenance {
        match edge {
            ProvenanceEdge::CanonicalizesTo {
                source_url,
                canonical_url,
                cross_host,
                ..
            } if !cross_host => {
                let src = normalize_resource_key(source_url);
                let dst = normalize_resource_key(canonical_url);
                if src != dst && !src.is_empty() && !dst.is_empty() {
                    parent_map.insert(
                        src,
                        (
                            dst.clone(),
                            OriginRelation::CanonicalAlias { canonical_key: dst },
                        ),
                    );
                }
            }
            ProvenanceEdge::Redirect {
                from_url,
                to_url,
                redirect_kind: RedirectKind::HttpPermanent,
                ..
            } if host_of(from_url) == host_of(to_url) => {
                let src = normalize_resource_key(from_url);
                let dst = normalize_resource_key(to_url);
                if src != dst && !src.is_empty() && !dst.is_empty() {
                    parent_map.insert(
                        src,
                        (
                            dst.clone(),
                            OriginRelation::PermanentRedirect { target_key: dst },
                        ),
                    );
                }
            }
            ProvenanceEdge::DuplicateOf {
                source_url,
                canonical_url,
                ..
            } => {
                let src = normalize_resource_key(source_url);
                let dst = normalize_resource_key(canonical_url);
                if src != dst && !src.is_empty() && !dst.is_empty() {
                    parent_map.entry(src).or_insert((
                        dst.clone(),
                        OriginRelation::DuplicateMirror { primary_key: dst },
                    ));
                }
            }
            ProvenanceEdge::DerivedFrom {
                doc_id,
                source_url,
                parent_doc_id,
                relation,
            } => {
                let child_k = doc_id_to_key
                    .get(doc_id.as_str())
                    .cloned()
                    .unwrap_or_default();
                let parent_k = parent_doc_id
                    .as_deref()
                    .and_then(|pid| doc_id_to_key.get(pid).cloned())
                    .unwrap_or_else(|| normalize_resource_key(source_url));
                if !child_k.is_empty() && !parent_k.is_empty() && child_k != parent_k {
                    parent_map.entry(child_k).or_insert((
                        parent_k.clone(),
                        OriginRelation::DerivedArtifact {
                            parent_key: parent_k,
                            relation: relation.clone(),
                        },
                    ));
                }
            }
            ProvenanceEdge::AttachmentOf {
                attachment_url,
                parent_url,
                member_path,
                ..
            } => {
                let att = normalize_resource_key(attachment_url);
                let par = normalize_resource_key(parent_url);
                if att != par && !att.is_empty() && !par.is_empty() {
                    let rel = if member_path.is_some() {
                        "archive_or_mail_member"
                    } else {
                        "attachment"
                    };
                    parent_map.entry(att).or_insert((
                        par.clone(),
                        OriginRelation::DerivedArtifact {
                            parent_key: par,
                            relation: rel.into(),
                        },
                    ));
                }
            }
            _ => {}
        }
    }

    let mut all_keys: BTreeSet<String> = nodes.iter().map(|n| n.resource_key.clone()).collect();
    for (k, (p, _)) in &parent_map {
        all_keys.insert(k.clone());
        all_keys.insert(p.clone());
    }

    let mut out = BTreeMap::new();
    for key in all_keys {
        let (root, rel) = resolve_root(&key, &parent_map);
        let independent = root == key && rel.is_independent();
        out.insert(
            key.clone(),
            SourceLineage {
                resource_key: key,
                origin_root_key: root,
                relation: rel,
                independent,
            },
        );
    }
    out
}

fn resolve_root(
    start: &str,
    parent_map: &BTreeMap<String, (String, OriginRelation)>,
) -> (String, OriginRelation) {
    let Some((_, first_rel)) = parent_map.get(start) else {
        return (start.to_owned(), OriginRelation::Independent);
    };
    let mut curr = start;
    let mut visited = BTreeSet::new();
    visited.insert(curr);
    for _ in 0..MAX_LINEAGE_HOPS {
        let Some((next, _)) = parent_map.get(curr) else {
            break;
        };
        if !visited.insert(next.as_str()) {
            // Break provenance cycles deterministically on the lexicographically smallest member.
            let cycle_rep = visited.iter().min().copied().unwrap_or(curr);
            return (cycle_rep.to_owned(), first_rel.clone());
        }
        curr = next;
    }
    (curr.to_owned(), first_rel.clone())
}

/// Compute independent origin root keys for a group of claims asserting the same fact.
///
/// Accounts for:
/// 1. Provenance lineage (canonical aliases, permanent redirects, duplicate mirrors, attachments/archives);
/// 2. Direct citation propagation: if claim source A links to claim source B (`Citation` or `Attachment`
///    or `External` role) within the same fact group, A and B share B's origin root.
pub fn independent_roots_for_claim_group(
    group: &[&ClaimRecord],
    lineage: &BTreeMap<String, SourceLineage>,
    citations: &[CitationEdge],
) -> Vec<String> {
    if group.is_empty() {
        return Vec::new();
    }
    let group_roots: BTreeSet<String> = group
        .iter()
        .map(|c| {
            lineage
                .get(&c.resource_key)
                .map(|l| l.origin_root_key.clone())
                .unwrap_or_else(|| c.resource_key.clone())
        })
        .collect();

    // Build multi-target citation adjacency among roots present in this same fact cluster.
    // Navigation/header/footer/internal chrome links never collapse source independence.
    let mut adj: BTreeMap<String, BTreeSet<String>> = BTreeMap::new();
    for edge in citations {
        let is_citation_role = match edge.role {
            LinkRole::Citation | LinkRole::Attachment => true,
            LinkRole::External => {
                edge.samples.is_empty()
                    || edge.samples.iter().any(|s| {
                        matches!(
                            s.section,
                            crate::citation::LinkSection::MainContent
                                | crate::citation::LinkSection::ReferenceList
                                | crate::citation::LinkSection::Unknown
                        )
                    })
            }
            _ => false,
        };
        if !is_citation_role {
            continue;
        }
        let from_root = lineage
            .get(&edge.from_resource_key)
            .map(|l| l.origin_root_key.as_str())
            .unwrap_or(&edge.from_resource_key);
        let to_root = lineage
            .get(&edge.to_resource_key)
            .map(|l| l.origin_root_key.as_str())
            .unwrap_or(&edge.to_resource_key);
        if from_root != to_root && group_roots.contains(from_root) && group_roots.contains(to_root)
        {
            adj.entry(from_root.to_owned())
                .or_default()
                .insert(to_root.to_owned());
        }
    }

    // Roots reachable via non-cyclic citations collapse onto their sink primary roots;
    // mutually citing cycles contribute a single deterministic cycle representative.
    let mut final_roots = BTreeSet::new();
    for root in &group_roots {
        let mut stack = vec![root.as_str()];
        let mut visited = BTreeSet::new();
        let mut sinks = BTreeSet::new();
        let mut cycle_members = BTreeSet::new();
        while let Some(curr) = stack.pop() {
            if !visited.insert(curr) {
                continue;
            }
            if let Some(targets) = adj.get(curr)
                && !targets.is_empty()
            {
                for target in targets {
                    if target.as_str() == root.as_str() {
                        cycle_members.insert(curr.to_owned());
                    }
                    if !visited.contains(target.as_str()) {
                        stack.push(target.as_str());
                    }
                }
            } else {
                sinks.insert(curr.to_owned());
            }
        }
        if !sinks.is_empty() {
            final_roots.extend(sinks);
        } else if let Some(rep) = cycle_members.into_iter().next() {
            final_roots.insert(rep);
        } else {
            final_roots.insert(root.clone());
        }
    }
    final_roots.into_iter().collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::evidence::{EvidenceAlignment, EvidenceSpan, ExtractionMethod, RawFieldTrace};
    use crate::model::SourceKind;
    use serde_json::json;

    #[test]
    fn collapses_mirrors_attachments_and_citing_pages_into_independent_roots() {
        let provenance = vec![
            ProvenanceEdge::DuplicateOf {
                source_url: "https://mirror.example.org/post".into(),
                canonical_url: "https://official.example.com/post".into(),
                reason: "content_duplicate".into(),
            },
            ProvenanceEdge::AttachmentOf {
                attachment_url: "https://official.example.com/files/spec.pdf".into(),
                parent_url: "https://official.example.com/post".into(),
                parent_doc_id: Some("d1".into()),
                media_id: None,
                member_path: None,
            },
        ];
        let citations = vec![CitationEdge {
            from_doc_id: "d_blog".into(),
            from_resource_key: "https://blog.thirdparty.io/review".into(),
            to_url: "https://official.example.com/post".into(),
            to_resource_key: "https://official.example.com/post".into(),
            role: LinkRole::External,
            occurrences: 1,
            samples: Vec::new(),
            crawled: true,
        }];
        let lineage = compute_source_lineage(&[], &provenance);
        let mk = |doc_id: &str, url: &str| {
            let trace = RawFieldTrace {
                field_path: "/price".into(),
                field_name: "price".into(),
                value: json!("$89"),
                raw_quote: "$89".into(),
                attr_name: None,
                method: ExtractionMethod::CssSelector {
                    selector: ".p".into(),
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
                end: Some(3),
                excerpt: "$89".into(),
                confidence: 1.0,
                alignment: EvidenceAlignment::ExactBlock,
                verified: true,
                field_path: "/price".into(),
                method: trace.method.clone(),
            };
            ClaimRecord::from_trace(
                "sku:X1",
                &normalize_resource_key(url),
                doc_id,
                url,
                SourceKind::Html,
                "http",
                1000,
                None,
                &trace,
                span,
            )
        };
        let c_official = mk("d1", "https://official.example.com/post");
        let c_mirror = mk("d2", "https://mirror.example.org/post");
        let c_pdf = mk("d3", "https://official.example.com/files/spec.pdf");
        let c_blog = mk("d4", "https://blog.thirdparty.io/review");
        let c_indep = mk("d5", "https://independent-lab.org/bench");

        let group_derived = vec![&c_official, &c_mirror, &c_pdf, &c_blog];
        let roots1 = independent_roots_for_claim_group(&group_derived, &lineage, &citations);
        assert_eq!(roots1, vec!["https://official.example.com/post"]);

        let group_with_indep = vec![&c_official, &c_mirror, &c_pdf, &c_blog, &c_indep];
        let roots2 = independent_roots_for_claim_group(&group_with_indep, &lineage, &citations);
        assert_eq!(roots2.len(), 2);
    }
}
