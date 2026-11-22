//! Startup API-key -> logical account resolution with seamless `1.6.0` persisted-store
//! tenant continuity and strict duplicate-`name` isolation.
//!
//! Resolution order per configured API key:
//! 1. Explicit `KeyLimits.account` (validated via [`validate_account_id`]) always wins.
//! 2. Otherwise, check whether an existing persistent [`Store`] or cluster [`Ledger`] already
//!    holds legacy `1.6.0` data for this key's unique prior identity (either unique `name` or
//!    64-bit `legacy_fnv1a_fingerprint`) while holding no data under the new SHA-256 fingerprint:
//!    - Unnamed key (`name: None`) with unique `legacy_fnv1a_fingerprint`: retained automatically
//!      so existing Context, DataGraph, Loop Sources, Jobs, Monitors, and Daily Quota stay intact.
//!    - Uniquely-named key (`name: Some(n)`, count == 1, not claimed by an explicit `account`):
//!      retained automatically with a warning advising the operator to pin `account = "<n>"`.
//!    - Duplicate `name` across 2+ keys: NEVER merged (preserves `P1-04` / `ITNA-02` isolation);
//!      logs an actionable warning if legacy store data exists under that shared `name`.
//! 3. All fresh/new keys resolve to their domain-separated SHA-256 fingerprint (`key-<32hex>`).

use std::collections::{HashMap, HashSet};
use std::sync::Arc;

use pagesift_store::Store;
use serde::Deserialize;

use crate::error::{ApiError, ErrorCode};
use crate::limits::{
    Caller, KeyLimits, fingerprint, legacy_fnv1a_fingerprint, usage_key, validate_account_id,
};

#[derive(Deserialize)]
struct OwnerProbe {
    #[serde(default)]
    owner: Option<String>,
}

/// Resolve all configured API keys once at router startup, returning:
/// - `callers`: `raw_key -> Caller` (O(1) lookup in auth middleware with zero per-request hashing)
/// - `accounts`: `resolved_account -> Arc<KeyLimits>` (shared by Limiter, Jobs restore, Monitors)
///
/// Enforces deterministic, order-independent validation across every configuration entrypoint
/// (TOML, direct Rust `Config`, cluster gateway, and restarts):
/// - Invalid explicit `KeyLimits.account` values are rejected.
/// - Storage probe errors during legacy tenant detection fail closed instead of silently
///   starting a fresh empty SHA-256 namespace.
/// - Ambiguous dual namespaces (both legacy `1.6.0` data and new SHA-256 data present for the
///   same key, or colliding legacy FNV-1a fingerprints with persisted data) fail closed.
/// - Conflicting `(requests_per_minute, max_concurrent_jobs, pages_per_day)` on keys resolving
///   to the same logical account are rejected; valid shared accounts share a single canonical
///   `Arc<KeyLimits>` across `Caller`, `Limiter`, `Jobs`, and `Monitors`.
pub(crate) type ResolvedCallers = (HashMap<String, Caller>, HashMap<String, Arc<KeyLimits>>);

pub(crate) fn resolve_callers(
    raw_keys: HashMap<String, Arc<KeyLimits>>,
    store: Option<&Store>,
    ledger: Option<&pagesift_cluster::Ledger>,
) -> Result<ResolvedCallers, ApiError> {
    if raw_keys.is_empty() {
        return Ok((HashMap::new(), HashMap::new()));
    }

    // Sort keys deterministically so resolution, diagnostics, and validation never depend on
    // HashMap iteration order.
    let mut sorted_keys: Vec<(String, Arc<KeyLimits>)> = raw_keys.into_iter().collect();
    sorted_keys.sort_by(|a, b| a.0.cmp(&b.0));

    let mut explicit_by_key: Vec<Option<String>> = Vec::with_capacity(sorted_keys.len());
    let mut explicit_accounts: HashSet<String> = HashSet::new();
    let mut unnamed_name_counts: HashMap<String, usize> = HashMap::new();
    let mut legacy_fp_counts: HashMap<String, usize> = HashMap::new();

    for (k, l) in &sorted_keys {
        if let Some(raw_acct) = l.account.as_deref() {
            let acct = validate_account_id(raw_acct).map_err(|e| {
                ApiError::invalid(format!("invalid API key account {raw_acct:?}: {e}"))
            })?;
            explicit_accounts.insert(acct.clone());
            explicit_by_key.push(Some(acct));
        } else {
            explicit_by_key.push(None);
            if let Some(n) = l.name.as_deref().map(str::trim).filter(|s| !s.is_empty()) {
                *unnamed_name_counts.entry(n.to_owned()).or_default() += 1;
            } else {
                *legacy_fp_counts
                    .entry(legacy_fnv1a_fingerprint(k))
                    .or_default() += 1;
            }
        }
    }

    // Collect every account ID worth probing against an existing store/ledger.
    let mut probe_accounts: HashSet<String> = HashSet::new();
    for ((k, l), explicit) in sorted_keys.iter().zip(&explicit_by_key) {
        if explicit.is_some() {
            continue;
        }
        probe_accounts.insert(fingerprint(k));
        if let Some(n) = l.name.as_deref().map(str::trim).filter(|s| !s.is_empty()) {
            probe_accounts.insert(n.to_owned());
        } else {
            probe_accounts.insert(legacy_fnv1a_fingerprint(k));
        }
    }

    let existing = if probe_accounts.is_empty() || (store.is_none() && ledger.is_none()) {
        HashSet::new()
    } else {
        find_existing_accounts(&probe_accounts, store, ledger)?
    };

    // Emit an actionable warning if 2+ keys share a display `name` that has legacy persisted data.
    let mut dup_names: Vec<(&String, &usize)> = unnamed_name_counts.iter().collect();
    dup_names.sort_by(|a, b| a.0.cmp(b.0));
    for (dup_name, count) in dup_names {
        if *count > 1 && existing.contains(dup_name) && !explicit_accounts.contains(dup_name) {
            tracing::warn!(
                name = %dup_name,
                key_count = *count,
                "multiple API keys share display name '{dup_name}' without explicit `account`; \
                 keys remain isolated by SHA-256 fingerprint, while legacy 1.6.0 persisted data \
                 exists under '{dup_name}'. Set `account = \"{dup_name}\"` explicitly on the \
                 owning key(s) in [[server.keys]] to attach that namespace."
            );
        }
    }

    let mut callers = HashMap::with_capacity(sorted_keys.len());
    let mut accounts: HashMap<String, Arc<KeyLimits>> = HashMap::with_capacity(sorted_keys.len());

    for ((k, l), explicit) in sorted_keys.into_iter().zip(explicit_by_key) {
        let resolved_acct = if let Some(acct) = explicit {
            acct
        } else {
            let sha_acct = fingerprint(&k);
            let legacy_candidate = match l.name.as_deref().map(str::trim).filter(|s| !s.is_empty())
            {
                Some(n) => {
                    let unique = unnamed_name_counts.get(n).copied().unwrap_or(0) == 1;
                    if unique && !explicit_accounts.contains(n) && validate_account_id(n).is_ok() {
                        Some((n.to_owned(), true))
                    } else {
                        None
                    }
                }
                None => {
                    let fp = legacy_fnv1a_fingerprint(&k);
                    let count = legacy_fp_counts.get(&fp).copied().unwrap_or(0);
                    if count > 1 && existing.contains(&fp) {
                        return Err(ApiError::invalid(format!(
                            "ambiguous legacy tenant namespace '{fp}': multiple unnamed API keys collide on the same 1.6.0 FNV-1a fingerprint with persisted data; assign explicit `account` IDs or migrate before starting"
                        )));
                    }
                    (count == 1).then_some((fp, false))
                }
            };

            match legacy_candidate {
                Some((legacy, from_name))
                    if existing.contains(&legacy) && existing.contains(&sha_acct) =>
                {
                    let guidance = if from_name {
                        format!(
                            "set `account = \"{legacy}\"` explicitly to keep the legacy namespace, or export/remove the unused namespace before starting"
                        )
                    } else {
                        "export or remove one of the two namespaces before starting so data is neither silently dropped nor merged".to_owned()
                    };
                    return Err(ApiError::invalid(format!(
                        "ambiguous tenant namespace: persisted data exists under both legacy 1.6.0 namespace '{legacy}' and SHA-256 namespace '{sha_acct}'; {guidance}"
                    )));
                }
                Some((legacy, from_name))
                    if existing.contains(&legacy) && !existing.contains(&sha_acct) =>
                {
                    if from_name {
                        tracing::warn!(
                            account = %legacy,
                            "retained legacy 1.6.0 unique key-name namespace '{legacy}' because \
                             persisted store data exists under it; set `account = \"{legacy}\"` \
                             explicitly in [[server.keys]] to pin this mapping permanently"
                        );
                    } else {
                        tracing::info!(
                            account = %legacy,
                            "retained legacy 1.6.0 key fingerprint namespace '{legacy}' because \
                             persisted store data exists under it"
                        );
                    }
                    legacy
                }
                _ => sha_acct,
            }
        };

        let canonical_limits = if let Some(prev) = accounts.get(&resolved_acct) {
            if prev.requests_per_minute != l.requests_per_minute
                || prev.max_concurrent_jobs != l.max_concurrent_jobs
                || prev.pages_per_day != l.pages_per_day
            {
                return Err(ApiError::invalid(format!(
                    "API keys sharing account {resolved_acct:?} must declare identical requests_per_minute, max_concurrent_jobs, and pages_per_day"
                )));
            }
            prev.clone()
        } else {
            let shared = Arc::new(KeyLimits {
                name: None,
                account: Some(resolved_acct.clone()),
                requests_per_minute: l.requests_per_minute,
                max_concurrent_jobs: l.max_concurrent_jobs,
                pages_per_day: l.pages_per_day,
            });
            accounts.insert(resolved_acct.clone(), shared.clone());
            shared
        };

        let display_name = l
            .name
            .as_deref()
            .map(str::trim)
            .filter(|s| !s.is_empty())
            .map(str::to_owned)
            .unwrap_or_else(|| resolved_acct.clone());
        let caller = Caller::with_resolved_account_and_display(
            resolved_acct,
            display_name,
            canonical_limits,
            true,
        );
        callers.insert(k, caller);
    }

    Ok((callers, accounts))
}

/// Probe existing persisted data across Context, Loop Sync, DataGraph, Daily Quota,
/// Attachment Access, Monitors, Jobs, Agents, and cluster Ledger for candidate account IDs.
///
/// Storage read errors and unreadable owner records fail closed instead of pretending
/// legacy persisted data does not exist.
fn find_existing_accounts(
    candidates: &HashSet<String>,
    store: Option<&Store>,
    ledger: Option<&pagesift_cluster::Ledger>,
) -> Result<HashSet<String>, ApiError> {
    let mut found = HashSet::new();

    if let Some(l) = ledger {
        for acct in candidates {
            if l.has_account_usage(acct) {
                found.insert(acct.clone());
            }
        }
    }

    let Some(st) = store else {
        return Ok(found);
    };

    // 1. Context / Loop Sync / DataGraph (single index pass without reading values)
    let tenant_ids: Vec<String> = candidates
        .iter()
        .filter(|a| !found.contains(*a))
        .map(|a| format!("acct:{a}"))
        .collect();
    if !tenant_ids.is_empty() {
        let existing_tenants = st.existing_tenants(&tenant_ids).map_err(|e| {
            ApiError::new(
                ErrorCode::Internal,
                format!("legacy tenant probe failed while scanning store index: {e}"),
            )
        })?;
        for t in existing_tenants {
            if let Some(acct) = t.strip_prefix("acct:") {
                found.insert(acct.to_owned());
            }
        }
    }

    // 2. Persisted daily quota usage (`quota/{account}`)
    for acct in candidates {
        if found.contains(acct) {
            continue;
        }
        let has_quota = st.tasks().kv_get(usage_key(acct)).map_err(|e| {
            ApiError::new(
                ErrorCode::Internal,
                format!("legacy tenant probe failed while reading quota/{acct}: {e}"),
            )
        })?;
        if has_quota.is_some() {
            found.insert(acct.clone());
        }
    }

    let mut unreadable_records = Vec::new();

    // 3. Persisted attachment ownership (`attachment-access/{account}/{id}`)
    if found.len() < candidates.len() {
        let access_keys = st.tasks().kv_keys("attachment-access/").map_err(|e| {
            ApiError::new(
                ErrorCode::Internal,
                format!("legacy tenant probe failed while listing attachment-access/: {e}"),
            )
        })?;
        for key in access_keys {
            if found.len() == candidates.len() {
                break;
            }
            match std::str::from_utf8(&key)
                .ok()
                .and_then(|s| s.strip_prefix("attachment-access/"))
                .and_then(|rest| rest.rsplit_once('/'))
            {
                Some((acct, id)) if !acct.is_empty() && !id.is_empty() => {
                    if candidates.contains(acct) {
                        found.insert(acct.to_owned());
                    }
                }
                _ => {
                    unreadable_records.push(format!(
                        "{}: malformed attachment-access key",
                        String::from_utf8_lossy(&key)
                    ));
                }
            }
        }
    }

    // 4. Persisted Monitors (`monitor/{id}`), Jobs (`job/{id}`), and Agents (`agent/{id}`)
    if found.len() < candidates.len() {
        for prefix in ["monitor/", "job/", "agent/"] {
            let keys = st.tasks().kv_keys(prefix).map_err(|e| {
                ApiError::new(
                    ErrorCode::Internal,
                    format!("legacy tenant probe failed while listing {prefix}: {e}"),
                )
            })?;
            for key in keys {
                if found.len() == candidates.len() {
                    break;
                }
                // Skip derived sub-keys (e.g. `job/{id}/hook_seq`, `job/{id}/hook_ack`) which
                // store plain numeric counters rather than JSON metadata with an `owner` field.
                let is_top_level_record = std::str::from_utf8(&key)
                    .ok()
                    .and_then(|s| s.strip_prefix(prefix))
                    .is_some_and(|rest| !rest.is_empty() && !rest.contains('/'));
                if !is_top_level_record {
                    continue;
                }
                let Some(bytes) = st.tasks().kv_get(&key).map_err(|e| {
                    ApiError::new(
                        ErrorCode::Internal,
                        format!(
                            "legacy tenant probe failed while reading {}: {e}",
                            String::from_utf8_lossy(&key)
                        ),
                    )
                })?
                else {
                    continue;
                };
                match serde_json::from_slice::<OwnerProbe>(&bytes) {
                    Ok(probe) => {
                        if let Some(owner) = probe.owner
                            && candidates.contains(&owner)
                        {
                            found.insert(owner);
                        }
                    }
                    Err(e) => {
                        let key_name = String::from_utf8_lossy(&key).into_owned();
                        tracing::error!(
                            record = %key_name,
                            error = %e,
                            "unreadable persisted task record during legacy tenant probe"
                        );
                        unreadable_records.push(key_name);
                    }
                }
            }
        }
    }

    // If any persisted monitor/job/agent record could not be parsed and candidate legacy accounts
    // remain unresolved, fail closed rather than silently pretending the key has no legacy data.
    if !unreadable_records.is_empty() && found.len() < candidates.len() {
        return Err(ApiError::new(
            ErrorCode::Internal,
            format!(
                "legacy tenant probe could not determine ownership of corrupt record(s) [{}]; repair the store or pin explicit `account` in [[server.keys]] before starting",
                unreadable_records.join(", ")
            ),
        ));
    }

    Ok(found)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn resolves_legacy_fnv1a_and_unique_name_from_persisted_store_while_isolating_duplicates() {
        let dir = tempfile::tempdir().unwrap();
        let store = Store::open_dir(dir.path()).unwrap();

        let unnamed_key = "sk-legacy-unnamed";
        let legacy_fp = legacy_fnv1a_fingerprint(unnamed_key);
        // 1. Seed Context data under legacy 1.6.0 FNV-1a fingerprint.
        store
            .context_for(Some(&format!("acct:{legacy_fp}")))
            .put_learn_bytes("default", b"{}")
            .unwrap();
        // 2. Seed daily quota under unique legacy name "legacy-team".
        store
            .tasks()
            .kv_put(usage_key("legacy-team"), b"20000 3")
            .unwrap();
        // 3. Seed persisted monitor owner under unique legacy name "mon-team".
        store
            .tasks()
            .kv_put("monitor/m1", br#"{"id":"m1","owner":"mon-team"}"#)
            .unwrap();
        // 4. Seed persisted job owner under duplicate name "dup-shared".
        store
            .tasks()
            .kv_put("job/j1", br#"{"id":"j1","owner":"dup-shared"}"#)
            .unwrap();

        let mut raw = HashMap::new();
        raw.insert(unnamed_key.to_owned(), Arc::new(KeyLimits::new()));
        raw.insert(
            "sk-unique".to_owned(),
            Arc::new(KeyLimits::new().with_name("legacy-team")),
        );
        raw.insert(
            "sk-mon".to_owned(),
            Arc::new(KeyLimits::new().with_name("mon-team")),
        );
        raw.insert(
            "sk-dup-1".to_owned(),
            Arc::new(KeyLimits::new().with_name("dup-shared")),
        );
        raw.insert(
            "sk-dup-2".to_owned(),
            Arc::new(KeyLimits::new().with_name("dup-shared")),
        );
        raw.insert("sk-fresh".to_owned(), Arc::new(KeyLimits::new()));
        raw.insert(
            "sk-explicit".to_owned(),
            Arc::new(KeyLimits::new().with_account("dup-shared")),
        );

        let (callers, _accounts) = resolve_callers(raw, Some(&store), None).unwrap();

        assert_eq!(&*callers[unnamed_key].account, legacy_fp.as_str());
        assert_eq!(&*callers["sk-unique"].account, "legacy-team");
        assert_eq!(&*callers["sk-mon"].account, "mon-team");
        // Duplicate display names NEVER merge even when legacy data exists under that name.
        assert_eq!(&*callers["sk-dup-1"].account, fingerprint("sk-dup-1"));
        assert_eq!(&*callers["sk-dup-2"].account, fingerprint("sk-dup-2"));
        assert_ne!(callers["sk-dup-1"].account, callers["sk-dup-2"].account);
        // Fresh key with no legacy data uses SHA-256 fingerprint.
        assert_eq!(&*callers["sk-fresh"].account, fingerprint("sk-fresh"));
        // Explicit `account` attaches cleanly.
        assert_eq!(&*callers["sk-explicit"].account, "dup-shared");
    }

    #[test]
    fn resolves_legacy_ledger_quota_account_on_cluster_gateway() {
        let ledger = pagesift_cluster::Ledger::open(pagesift_cluster::LedgerConfig::new(
            pagesift_cluster::MonoClock::new(),
            false,
        ))
        .unwrap();
        let legacy_key = "sk-gw-legacy";
        let legacy_fp = legacy_fnv1a_fingerprint(legacy_key);
        ledger.set_limits(
            &legacy_fp,
            pagesift_cluster::AccountLimits {
                pages_per_day: Some(10),
                requests_per_minute: None,
            },
        );
        let hold = ledger.reserve(&legacy_fp, 1).unwrap();
        ledger.settle(&hold, 1).unwrap();

        let mut raw = HashMap::new();
        raw.insert(
            legacy_key.to_owned(),
            Arc::new(KeyLimits::new().with_pages_per_day(10)),
        );
        let (callers, _) = resolve_callers(raw, None, Some(&ledger)).unwrap();
        assert_eq!(&*callers[legacy_key].account, legacy_fp.as_str());
    }

    #[test]
    fn p2_06_dual_legacy_and_sha_namespaces_are_not_silently_ignored() {
        let dir = tempfile::tempdir().unwrap();
        let store = Store::open_dir(dir.path()).unwrap();
        let k = "sk-dual-upgrade";
        let legacy_fp = legacy_fnv1a_fingerprint(k);
        let sha_fp = fingerprint(k);

        // Both 1.6.0 legacy namespace AND 1.6.1 SHA-256 namespace hold persisted data.
        store
            .context_for(Some(&format!("acct:{legacy_fp}")))
            .put_learn_bytes("default", b"{}")
            .unwrap();
        store
            .context_for(Some(&format!("acct:{sha_fp}")))
            .put_learn_bytes("default", b"{}")
            .unwrap();

        let res = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
            crate::router(
                Arc::new(crate::test_engine::FakeEngine::open()),
                crate::Config {
                    api_keys: vec![k.to_owned()],
                    store: Some(store),
                    ..Default::default()
                },
            )
        }));
        assert!(
            res.is_err(),
            "dual legacy + SHA-256 namespaces for the same key must fail closed instead of silently dropping legacy data"
        );
    }

    #[test]
    fn p2_06_corrupt_task_record_during_legacy_probe_does_not_silently_switch_to_fresh_namespace() {
        let dir = tempfile::tempdir().unwrap();
        let store = Store::open_dir(dir.path()).unwrap();
        // Seed a corrupt monitor record whose owner cannot be inspected.
        store
            .tasks()
            .kv_put("monitor/corrupt-probe", b"{corrupt json")
            .unwrap();

        let res = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
            crate::router(
                Arc::new(crate::test_engine::FakeEngine::open()),
                crate::Config {
                    api_keys: vec!["sk-probe-corrupt".to_owned()],
                    store: Some(store),
                    ..Default::default()
                },
            )
        }));
        assert!(
            res.is_err(),
            "unreadable persisted owner record during legacy tenant probe must fail closed instead of pretending key is fresh"
        );
    }

    #[test]
    fn p2_06_detects_agent_and_attachment_access_legacy_namespaces_and_explicit_disambiguation() {
        // 1. Legacy account existing ONLY in `agent/` is detected and preserved.
        {
            let dir = tempfile::tempdir().unwrap();
            let store = Store::open_dir(dir.path()).unwrap();
            let k = "sk-agent-only-160";
            let legacy_fp = legacy_fnv1a_fingerprint(k);
            store
                .tasks()
                .kv_put(
                    "agent/ag-1",
                    serde_json::json!({ "owner": legacy_fp })
                        .to_string()
                        .as_bytes(),
                )
                .unwrap();
            let mut raw = HashMap::new();
            raw.insert(k.to_owned(), Arc::new(KeyLimits::new()));
            let (callers, _) = resolve_callers(raw, Some(&store), None).unwrap();
            assert_eq!(&*callers[k].account, legacy_fp.as_str());
        }

        // 2. Legacy account existing ONLY in `attachment-access/{account}/{id}` is detected and preserved.
        {
            let dir = tempfile::tempdir().unwrap();
            let store = Store::open_dir(dir.path()).unwrap();
            let k = "sk-attach-only-160";
            let legacy_fp = legacy_fnv1a_fingerprint(k);
            store
                .tasks()
                .kv_put(
                    format!("attachment-access/{legacy_fp}/att-1"),
                    b"9999999999999",
                )
                .unwrap();
            let mut raw = HashMap::new();
            raw.insert(k.to_owned(), Arc::new(KeyLimits::new()));
            let (callers, _) = resolve_callers(raw, Some(&store), None).unwrap();
            assert_eq!(&*callers[k].account, legacy_fp.as_str());
        }

        // 3. Corrupt `agent/` or `attachment-access/` record during legacy probe fails closed.
        for corrupt_key in ["agent/broken", "attachment-access/broken", "job/broken"] {
            let dir = tempfile::tempdir().unwrap();
            let store = Store::open_dir(dir.path()).unwrap();
            store.tasks().kv_put(corrupt_key, b"{not-json").unwrap();
            let mut raw = HashMap::new();
            raw.insert("sk-corrupt-check".to_owned(), Arc::new(KeyLimits::new()));
            let err = resolve_callers(raw, Some(&store), None)
                .expect_err(&format!("corrupt {corrupt_key} must fail closed"));
            assert_eq!(err.code, crate::ErrorCode::Internal, "{err}");
        }

        // 4. Dual legacy + SHA-256 namespaces can be explicitly disambiguated via `KeyLimits::with_account`.
        {
            let dir = tempfile::tempdir().unwrap();
            let store = Store::open_dir(dir.path()).unwrap();
            let k = "sk-dual-resolved";
            let legacy_fp = legacy_fnv1a_fingerprint(k);
            let sha_fp = fingerprint(k);
            store
                .context_for(Some(&format!("acct:{legacy_fp}")))
                .put_learn_bytes("default", b"{}")
                .unwrap();
            store
                .context_for(Some(&format!("acct:{sha_fp}")))
                .put_learn_bytes("default", b"{}")
                .unwrap();
            let mut raw = HashMap::new();
            raw.insert(
                k.to_owned(),
                Arc::new(KeyLimits::new().with_account("chosen-explicit")),
            );
            let (callers, _) = resolve_callers(raw, Some(&store), None).unwrap();
            assert_eq!(&*callers[k].account, "chosen-explicit");
        }
    }

    #[test]
    fn p2_06_underlying_store_read_error_fails_closed_instead_of_switching_to_fresh() {
        let dir = tempfile::tempdir().unwrap();
        let store = Store::open_dir(dir.path()).unwrap();
        let k = "sk-store-io-err";
        let legacy_fp = legacy_fnv1a_fingerprint(k);
        // Write a real Context entry into the segment file.
        store
            .context_for(Some(&format!("acct:{legacy_fp}")))
            .put_learn_bytes("default", b"{\"ok\":true}")
            .unwrap();
        // Corrupt/truncate all segment files on disk so `Store::existing_tenants()` hits a real I/O or CRC error.
        let mut corrupted = 0usize;
        for entry in std::fs::read_dir(dir.path()).unwrap() {
            let path = entry.unwrap().path();
            if path.is_file()
                && path
                    .extension()
                    .and_then(|e| e.to_str())
                    .is_some_and(|ext| ext == "seg" || ext == "log")
            {
                std::fs::write(&path, b"").unwrap();
                corrupted += 1;
            }
        }
        assert!(
            corrupted > 0,
            "expected at least one segment file under {:?} to corrupt",
            dir.path()
        );
        let mut raw = HashMap::new();
        raw.insert(k.to_owned(), Arc::new(KeyLimits::new()));
        let err = resolve_callers(raw, Some(&store), None)
            .expect_err("underlying Store read error during legacy probe must fail closed");
        assert_eq!(err.code, crate::ErrorCode::Internal, "{err}");
    }

    #[test]
    fn p2_06_webhook_hook_seq_and_ack_subkeys_do_not_break_startup() {
        let dir = tempfile::tempdir().unwrap();
        let store = Store::open_dir(dir.path()).unwrap();
        // Real webhook jobs write plain numeric counters under `job/{id}/hook_seq` and `job/{id}/hook_ack`.
        store.tasks().kv_put("job/j-1/hook_seq", b"2").unwrap();
        store.tasks().kv_put("job/j-1/hook_ack", b"1").unwrap();
        let mut raw = HashMap::new();
        raw.insert(
            "sk-fresh-with-webhooks".to_owned(),
            Arc::new(KeyLimits::new()),
        );
        let (callers, _) = resolve_callers(raw, Some(&store), None)
            .expect("derived job/{id}/hook_* subkeys must not be misread as corrupt job metadata");
        assert_eq!(
            &*callers["sk-fresh-with-webhooks"].account,
            fingerprint("sk-fresh-with-webhooks")
        );
    }
}
