//! At-least-once delivery, idempotent acknowledgement, and full-resync detection for external
//! vector/search index consumers (`ns::CONTEXT`).

use pagesift_graph::context::{ConsumerCheckpoint, ContextDeltaPollResult, ReplayStatus};

use super::{ContextSourceMeta, ContextStore, decode_json, encode_json, validate_ident};
use crate::Result;
use crate::engine::ns;
use crate::sync::now_store_ms;

// [策略] 外部索引消费者单次拉取默认批次上限 32 条 ContextDelta
pub const DEFAULT_DELTA_POLL_LIMIT: usize = 32;

impl<'a> ContextStore<'a> {
    /// Read a persisted external consumer checkpoint for `(consumer_id, source_id)`.
    pub fn get_consumer_checkpoint(
        &self,
        consumer_id: &str,
        source_id: &str,
    ) -> Result<Option<ConsumerCheckpoint>> {
        validate_ident(consumer_id, "consumer_id")?;
        validate_ident(source_id, "source_id")?;
        let k = self.checkpoint_key(consumer_id, source_id);
        let Some((_, raw)) = self.store.get(ns::CONTEXT, &k)? else {
            return Ok(None);
        };
        decode_json(&raw, "consumer checkpoint").map(Some)
    }

    /// Poll incremental [`pagesift_graph::context::ContextDelta`] events from a consumer's checkpoint.
    ///
    /// Never claims cross-system atomicity: detects ABA source recreation, deleted Sync sources,
    /// schema/model spec drift, and retention-window gaps, returning [`ReplayStatus::FullResyncRequired`] when needed.
    pub fn poll_consumer_deltas(
        &self,
        consumer_id: &str,
        source_id: &str,
        expected_spec_fingerprint: Option<&str>,
        limit: usize,
    ) -> Result<ContextDeltaPollResult> {
        let cp = self.get_consumer_checkpoint(consumer_id, source_id)?;
        let Some(meta) = self.get_meta(source_id)? else {
            // If consumer previously held state for a source that was since purged, alert FullResync!
            let status = if cp
                .as_ref()
                .is_some_and(|c| c.acked_delta_seq > 0 || !c.instance_id.is_empty())
            {
                ReplayStatus::FullResyncRequired {
                    reason: "source_purged_or_missing".to_owned(),
                }
            } else {
                ReplayStatus::UpToDate
            };
            return Ok(ContextDeltaPollResult {
                status,
                source_id: source_id.to_owned(),
                current_instance_id: String::new(),
                latest_delta_seq: 0,
                checkpoint: cp,
                deltas: Vec::new(),
            });
        };

        if !self.is_meta_live(&meta)? {
            return Ok(ContextDeltaPollResult {
                status: ReplayStatus::FullResyncRequired {
                    reason: "source_instance_deleted_or_superseded".to_owned(),
                },
                source_id: source_id.to_owned(),
                current_instance_id: meta.instance_id,
                latest_delta_seq: meta.latest_delta_seq,
                checkpoint: cp,
                deltas: Vec::new(),
            });
        }

        let meta_fp = meta.spec_stamp.fingerprint();
        if let Some(req_fp) = expected_spec_fingerprint
            && !req_fp.is_empty()
            && req_fp != meta_fp
        {
            return Ok(ContextDeltaPollResult {
                status: ReplayStatus::FullResyncRequired {
                    reason: format!("spec_fingerprint_mismatch:store={meta_fp},consumer={req_fp}"),
                },
                source_id: source_id.to_owned(),
                current_instance_id: meta.instance_id,
                latest_delta_seq: meta.latest_delta_seq,
                checkpoint: cp,
                deltas: Vec::new(),
            });
        }

        if let Some(existing_cp) = &cp {
            if existing_cp.instance_id != meta.instance_id {
                return Ok(ContextDeltaPollResult {
                    status: ReplayStatus::FullResyncRequired {
                        reason: "source_instance_recreated_aba_guard".to_owned(),
                    },
                    source_id: source_id.to_owned(),
                    current_instance_id: meta.instance_id,
                    latest_delta_seq: meta.latest_delta_seq,
                    checkpoint: cp,
                    deltas: Vec::new(),
                });
            }
            if !existing_cp.spec_fingerprint.is_empty() && existing_cp.spec_fingerprint != meta_fp {
                return Ok(ContextDeltaPollResult {
                    status: ReplayStatus::FullResyncRequired {
                        reason: "checkpoint_spec_fingerprint_outdated".to_owned(),
                    },
                    source_id: source_id.to_owned(),
                    current_instance_id: meta.instance_id,
                    latest_delta_seq: meta.latest_delta_seq,
                    checkpoint: cp,
                    deltas: Vec::new(),
                });
            }
        }

        let acked = cp.as_ref().map(|c| c.acked_delta_seq).unwrap_or(0);
        if acked >= meta.latest_delta_seq {
            return Ok(ContextDeltaPollResult {
                status: ReplayStatus::UpToDate,
                source_id: source_id.to_owned(),
                current_instance_id: meta.instance_id,
                latest_delta_seq: meta.latest_delta_seq,
                checkpoint: cp,
                deltas: Vec::new(),
            });
        }

        // If consumer fell behind the retained ring window, require baseline snapshot resync!
        if acked.saturating_add(1) < meta.earliest_retained_delta_seq {
            return Ok(ContextDeltaPollResult {
                status: ReplayStatus::FullResyncRequired {
                    reason: format!(
                        "checkpoint_behind_retained_window:acked={acked},earliest={}",
                        meta.earliest_retained_delta_seq
                    ),
                },
                source_id: source_id.to_owned(),
                current_instance_id: meta.instance_id,
                latest_delta_seq: meta.latest_delta_seq,
                checkpoint: cp,
                deltas: Vec::new(),
            });
        }

        let deltas = self.list_deltas(source_id, Some(acked), limit.max(1))?;
        Ok(ContextDeltaPollResult {
            status: if deltas.is_empty() {
                ReplayStatus::UpToDate
            } else {
                ReplayStatus::IncrementalReady
            },
            source_id: source_id.to_owned(),
            current_instance_id: meta.instance_id,
            latest_delta_seq: meta.latest_delta_seq,
            checkpoint: cp,
            deltas,
        })
    }

    /// Idempotently advance a consumer checkpoint (monotonic against duplicate, out-of-order,
    /// cross-instance stale, or future-overflow acks).
    pub fn ack_consumer_checkpoint(
        &self,
        mut incoming: ConsumerCheckpoint,
    ) -> Result<ConsumerCheckpoint> {
        validate_ident(&incoming.consumer_id, "consumer_id")?;
        validate_ident(&incoming.source_id, "source_id")?;
        validate_ident(&incoming.instance_id, "instance_id")?;
        let now = now_store_ms();
        if incoming.updated_at_ms == 0 {
            incoming.updated_at_ms = now;
        }
        let mut tx = self.store.atomic_tx();
        let meta_opt: Option<ContextSourceMeta> =
            match tx.get(ns::CONTEXT, &self.meta_key(&incoming.source_id))? {
                Some((_, raw)) => Some(decode_json(&raw, "context source meta")?),
                None => None,
            };

        let k = self.checkpoint_key(&incoming.consumer_id, &incoming.source_id);
        let current_opt: Option<ConsumerCheckpoint> = match tx.get(ns::CONTEXT, &k)? {
            Some((_, raw)) => Some(decode_json(&raw, "consumer checkpoint")?),
            None => None,
        };

        if let Some(meta) = &meta_opt {
            // ABA guard on out-of-order late ACK from an old instance after consumer already synced to new instance!
            if let Some(current) = &current_opt
                && current.instance_id == meta.instance_id
                && incoming.instance_id != meta.instance_id
            {
                return Ok(current.clone());
            }
            if incoming.instance_id == meta.instance_id {
                if incoming.acked_delta_seq > meta.latest_delta_seq {
                    incoming.acked_delta_seq = meta.latest_delta_seq;
                }
                if incoming.spec_fingerprint.is_empty()
                    && current_opt.as_ref().is_none_or(|c| {
                        c.instance_id != incoming.instance_id || c.spec_fingerprint.is_empty()
                    })
                {
                    incoming.spec_fingerprint = meta.spec_stamp.fingerprint();
                }
            }
        }

        if let Some(current) = current_opt
            && current.instance_id == incoming.instance_id
        {
            if incoming.spec_fingerprint.is_empty() && !current.spec_fingerprint.is_empty() {
                incoming.spec_fingerprint = current.spec_fingerprint.clone();
            }
            // Duplicate or out-of-order lower sequence number on the same instance & spec is a safe no-op!
            if current.spec_fingerprint == incoming.spec_fingerprint
                && current.acked_delta_seq >= incoming.acked_delta_seq
            {
                return Ok(current);
            }
        }

        tx.put(
            ns::CONTEXT,
            &k,
            &encode_json(&incoming, "consumer checkpoint")?,
            now,
        )?;
        tx.commit()?;
        Ok(incoming)
    }
}
