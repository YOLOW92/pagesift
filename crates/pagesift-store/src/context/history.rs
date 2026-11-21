//! Bounded per-resource point-in-time snapshot history (`chis\x1f{source_id}\x1f{resource_key}`)
//! enabling exact `as_of_ms` historical Context replay across 304 refreshes, edits, and 410 withdrawals.

use pagesift_graph::context::{KnowledgeUnit, UnitStatus};
use serde::{Deserialize, Serialize};

use super::{ContextStore, SEP, decode_zstd_json, encode_zstd_json, validate_ident};
use crate::engine::{AtomicTx, ns};
use crate::{Error, Result};

// [策略] 每个资源保留最近 32 个时间点状态演进快照（连续同内容 304 自动折叠仅占首尾 2 槽）
pub const MAX_RESOURCE_HISTORY_ENTRIES: usize = 32;

/// Point-in-time resource unit snapshot recorded at `observed_at_ms`.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ResourceHistoryEntry {
    pub observed_at_ms: u64,
    /// Live/unverified units at `observed_at_ms`; empty `Vec` represents confirmed withdrawal at `observed_at_ms`.
    pub units: Vec<KnowledgeUnit>,
}

fn same_unit_version_ignoring_obs_time(a: &[KnowledgeUnit], b: &[KnowledgeUnit]) -> bool {
    if a.len() != b.len() {
        return false;
    }
    a.iter().zip(b.iter()).all(|(x, y)| {
        x.unit_id == y.unit_id
            && x.revision_id == y.revision_id
            && x.content_hash == y.content_hash
            && std::mem::discriminant(&x.status) == std::mem::discriminant(&y.status)
    })
}

pub(crate) fn infer_resource_event_ms(
    resource_key: &str,
    units: &[KnowledgeUnit],
    retired_units: &[KnowledgeUnit],
    fallback_ms: u64,
) -> u64 {
    if !units.is_empty() {
        let mut best = 0u64;
        for u in units {
            let status_ts = match &u.status {
                UnitStatus::Unverified { since_ms, .. } => *since_ms,
                UnitStatus::Withdrawn { at_ms, .. } | UnitStatus::Superseded { at_ms, .. } => {
                    *at_ms
                }
                _ => 0,
            };
            best = best.max(u.observed_at_ms).max(status_ts);
        }
        if best > 0 {
            return best;
        }
    }
    let mut retired_ts = 0u64;
    for u in retired_units {
        if u.location.resource_key == resource_key {
            let ts = match &u.status {
                UnitStatus::Withdrawn { at_ms, .. } | UnitStatus::Superseded { at_ms, .. } => {
                    *at_ms
                }
                _ => u.observed_at_ms,
            };
            retired_ts = retired_ts.max(ts);
        }
    }
    if retired_ts > 0 {
        retired_ts
    } else {
        fallback_ms
    }
}

pub(crate) fn merge_history_entry(
    entries: &mut Vec<ResourceHistoryEntry>,
    observed_at_ms: u64,
    units: Vec<KnowledgeUnit>,
) {
    if let Ok(idx) = entries.binary_search_by_key(&observed_at_ms, |e| e.observed_at_ms) {
        entries[idx].units = units;
        return;
    }
    // Compress repeated 304 / identical-content reconfirmations so `[first_obs, latest_obs]` uses at most 2 slots!
    if entries.len() >= 2
        && observed_at_ms > entries[entries.len() - 1].observed_at_ms
        && same_unit_version_ignoring_obs_time(&entries[entries.len() - 1].units, &units)
        && same_unit_version_ignoring_obs_time(
            &entries[entries.len() - 2].units,
            &entries[entries.len() - 1].units,
        )
    {
        let last = entries.last_mut().unwrap();
        last.observed_at_ms = observed_at_ms;
        last.units = units;
        return;
    }

    let pos = entries.partition_point(|e| e.observed_at_ms < observed_at_ms);
    entries.insert(
        pos,
        ResourceHistoryEntry {
            observed_at_ms,
            units,
        },
    );
    if entries.len() > MAX_RESOURCE_HISTORY_ENTRIES {
        let excess = entries.len() - MAX_RESOURCE_HISTORY_ENTRIES;
        entries.drain(0..excess);
    }
}

impl<'a> ContextStore<'a> {
    pub(crate) fn history_key(&self, source_id: &str, resource_key: &str) -> Vec<u8> {
        self.sync_key(&format!("chis{SEP}{source_id}{SEP}"), resource_key)
    }

    pub(crate) fn record_resource_history_in_tx(
        &self,
        tx: &mut AtomicTx<'_>,
        source_id: &str,
        resource_key: &str,
        observed_at_ms: u64,
        units: &[KnowledgeUnit],
        now_ms: u64,
    ) -> Result<()> {
        let hk = self.history_key(source_id, resource_key);
        if hk.len() > u16::MAX as usize {
            return Err(Error::KeyTooLarge(hk.len()));
        }
        let mut entries: Vec<ResourceHistoryEntry> = match tx.get(ns::CONTEXT, &hk)? {
            Some((_, raw)) => decode_zstd_json(
                &raw,
                &format!("resource unit history ({source_id}/{resource_key})"),
            )?,
            None => Vec::new(),
        };
        merge_history_entry(&mut entries, observed_at_ms, units.to_vec());
        let lvl = self.store.config().snapshot_compress_level;
        let encoded = encode_zstd_json(
            &entries,
            lvl,
            &format!("resource unit history ({source_id}/{resource_key})"),
        )?;
        tx.put(ns::CONTEXT, &hk, &encoded, now_ms)?;
        Ok(())
    }

    /// Reconstruct live/unverified units of `source_id` as of `cutoff_ms` (point-in-time historical view).
    ///
    /// Returns `(found_history_coverage, units)`.
    pub fn load_units_as_of(
        &self,
        source_id: &str,
        cutoff_ms: u64,
    ) -> Result<(bool, Vec<KnowledgeUnit>)> {
        let (found, _, units) =
            self.load_units_as_of_filtered(source_id, cutoff_ms, None, usize::MAX)?;
        Ok((found, units))
    }

    /// Stream point-in-time units as of `cutoff_ms` with optional resource filter and output cap,
    /// returning `(found_history_coverage, total_matching, capped_units)`.
    pub fn load_units_as_of_filtered(
        &self,
        source_id: &str,
        cutoff_ms: u64,
        resource_key_filter: Option<&str>,
        limit: usize,
    ) -> Result<(bool, usize, Vec<KnowledgeUnit>)> {
        validate_ident(source_id, "source_id")?;
        let Some(meta) = self.get_meta(source_id)? else {
            return Ok((false, 0, Vec::new()));
        };
        if !self.is_meta_live(&meta)? {
            return Ok((false, 0, Vec::new()));
        }
        let candidate_keys = if meta.history_resource_keys.is_empty() {
            &meta.resource_keys
        } else {
            &meta.history_resource_keys
        };
        let mut out = Vec::new();
        let mut total = 0usize;
        let mut any_history_record = false;
        for rk in candidate_keys {
            if let Some(target_rk) = resource_key_filter
                && rk != target_rk
            {
                continue;
            }
            let hk = self.history_key(source_id, rk);
            if let Some((_, raw)) = self.store.get(ns::CONTEXT, &hk)? {
                any_history_record = true;
                let entries: Vec<ResourceHistoryEntry> =
                    decode_zstd_json(&raw, &format!("resource unit history ({source_id}/{rk})"))?;
                if let Some(chosen) = entries.iter().rfind(|e| e.observed_at_ms <= cutoff_ms) {
                    total += chosen.units.len();
                    if out.len() < limit {
                        let take = limit - out.len();
                        out.extend(chosen.units.iter().take(take).cloned());
                    }
                }
            } else if let Some((_, raw)) =
                self.store.get(ns::CONTEXT, &self.res_key(source_id, rk))?
            {
                let live: Vec<KnowledgeUnit> = decode_zstd_json(&raw, "resource units")?;
                for u in live.into_iter().filter(|u| u.observed_at_ms <= cutoff_ms) {
                    total += 1;
                    if out.len() < limit {
                        out.push(u);
                    }
                }
            }
        }
        Ok((any_history_record, total, out))
    }
}
