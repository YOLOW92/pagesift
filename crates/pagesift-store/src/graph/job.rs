//! Persistent job-level `GraphSummary` and `PageGraphDelta` storage (`ns::GRAPH_JOB`).

use std::time::SystemTime;

use pagesift_graph::{GraphSummary, PageGraphDelta};

use super::{
    DEFAULT_GRAPH_DELTAS_LIMIT, GraphStore, MAX_GRAPH_BLOB, PREFIX_DELTA, PREFIX_SUMMARY,
    decode_json, encode_json,
};
use crate::engine::ns;
use crate::snapshot::compress_snapshot;
use crate::{Error, Result, to_ms};

impl<'a> GraphStore<'a> {
    /// Persist a zstd-compressed job [`GraphSummary`] under `ns::GRAPH_JOB` with symmetric capacity checks.
    pub fn put_job_summary(&self, job_id: &str, summary: &GraphSummary) -> Result<()> {
        let json = encode_json(summary, "graph summary")?;
        if json.len() > MAX_GRAPH_BLOB {
            return Err(Error::ValueTooLarge {
                size: json.len(),
                limit: MAX_GRAPH_BLOB,
            });
        }
        let compressed = compress_snapshot(&json, self.store.config().snapshot_compress_level);
        if compressed.len() > MAX_GRAPH_BLOB {
            return Err(Error::ValueTooLarge {
                size: compressed.len(),
                limit: MAX_GRAPH_BLOB,
            });
        }
        self.store
            .put(
                ns::GRAPH_JOB,
                &self.scoped_prefixed(PREFIX_SUMMARY, job_id),
                &compressed,
                to_ms(SystemTime::now()),
            )
            .map(drop)
    }

    /// Load and decompress a stored job [`GraphSummary`].
    pub fn get_job_summary(&self, job_id: &str) -> Result<Option<GraphSummary>> {
        let Some((_, raw)) = self
            .store
            .get(ns::GRAPH_JOB, &self.scoped_prefixed(PREFIX_SUMMARY, job_id))?
        else {
            return Ok(None);
        };
        let decompressed = pagesift_compress::decompress(&raw, MAX_GRAPH_BLOB)
            .map_err(|e| Error::Corrupt(format!("graph summary {job_id}: {e}")))?;
        decode_json(&decompressed, "graph summary").map(Some)
    }

    /// Persist one zstd-compressed [`PageGraphDelta`] belonging to `job_id` with symmetric capacity checks.
    pub fn append_job_delta(&self, job_id: &str, delta: &PageGraphDelta) -> Result<()> {
        let json = encode_json(delta, "page graph delta")?;
        if json.len() > MAX_GRAPH_BLOB {
            return Err(Error::ValueTooLarge {
                size: json.len(),
                limit: MAX_GRAPH_BLOB,
            });
        }
        let compressed = compress_snapshot(&json, self.store.config().snapshot_compress_level);
        if compressed.len() > MAX_GRAPH_BLOB {
            return Err(Error::ValueTooLarge {
                size: compressed.len(),
                limit: MAX_GRAPH_BLOB,
            });
        }
        let key = self.scoped_compound(PREFIX_DELTA, job_id, &delta.node.doc_id);
        let ts = if delta.node.observed_at_ms > 0 {
            delta.node.observed_at_ms
        } else {
            to_ms(SystemTime::now())
        };
        self.store
            .put(ns::GRAPH_JOB, &key, &compressed, ts)
            .map(drop)
    }

    /// Read stored [`PageGraphDelta`] entries for `job_id`, bounded by [`DEFAULT_GRAPH_DELTAS_LIMIT`].
    pub fn job_deltas(&self, job_id: &str) -> Result<Vec<PageGraphDelta>> {
        let (deltas, _) = self.job_deltas_bounded(job_id, DEFAULT_GRAPH_DELTAS_LIMIT)?;
        Ok(deltas)
    }

    /// Read up to `limit` [`PageGraphDelta`] entries for `job_id`, sorted deterministically.
    pub fn job_deltas_bounded(
        &self,
        job_id: &str,
        limit: usize,
    ) -> Result<(Vec<PageGraphDelta>, bool)> {
        let mut out = Vec::new();
        let has_more = self.visit_job_deltas(job_id, limit, |delta| {
            out.push(delta);
            Ok(())
        })?;
        out.sort_by(|a, b| {
            a.node
                .observed_at_ms
                .cmp(&b.node.observed_at_ms)
                .then_with(|| a.node.doc_id.cmp(&b.node.doc_id))
        });
        Ok((out, has_more))
    }

    /// Stream at most `limit` decoded [`PageGraphDelta`] items for `job_id` without buffering all in RAM.
    pub fn visit_job_deltas(
        &self,
        job_id: &str,
        limit: usize,
        mut visitor: impl FnMut(PageGraphDelta) -> Result<()>,
    ) -> Result<bool> {
        let prefix = self.scoped_compound(PREFIX_DELTA, job_id, "");
        let keys = self.store.keys(ns::GRAPH_JOB, |k| k.starts_with(&prefix))?;
        let has_more = keys.len() > limit;
        for key in keys.into_iter().take(limit) {
            if let Some((_, raw)) = self.store.get(ns::GRAPH_JOB, &key)? {
                let decompressed = pagesift_compress::decompress(&raw, MAX_GRAPH_BLOB)
                    .map_err(|e| Error::Corrupt(format!("graph delta {job_id}: {e}")))?;
                visitor(decode_json(&decompressed, "page graph delta")?)?;
            }
        }
        Ok(has_more)
    }

    /// Delete stored summary and page deltas belonging to `job_id` within this tenant scope.
    pub fn purge_job(&self, job_id: &str) -> Result<usize> {
        let summary_key = self.scoped_prefixed(PREFIX_SUMMARY, job_id);
        let delta_prefix = self.scoped_compound(PREFIX_DELTA, job_id, "");
        let delta_keys = self
            .store
            .keys(ns::GRAPH_JOB, |k| k.starts_with(&delta_prefix))?;
        let mut removed = 0usize;
        if self.store.delete(ns::GRAPH_JOB, &summary_key)? {
            removed += 1;
        }
        for key in delta_keys {
            if self.store.delete(ns::GRAPH_JOB, &key)? {
                removed += 1;
            }
        }
        Ok(removed)
    }
}
