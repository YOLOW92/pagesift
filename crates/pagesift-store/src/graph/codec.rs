//! Key scoping and JSON codec helpers for `GraphStore`.

use serde::Serialize;
use serde::de::DeserializeOwned;

use super::*;
use crate::engine::group_key;

pub(crate) fn encode_tenant_prefix(tenant: &str) -> String {
    let hash = blake3::hash(tenant.as_bytes());
    let mut out = String::with_capacity(35); // [协议] 固定前缀 "t:" + 32 位 hex + "\0" 共 35 字节
    out.push_str("t:");
    for b in &hash.as_bytes()[..16] {
        let _ = write!(out, "{b:02x}");
    }
    out.push('\0');
    out
}

pub(crate) fn scope_prefixed_key(scope: &str, prefix: &[u8], body: &str) -> Vec<u8> {
    let mut out = Vec::with_capacity(scope.len() + prefix.len() + body.len());
    out.extend_from_slice(scope.as_bytes());
    out.extend_from_slice(prefix);
    out.extend_from_slice(body.as_bytes());
    out
}

pub(super) fn scope_list_key(scope: &str, key: &str) -> Vec<u8> {
    let mut out = Vec::with_capacity(scope.len() + key.len());
    out.extend_from_slice(scope.as_bytes());
    out.extend_from_slice(key.as_bytes());
    out
}

pub(crate) enum PreparedSnap {
    Store {
        hash: ContentHash,
        raw_len: u32,
        compressed: Option<Vec<u8>>,
        raw_bytes: Vec<u8>,
    },
    Unavailable {
        reason: String,
        warning: Option<String>,
    },
}

pub(crate) struct PreparedVersionCommit {
    pub(crate) resource_key: String,
    pub(crate) doc_id: String,
    pub(crate) expected_prev_doc_id: Option<String>,
    pub(crate) alias_keys: Vec<String>,
    pub(crate) candidate_keys: BTreeSet<String>,
    pub(crate) rec_bytes: Vec<u8>,
    pub(crate) edge_bytes: Option<Vec<u8>>,
    pub(crate) edge_resource_key: String,
    pub(crate) ts: u64,
    pub(crate) compress_level: i32,
    pub(crate) prepared_snap: PreparedSnap,
}

pub(crate) fn latest_tail_in_tx<T: TxOps + ?Sized>(
    tx: &T,
    scope: &str,
    resource_key: &str,
) -> Result<Option<VersionRecord>> {
    let list_key = scope_list_key(scope, resource_key);
    let Some(g) = tx.group(ns::GRAPH_NODE, &list_key) else {
        return Ok(None);
    };
    if g.live == 0 || g.next <= g.head {
        return Ok(None);
    }
    for seq in (g.head..g.next).rev() {
        let key = group_key(&list_key, &seq.to_be_bytes())?;
        if let Some((_, raw)) = tx.get(ns::GRAPH_NODE, &key)? {
            return decode_json(&raw, "version record").map(Some);
        }
    }
    Ok(None)
}

pub(crate) fn record_ts(record: &VersionRecord) -> u64 {
    if record.observed_at_ms > 0 {
        record.observed_at_ms
    } else {
        to_ms(std::time::SystemTime::now())
    }
}

pub(crate) fn initial_key(url_or_key: &str) -> String {
    if url_or_key.contains("://") {
        normalize_resource_key(url_or_key)
    } else {
        url_or_key.to_string()
    }
}

pub(crate) fn encode_json<T: Serialize + ?Sized>(value: &T, label: &str) -> Result<Vec<u8>> {
    serde_json::to_vec(value).map_err(|e| Error::Corrupt(format!("encode {label}: {e}")))
}

pub(crate) fn decode_json<T: DeserializeOwned>(raw: &[u8], label: &str) -> Result<T> {
    serde_json::from_slice(raw).map_err(|e| Error::Corrupt(format!("decode {label}: {e}")))
}
