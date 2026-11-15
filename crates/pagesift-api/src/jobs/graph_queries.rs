//! Persistent `GraphStore` query helpers (bounded & point-in-time checked).

use pagesift_graph::{ClaimLifecycleRecord, ConflictRecord};
use pagesift_store::Store;

use crate::error::{ApiError, ErrorCode};

/// Query persistent `GraphStore` version history with optional `as_of_ms`.
pub async fn store_graph_history(
    store: &Store,
    tenant: Option<&str>,
    url_or_key: &str,
    as_of_ms: Option<u64>,
    limit: usize,
) -> Result<pagesift_store::GraphHistoryResult, ApiError> {
    let target = url_or_key.to_string();
    let scope = tenant.map(str::to_string);
    store
        .blocking(move |s| {
            let g = s.graph_for(scope.as_deref());
            match as_of_ms {
                Some(cutoff) => g.history_as_of(&target, Some(cutoff), limit),
                None => g.history_bounded(&target, limit),
            }
        })
        .await
        .map_err(|e| ApiError::new(ErrorCode::Internal, format!("graph history failed: {e}")))
}

/// Query persistent `GraphStore` conflicts with optional `subject`, `as_of_ms`, and pagination.
pub async fn store_graph_conflicts(
    store: &Store,
    tenant: Option<&str>,
    subject: Option<&str>,
    as_of_ms: Option<u64>,
    limit: usize,
    offset: usize,
) -> Result<(usize, Vec<ConflictRecord>, bool), ApiError> {
    let (total, items, has_more, _) =
        store_graph_conflicts_checked(store, tenant, subject, as_of_ms, limit, offset).await?;
    Ok((total, items, has_more))
}

/// Query persistent `GraphStore` conflicts returning `(total, page, truncated, history_unavailable)`.
pub async fn store_graph_conflicts_checked(
    store: &Store,
    tenant: Option<&str>,
    subject: Option<&str>,
    as_of_ms: Option<u64>,
    limit: usize,
    offset: usize,
) -> Result<(usize, Vec<ConflictRecord>, bool, bool), ApiError> {
    let subj = subject.map(str::to_string);
    let scope = tenant.map(str::to_string);
    let (items, total, has_more, history_unavailable) = store
        .blocking(move |s| {
            s.graph_for(scope.as_deref()).list_conflicts_as_of_checked(
                subj.as_deref(),
                as_of_ms,
                limit,
                offset,
            )
        })
        .await
        .map_err(|e| ApiError::new(ErrorCode::Internal, format!("graph conflicts failed: {e}")))?;
    Ok((total, items, has_more, history_unavailable))
}

/// Query persistent `GraphStore` claim lifecycle records with optional `url_or_key`, `subject`, `as_of_ms`, and pagination.
pub async fn store_graph_claims(
    store: &Store,
    tenant: Option<&str>,
    url_or_key: Option<&str>,
    subject: Option<&str>,
    as_of_ms: Option<u64>,
    limit: usize,
    offset: usize,
) -> Result<(usize, Vec<ClaimLifecycleRecord>, bool), ApiError> {
    let (total, items, has_more, _) =
        store_graph_claims_checked(store, tenant, url_or_key, subject, as_of_ms, limit, offset)
            .await?;
    Ok((total, items, has_more))
}

/// Query persistent `GraphStore` claims returning `(total, page, truncated, history_unavailable)`.
pub async fn store_graph_claims_checked(
    store: &Store,
    tenant: Option<&str>,
    url_or_key: Option<&str>,
    subject: Option<&str>,
    as_of_ms: Option<u64>,
    limit: usize,
    offset: usize,
) -> Result<(usize, Vec<ClaimLifecycleRecord>, bool, bool), ApiError> {
    let target = url_or_key.map(str::to_string);
    let subj = subject.map(str::to_string);
    let scope = tenant.map(str::to_string);
    let (items, total, has_more, history_unavailable) = store
        .blocking(move |s| {
            let g = s.graph_for(scope.as_deref());
            if let Some(ref u) = target {
                let mut res = g.claims_for_resource_as_of_checked(u, as_of_ms)?;
                if let Some(ref sk) = subj {
                    res.claims.retain(|c| &c.claim.subject_key == sk);
                }
                let total = res.claims.len();
                let end = total.min(offset.saturating_add(limit));
                let has_more = end < total;
                let page = if offset >= total {
                    Vec::new()
                } else {
                    res.claims
                        .into_iter()
                        .skip(offset)
                        .take(end - offset)
                        .collect()
                };
                Ok::<_, pagesift_store::Error>((page, total, has_more, res.history_unavailable))
            } else {
                g.list_claims_as_of_checked(subj.as_deref(), as_of_ms, limit, offset)
            }
        })
        .await
        .map_err(|e| ApiError::new(ErrorCode::Internal, format!("graph claims failed: {e}")))?;
    Ok((total, items, has_more, history_unavailable))
}
