//! Cross-time version lineage for the same logical document across snapshots and URL moves.

use serde::{Deserialize, Serialize};

/// Classification of how a document version transitioned from its predecessor.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum VersionChangeKind {
    Unchanged,
    NoiseOnly,
    Substantive,
    UrlMoved,
    Removed,
}

/// Block-level diff counters aligned with `pagesift-diff::DiffStats`.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct VersionDiffStats {
    pub old_blocks: usize,
    pub new_blocks: usize,
    pub unchanged: usize,
    pub added: usize,
    pub removed: usize,
    pub modified: usize,
    pub moved: usize,
    pub noise: usize,
    pub substantive: usize,
}

/// Compact snapshot metadata entry stored per resource timeline (`ns::GRAPH_*` or job history).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct VersionRecord {
    pub resource_key: String,
    pub doc_id: String,
    pub url: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub canonical_url: Option<String>,
    pub observed_at_ms: u64,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub content_hash: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub text_hash: Option<String>,
    pub status_code: u16,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub etag: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub last_modified: Option<String>,
}

/// Directed temporal version transition edge linking two observations of a logical resource.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct VersionEdge {
    pub resource_key: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub prev_doc_id: Option<String>,
    pub curr_doc_id: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub prev_url: Option<String>,
    pub curr_url: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub prev_observed_at_ms: Option<u64>,
    pub curr_observed_at_ms: u64,
    pub change_kind: VersionChangeKind,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub diff_stats: Option<VersionDiffStats>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub diff_unavailable_reason: Option<String>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub changed_block_ids: Vec<String>,
}

/// Classify version transition between consecutive records.
pub fn classify_version_change(
    prev: &VersionRecord,
    curr: &VersionRecord,
    diff_stats: Option<&VersionDiffStats>,
) -> VersionChangeKind {
    const HTTP_NOT_FOUND: u16 = 404; // [协议] HTTP 404 Not Found
    const HTTP_GONE: u16 = 410; // [协议] HTTP 410 Gone
    const HTTP_NOT_MODIFIED: u16 = 304; // [协议] HTTP 304 Not Modified
    if matches!(curr.status_code, HTTP_NOT_FOUND | HTTP_GONE) {
        return VersionChangeKind::Removed;
    }
    if curr.status_code == HTTP_NOT_MODIFIED {
        return VersionChangeKind::Unchanged;
    }
    if let Some(stats) = diff_stats {
        if stats.substantive > 0 {
            return VersionChangeKind::Substantive;
        }
        if stats.noise > 0 || stats.moved > 0 {
            return VersionChangeKind::NoiseOnly;
        }
        if prev.url != curr.url {
            return VersionChangeKind::UrlMoved;
        }
        return VersionChangeKind::Unchanged;
    }
    if prev.url != curr.url {
        return VersionChangeKind::UrlMoved;
    }
    if prev.content_hash.is_some() && prev.content_hash == curr.content_hash {
        return VersionChangeKind::Unchanged;
    }
    VersionChangeKind::Substantive
}

/// Build consecutive temporal edges from time-ordered `VersionRecord` entries of one resource.
pub fn link_version_pair(
    prev: &VersionRecord,
    curr: &VersionRecord,
    diff_stats: Option<VersionDiffStats>,
    changed_block_ids: Vec<String>,
) -> VersionEdge {
    let change_kind = classify_version_change(prev, curr, diff_stats.as_ref());
    VersionEdge {
        resource_key: curr.resource_key.clone(),
        prev_doc_id: Some(prev.doc_id.clone()),
        curr_doc_id: curr.doc_id.clone(),
        prev_url: Some(prev.url.clone()),
        curr_url: curr.url.clone(),
        prev_observed_at_ms: Some(prev.observed_at_ms),
        curr_observed_at_ms: curr.observed_at_ms,
        change_kind,
        diff_stats,
        diff_unavailable_reason: None,
        changed_block_ids,
    }
}

/// Connect chronologically sorted `VersionRecord` snapshots for one logical resource.
pub fn build_version_chain(records: &[VersionRecord]) -> Vec<VersionEdge> {
    if records.len() < 2 {
        return Vec::new();
    }
    let mut sorted = records.to_vec();
    sorted.sort_by_key(|r| r.observed_at_ms);
    sorted
        .windows(2)
        .map(|w| link_version_pair(&w[0], &w[1], None, Vec::new()))
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn rec(id: &str, url: &str, ts: u64, hash: &str, status: u16) -> VersionRecord {
        VersionRecord {
            resource_key: "https://example.com/pricing".into(),
            doc_id: id.into(),
            url: url.into(),
            canonical_url: Some("https://example.com/pricing".into()),
            observed_at_ms: ts,
            content_hash: Some(hash.into()),
            text_hash: None,
            status_code: status,
            etag: None,
            last_modified: None,
        }
    }

    #[test]
    fn bridges_url_move_noise_and_removal() {
        let records = vec![
            rec("d1", "https://example.com/2025/pricing", 100, "h1", 200),
            rec("d2", "https://example.com/pricing", 200, "h1", 200),
            rec("d3", "https://example.com/pricing", 300, "h2", 200),
            rec("d4", "https://example.com/pricing", 400, "h2", 410),
        ];
        let edges = build_version_chain(&records);
        assert_eq!(edges.len(), 3);
        assert_eq!(edges[0].change_kind, VersionChangeKind::UrlMoved);
        assert_eq!(edges[1].change_kind, VersionChangeKind::Substantive);
        assert_eq!(edges[2].change_kind, VersionChangeKind::Removed);
    }
}
