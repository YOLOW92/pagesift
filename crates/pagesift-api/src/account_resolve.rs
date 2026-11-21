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
pub(crate) fn resolve_callers(
    raw_keys: HashMap<String, Arc<KeyLimits>>,
    store: Option<&Store>,
    ledger: Option<&pagesift_cluster::Ledger>,
) -> (HashMap<String, Caller>, HashMap<String, Arc<KeyLimits>>) {
    if raw_keys.is_empty() {
        return (HashMap::new(), HashMap::new());
    }

    let mut explicit_accounts: HashSet<String> = HashSet::new();
    let mut unnamed_name_counts: HashMap<String, usize> = HashMap::new();
    let mut legacy_fp_counts: HashMap<String, usize> = HashMap::new();

    for (k, l) in &raw_keys {
        if let Some(acct) = l
            .account
            .as_deref()
            .and_then(|raw| validate_account_id(raw).ok())
        {
            explicit_accounts.insert(acct);
        } else if let Some(n) = l.name.as_deref().map(str::trim).filter(|s| !s.is_empty()) {
            *unnamed_name_counts.entry(n.to_owned()).or_default() += 1;
        } else {
            *legacy_fp_counts
                .entry(legacy_fnv1a_fingerprint(k))
                .or_default() += 1;
        }
    }

    // Collect every account ID worth probing against an existing store/ledger.
    let mut probe_accounts: HashSet<String> = HashSet::new();
    for (k, l) in &raw_keys {
        if l.account
            .as_deref()
            .and_then(|raw| validate_account_id(raw).ok())
            .is_some()
        {
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
        find_existing_accounts(&probe_accounts, store, ledger)
    };

    // Emit an actionable warning if 2+ keys share a display `name` that has legacy persisted data.
    for (dup_name, count) in &unnamed_name_counts {
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

    let mut callers = HashMap::with_capacity(raw_keys.len());
    let mut accounts: HashMap<String, Arc<KeyLimits>> = HashMap::with_capacity(raw_keys.len());

    for (k, l) in raw_keys {
        let resolved_acct = if let Some(acct) = l
            .account
            .as_deref()
            .and_then(|raw| validate_account_id(raw).ok())
        {
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
                    let unique = legacy_fp_counts.get(&fp).copied().unwrap_or(0) == 1;
                    unique.then_some((fp, false))
                }
            };

            match legacy_candidate {
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

        if let Some(prev) = accounts.get(&resolved_acct)
            && (prev.requests_per_minute != l.requests_per_minute
                || prev.max_concurrent_jobs != l.max_concurrent_jobs
                || prev.pages_per_day != l.pages_per_day)
        {
            tracing::warn!(
                account = %resolved_acct,
                "multiple API keys share the same explicit account with differing limits; \
                 keeping the tighter configuration is recommended"
            );
        }
        accounts.insert(resolved_acct.clone(), l.clone());
        let caller = Caller::with_resolved_account(resolved_acct, l, true);
        callers.insert(k, caller);
    }

    (callers, accounts)
}

/// Probe existing persisted data across Context, Loop Sync, DataGraph, Daily Quota, Monitors,
/// Jobs, and cluster Ledger for the candidate account IDs.
fn find_existing_accounts(
    candidates: &HashSet<String>,
    store: Option<&Store>,
    ledger: Option<&pagesift_cluster::Ledger>,
) -> HashSet<String> {
    let mut found = HashSet::new();

    if let Some(l) = ledger {
        for acct in candidates {
            if l.has_account_usage(acct) {
                found.insert(acct.clone());
            }
        }
    }

    let Some(st) = store else {
        return found;
    };

    // 1. Context / Loop Sync / DataGraph (single index pass without reading values)
    let tenant_ids: Vec<String> = candidates
        .iter()
        .filter(|a| !found.contains(*a))
        .map(|a| format!("acct:{a}"))
        .collect();
    if !tenant_ids.is_empty()
        && let Ok(existing_tenants) = st.existing_tenants(&tenant_ids)
    {
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
        if st.tasks().kv_get(usage_key(acct)).ok().flatten().is_some() {
            found.insert(acct.clone());
        }
    }

    // 3. Persisted Monitors (`monitor/{id}`) and Jobs (`job/{id}`)
    if found.len() < candidates.len() {
        for prefix in ["monitor/", "job/"] {
            let Ok(keys) = st.tasks().kv_keys(prefix) else {
                continue;
            };
            for key in keys {
                if found.len() == candidates.len() {
                    break;
                }
                if let Ok(Some(bytes)) = st.tasks().kv_get(&key)
                    && let Ok(probe) = serde_json::from_slice::<OwnerProbe>(&bytes)
                    && let Some(owner) = probe.owner
                    && candidates.contains(&owner)
                {
                    found.insert(owner);
                }
            }
        }
    }

    found
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

        let (callers, _accounts) = resolve_callers(raw, Some(&store), None);

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
        let (callers, _) = resolve_callers(raw, None, Some(&ledger));
        assert_eq!(&*callers[legacy_key].account, legacy_fp.as_str());
    }
}
