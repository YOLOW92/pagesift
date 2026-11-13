//! URL, redirect, canonical declaration, duplicate, and derived/attachment lineage edges.

use std::collections::BTreeMap;

use serde::{Deserialize, Serialize};

use crate::hash::{host_of, normalize_resource_key, normalize_url};

/// How one URL transitioned to another during fetch or browser navigation.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum RedirectKind {
    HttpPermanent,
    HttpTemporary,
    MetaRefresh,
    BrowserNav,
}

impl RedirectKind {
    pub fn from_status(status: u16) -> Self {
        match status {
            301 | 308 => Self::HttpPermanent, // [协议] HTTP 永久重定向状态码
            _ => Self::HttpTemporary,
        }
    }
}

/// Where a `canonical` target URL was declared.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum CanonicalSource {
    HtmlLink,
    HttpHeader,
}

/// Directed source-relationship edge linking URLs, snapshots, archives, email parts, and media.
#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "camelCase")]
pub enum ProvenanceEdge {
    Redirect {
        from_url: String,
        to_url: String,
        status: u16,
        redirect_kind: RedirectKind,
    },
    CanonicalizesTo {
        source_url: String,
        canonical_url: String,
        declared_by: CanonicalSource,
        cross_host: bool,
    },
    HasAlternate {
        primary_url: String,
        alt_url: String,
        #[serde(skip_serializing_if = "Option::is_none")]
        media_type: Option<String>,
        #[serde(skip_serializing_if = "Option::is_none")]
        hreflang: Option<String>,
    },
    DuplicateOf {
        source_url: String,
        canonical_url: String,
        reason: String,
    },
    DerivedFrom {
        doc_id: String,
        source_url: String,
        #[serde(skip_serializing_if = "Option::is_none")]
        parent_doc_id: Option<String>,
        relation: String,
    },
    AttachmentOf {
        attachment_url: String,
        parent_url: String,
        #[serde(skip_serializing_if = "Option::is_none")]
        parent_doc_id: Option<String>,
        #[serde(skip_serializing_if = "Option::is_none")]
        media_id: Option<String>,
        #[serde(skip_serializing_if = "Option::is_none")]
        member_path: Option<String>,
    },
}

/// Aliased URLs collapsed onto the same canonical URL target.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct CanonicalCluster {
    pub canonical_url: String,
    pub canonical_key: String,
    pub aliases: Vec<String>,
    pub declared_by: Vec<CanonicalSource>,
    pub cross_host: bool,
}

/// Construct a canonicalization edge with deterministic normalization and cross-host check.
pub fn canonical_edge(
    source_url: &str,
    canonical_url: &str,
    declared_by: CanonicalSource,
) -> Option<ProvenanceEdge> {
    let src = normalize_url(source_url);
    let can = normalize_url(canonical_url);
    if src == can || can.is_empty() {
        return None;
    }
    let cross_host = host_of(&src) != host_of(&can);
    Some(ProvenanceEdge::CanonicalizesTo {
        source_url: src,
        canonical_url: can,
        declared_by,
        cross_host,
    })
}

/// Group `CanonicalizesTo` edges into sorted canonical equivalence clusters.
pub fn cluster_canonicals(edges: &[ProvenanceEdge]) -> Vec<CanonicalCluster> {
    struct Acc {
        canonical_url: String,
        aliases: Vec<String>,
        declared_by: Vec<CanonicalSource>,
        cross_host: bool,
    }
    let mut map: BTreeMap<String, Acc> = BTreeMap::new();
    for edge in edges {
        let ProvenanceEdge::CanonicalizesTo {
            source_url,
            canonical_url,
            declared_by,
            cross_host,
        } = edge
        else {
            continue;
        };
        let key = normalize_resource_key(canonical_url);
        let acc = map.entry(key).or_insert_with(|| Acc {
            canonical_url: canonical_url.clone(),
            aliases: Vec::new(),
            declared_by: Vec::new(),
            cross_host: false,
        });
        if !acc.aliases.contains(source_url) {
            acc.aliases.push(source_url.clone());
        }
        if !acc.declared_by.contains(declared_by) {
            acc.declared_by.push(*declared_by);
        }
        acc.cross_host |= *cross_host;
    }

    map.into_iter()
        .map(|(canonical_key, mut acc)| {
            acc.aliases.sort();
            acc.declared_by.sort();
            CanonicalCluster {
                canonical_url: acc.canonical_url,
                canonical_key,
                aliases: acc.aliases,
                declared_by: acc.declared_by,
                cross_host: acc.cross_host,
            }
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn groups_aliases_into_canonical_cluster() {
        let mut edges = Vec::new();
        edges.extend(canonical_edge(
            "https://shop.example.com/item?utm_source=ad",
            "https://www.example.com/item",
            CanonicalSource::HtmlLink,
        ));
        assert!(
            canonical_edge(
                "https://www.example.com/item?utm_medium=email",
                "https://www.example.com/item",
                CanonicalSource::HtmlLink,
            )
            .is_none()
        );
        edges.extend(canonical_edge(
            "https://www.example.com/item?ref=nav",
            "https://www.example.com/item",
            CanonicalSource::HttpHeader,
        ));
        assert_eq!(edges.len(), 2);
        let clusters = cluster_canonicals(&edges);
        assert_eq!(clusters.len(), 1);
        assert!(clusters[0].cross_host);
        assert_eq!(
            clusters[0].aliases,
            [
                "https://shop.example.com/item",
                "https://www.example.com/item?ref=nav",
            ]
        );
        assert_eq!(
            clusters[0].declared_by,
            [CanonicalSource::HtmlLink, CanonicalSource::HttpHeader]
        );
    }
}
