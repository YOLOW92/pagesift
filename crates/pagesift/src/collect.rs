//! Incremental JSON API collection with page checkpoints and acknowledged events.
//! A source must have one writer and a stable, authoritative listing to enable
//! deletion. Delivery is at least once; destinations must deduplicate events.
use std::collections::{BTreeMap, BTreeSet};

use anyhow::{Context, Result, bail, ensure};
use pagesift_net::FetchRequest;
use pagesift_policy::{Policy, RobotsMode};
use pagesift_store::Store;
use serde::{Deserialize, Serialize};
use serde_json::Value;
use url::Url;

/// Site-specific JSON pointers. A null next pointer explicitly ends a listing;
/// a missing pointer is an error, never evidence of deletion.
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct JsonAdapter {
    pub items_pointer: String,
    pub id_pointer: String,
    pub next_pointer: String,
}

/// Built-in site protocol or an explicit JSON API mapping.
#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "camelCase")]
pub enum Adapter {
    Json(JsonAdapter),
    WordPress,
}

impl From<JsonAdapter> for Adapter {
    fn from(value: JsonAdapter) -> Self {
        Self::Json(value)
    }
}

impl Adapter {
    fn mapping(&self) -> JsonAdapter {
        match self {
            Self::Json(a) => a.clone(),
            Self::WordPress => JsonAdapter {
                items_pointer: "/items".into(),
                id_pointer: "/id".into(),
                next_pointer: "/next".into(),
            },
        }
    }

    fn normalize(&self, url: &Url, response: &pagesift_net::FetchResponse) -> Result<Value> {
        let body: Value = serde_json::from_slice(&response.body)?;
        match self {
            Self::Json(_) => Ok(body),
            Self::WordPress => {
                // WordPress REST API pagination contract; do not infer the last
                // page from a short/empty array when metadata is unavailable.
                let rows = body
                    .as_array()
                    .context("WordPress collection must be an array")?;
                let pages: u64 = response
                    .header("x-wp-totalpages")
                    .context("missing X-WP-TotalPages")?
                    .parse()?;
                let total: u64 = response
                    .header("x-wp-total")
                    .context("missing X-WP-Total")?
                    .parse()?;
                let values: Vec<_> = url.query_pairs().filter(|(k, _)| k == "page").collect();
                ensure!(values.len() <= 1, "duplicate page parameter");
                let page_sizes: Vec<_> =
                    url.query_pairs().filter(|(k, _)| k == "per_page").collect();
                ensure!(page_sizes.len() <= 1, "duplicate per_page parameter");
                let size: u64 = page_sizes
                    .first()
                    .map(|(_, v)| v.parse())
                    .transpose()?
                    .unwrap_or(10);
                ensure!((1..=100).contains(&size), "invalid WordPress page size");
                ensure!(
                    pages == total.div_ceil(size),
                    "inconsistent WordPress totals"
                );
                let page: u64 = values
                    .first()
                    .map(|(_, v)| v.parse())
                    .transpose()?
                    .unwrap_or(1);
                ensure!(
                    page > 0 && (page <= pages || (page == 1 && pages == 0 && total == 0)),
                    "invalid WordPress page metadata"
                );
                ensure!(
                    (total == 0) == rows.is_empty(),
                    "empty or inconsistent WordPress listing"
                );
                let next = if page < pages {
                    let mut next = url.clone();
                    let params: Vec<_> = url
                        .query_pairs()
                        .filter(|(k, _)| k != "page")
                        .map(|(k, v)| (k.into_owned(), v.into_owned()))
                        .collect();
                    next.set_query(None);
                    next.query_pairs_mut()
                        .extend_pairs(params)
                        .append_pair("page", &(page + 1).to_string());
                    Some(next.to_string())
                } else {
                    None
                };
                Ok(serde_json::json!({"items": rows, "next": next}))
            }
        }
    }
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct ItemEvent {
    pub source: String,
    pub id: String,
    /// None denotes an authoritative deletion.
    pub value: Option<Value>,
}

#[derive(Clone, Default, Serialize, Deserialize)]
struct State {
    version: u32,
    #[serde(default)]
    expected_total: Option<usize>,
    inventory: BTreeMap<String, String>,
    seen: BTreeMap<String, String>,
    next: Option<String>,
    visited: BTreeSet<String>,
    active: bool,
    deleting: bool,
    adapter: String,
    authoritative: bool,
}

struct Canonical<'a>(&'a Value);

impl Serialize for Canonical<'_> {
    fn serialize<S: serde::Serializer>(
        &self,
        serializer: S,
    ) -> std::result::Result<S::Ok, S::Error> {
        use serde::ser::{SerializeMap, SerializeSeq};
        match self.0 {
            Value::Object(map) => {
                let mut entries: Vec<_> = map.iter().collect();
                entries.sort_unstable_by(|a, b| a.0.cmp(b.0));
                let mut out = serializer.serialize_map(Some(entries.len()))?;
                for (key, value) in entries {
                    out.serialize_entry(key, &Canonical(value))?;
                }
                out.end()
            }
            Value::Array(values) => {
                let mut out = serializer.serialize_seq(Some(values.len()))?;
                for value in values {
                    out.serialize_element(&Canonical(value))?;
                }
                out.end()
            }
            value => value.serialize(serializer),
        }
    }
}

fn revision(value: &Value) -> String {
    // Explicit sorting also works when a consumer enables preserve_order on
    // serde_json. Equal objects with different wire key order share a revision.
    let bytes = serde_json::to_vec(&Canonical(value)).expect("JSON values serialize infallibly");
    blake3::hash(&bytes).to_hex().to_string()
}

/// Maximum serialized checkpoint, bounding the in-memory inventory and journal.
/// Larger collections should use separate source partitions.
pub const MAX_COLLECTION_BYTES: usize = 32 * 1024 * 1024;

/// A persistent single-writer collection. Every acknowledged page and its next
/// URL are stored together in one CRC-protected record. Unacknowledged pages
/// replay after a crash. Store::flush is called before advancing the checkpoint.
pub struct Collection<'a> {
    store: &'a Store,
    source: String,
    key: Vec<u8>,
    adapter: Adapter,
    state: State,
}

impl<'a> Collection<'a> {
    pub fn open(store: &'a Store, source: &str, adapter: impl Into<Adapter>) -> Result<Self> {
        ensure!(!source.trim().is_empty(), "source must not be empty");
        let adapter = adapter.into();
        let key = format!("pagesift:collection:{source}").into_bytes();
        let mut state = match store.tasks().kv_get(&key)? {
            None => State::default(),
            Some(bytes) => {
                ensure!(
                    bytes.len() <= MAX_COLLECTION_BYTES,
                    "collection checkpoint exceeds 32 MiB"
                );
                let mut value: Value = serde_json::from_slice(&bytes)?;
                let version = match value.get("version") {
                    None => 0,
                    Some(v) => v.as_u64().context("invalid collection state version")?,
                };
                ensure!(
                    version <= 1,
                    "unsupported collection state version {version}"
                );
                if version == 0 {
                    // Previous checkpoints stored complete payloads. Migrate in
                    // memory, preserving the cursor and already seen identities.
                    for field in ["inventory", "seen"] {
                        for row in value
                            .get_mut(field)
                            .and_then(Value::as_object_mut)
                            .context("invalid legacy collection inventory")?
                            .values_mut()
                        {
                            *row = Value::String(revision(row));
                        }
                    }
                    value["version"] = Value::from(1);
                }
                serde_json::from_value(value)?
            }
        };
        state.version = 1;
        Ok(Self {
            store,
            source: source.into(),
            key,
            adapter,
            state,
        })
    }

    fn save(&self) -> Result<()> {
        let bytes = serde_json::to_vec(&self.state)?;
        ensure!(
            bytes.len() <= MAX_COLLECTION_BYTES,
            "collection checkpoint exceeds 32 MiB; partition the source"
        );
        self.store.tasks().kv_put(&self.key, &bytes)?;
        self.store.flush()?;
        Ok(())
    }

    /// Begin or resume. An active scan retains its saved cursor and policy.
    pub fn begin(&mut self, start: &Url, authoritative: bool) -> Result<()> {
        ensure!(
            matches!(start.scheme(), "http" | "https"),
            "HTTP URL required"
        );
        let signature = serde_json::to_string(&(start.as_str(), &self.adapter))?;
        if self.state.active {
            ensure!(
                self.state.adapter == signature && self.state.authoritative == authoritative,
                "cannot change adapter, start URL or deletion policy during a scan"
            );
            return Ok(());
        }
        self.state.expected_total = None;
        if matches!(self.adapter, Adapter::WordPress) && authoritative {
            ensure!(
                start
                    .query_pairs()
                    .filter(|(k, _)| k == "page")
                    .all(|(_, v)| v == "1"),
                "authoritative scan must start on page 1"
            );
        }
        self.state.seen.clear();
        self.state.visited.clear();
        self.state.next = Some(start.to_string());
        self.state.active = true;
        self.state.deleting = false;
        self.state.adapter = signature;
        self.state.authoritative = authoritative;
        self.save()
    }

    /// Discard an incomplete scan while preserving the committed inventory.
    /// Use this when the server listing changed or its cursor expired.
    pub fn abort(&mut self) -> Result<()> {
        self.state.active = false;
        self.state.deleting = false;
        self.state.next = None;
        self.state.seen.clear();
        self.state.visited.clear();
        self.save()
    }

    /// Feed an HTML listing with explicit item IDs into the same incremental
    /// pipeline. The caller must confirm terminal status; absence of rel=next
    /// alone is not proof of a complete listing.
    pub fn accept_html_page(
        &mut self,
        url: &Url,
        html: &str,
        next: Option<&str>,
        terminal: bool,
        deliver: &mut impl FnMut(&ItemEvent) -> Result<()>,
    ) -> Result<()> {
        ensure!(
            terminal == next.is_none(),
            "supply next URL or explicit terminal confirmation"
        );
        let mapping = self.adapter.mapping();
        ensure!(
            mapping.items_pointer == "/items"
                && mapping.id_pointer == "/id"
                && mapping.next_pointer == "/next",
            "HTML discovery requires the standard /items, /id, /next mapping"
        );
        ensure!(
            matches!(self.adapter, Adapter::Json(_)),
            "HTML discovery requires a JSON mapping, not WordPress"
        );
        let items = pagesift_extract::discover_items(html).map_err(anyhow::Error::msg)?;
        self.accept_page(
            url,
            &serde_json::json!({"items": items, "next": next}),
            deliver,
        )
    }

    pub fn next_url(&self) -> Option<&str> {
        self.state.next.as_deref()
    }
    pub fn is_active(&self) -> bool {
        self.state.active
    }

    /// Validate the entire page before delivering anything. A callback returning
    /// success means the destination has durably accepted the event.
    pub fn accept_page(
        &mut self,
        url: &Url,
        page: &Value,
        deliver: &mut impl FnMut(&ItemEvent) -> Result<()>,
    ) -> Result<()> {
        ensure!(
            self.state.active && !self.state.deleting,
            "no page expected"
        );
        ensure!(
            self.next_url() == Some(url.as_str()),
            "unexpected checkpoint URL"
        );
        let mapping = self.adapter.mapping();
        let rows = page
            .pointer(&mapping.items_pointer)
            .and_then(Value::as_array)
            .context("items pointer must reference an array")?;
        let next = page
            .pointer(&mapping.next_pointer)
            .context("missing next pointer")?;
        let next = if next.is_null() {
            None
        } else {
            let mut next = url.join(next.as_str().context("next must be a URL string or null")?)?;
            next.set_fragment(None);
            ensure!(
                next.origin() == url.origin(),
                "pagination cannot cross origins"
            );
            ensure!(
                next != *url && !self.state.visited.contains(next.as_str()),
                "pagination cycle"
            );
            Some(next.to_string())
        };
        let mut parsed = BTreeMap::new();
        for row in rows {
            let id = row
                .pointer(&mapping.id_pointer)
                .context("missing business id")?;
            let id = match id {
                Value::String(s) if !s.trim().is_empty() => s.clone(),
                Value::Number(n) => n.to_string(),
                _ => bail!("business id must be a nonempty string or number"),
            };
            ensure!(
                !parsed.contains_key(&id) && !self.state.seen.contains_key(&id),
                "duplicate business id {id}; listing may have shifted"
            );
            parsed.insert(id, row.clone());
        }
        let mut projected = self.state.clone();
        let revisions: BTreeMap<_, _> = parsed
            .iter()
            .map(|(id, value)| (id.clone(), revision(value)))
            .collect();
        projected.seen.extend(revisions.clone());
        projected.visited.insert(url.to_string());
        projected.next = next;
        projected.deleting = projected.next.is_none();
        ensure!(
            serde_json::to_vec(&projected)?.len() <= MAX_COLLECTION_BYTES,
            "collection checkpoint exceeds 32 MiB; partition the source"
        );
        for (id, value) in &parsed {
            if self.state.inventory.get(id) != revisions.get(id) {
                deliver(&ItemEvent {
                    source: self.source.clone(),
                    id: id.clone(),
                    value: Some(value.clone()),
                })?;
            }
        }
        self.state = projected;
        self.save()
    }

    /// Only a validated terminal page enables finalization. Errors, page limits,
    /// cancellation and missing pagination metadata can never cause deletion.
    pub fn finish(&mut self, deliver: &mut impl FnMut(&ItemEvent) -> Result<()>) -> Result<()> {
        ensure!(
            self.state.active && self.state.deleting,
            "scan is incomplete"
        );
        if self.state.authoritative {
            for id in self
                .state
                .inventory
                .keys()
                .filter(|id| !self.state.seen.contains_key(*id))
            {
                deliver(&ItemEvent {
                    source: self.source.clone(),
                    id: id.clone(),
                    value: None,
                })?;
            }
            self.state.inventory = self.state.seen.clone();
        } else {
            self.state.inventory.extend(self.state.seen.clone());
        }
        self.state.active = false;
        self.state.deleting = false;
        self.state.next = None;
        self.save()
    }

    /// Fetch real API pages through the normal SSRF, robots and rate-limit
    /// policy. A bounded invocation can be resumed by opening the same source.
    pub async fn run(
        &mut self,
        policy: &Policy,
        max_pages: usize,
        deliver: &mut impl FnMut(&ItemEvent) -> Result<()>,
    ) -> Result<bool> {
        for _ in 0..max_pages {
            let Some(next) = self.next_url().map(str::to_owned) else {
                break;
            };
            let url = Url::parse(&next)?;
            let response = policy
                .fetch(FetchRequest::new(next), RobotsMode::Respect)
                .await?;
            ensure!(
                response.status == 200 && !response.truncated,
                "incomplete API response"
            );
            ensure!(
                response.final_url.origin() == url.origin(),
                "API redirected across origins"
            );
            let body = self.adapter.normalize(&url, &response)?;
            if matches!(self.adapter, Adapter::WordPress) {
                let total: usize = response
                    .header("x-wp-total")
                    .context("missing total")?
                    .parse()?;
                ensure!(
                    self.state.expected_total.is_none_or(|old| old == total),
                    "listing count changed during scan"
                );
                if body["next"].is_null() {
                    ensure!(
                        self.state.seen.len()
                            + body["items"].as_array().context("missing rows")?.len()
                            == total,
                        "incomplete WordPress listing"
                    );
                }
                self.state.expected_total = Some(total);
            }
            self.accept_page(&url, &body, deliver)?;
        }
        if self.state.deleting {
            self.finish(deliver)?;
        }
        Ok(!self.state.active)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;
    fn adapter() -> JsonAdapter {
        JsonAdapter {
            items_pointer: "/items".into(),
            id_pointer: "/id".into(),
            next_pointer: "/next".into(),
        }
    }
    #[test]
    fn resumes_checkpoint_replays_failed_delivery_and_deletes_only_at_end() -> Result<()> {
        let dir = tempfile::tempdir()?;
        let url = Url::parse("https://example.com/api?page=1")?;
        let next = url.join("?page=2")?;
        let mut events = Vec::new();
        {
            let store = Store::open_dir(dir.path())?;
            let mut c = Collection::open(&store, "inventory", adapter())?;
            c.begin(&url, true)?;
            let page = json!({"items":[{"id":"A","url":"/old"}],"next":"?page=2"});
            assert!(
                c.accept_page(&url, &page, &mut |_| bail!("sink failed"))
                    .is_err()
            );
            assert_eq!(c.next_url(), Some(url.as_str()));
            c.accept_page(&url, &page, &mut |e| {
                events.push(e.clone());
                Ok(())
            })?;
            assert!(c.finish(&mut |_| Ok(())).is_err());
        }
        {
            let store = Store::open_dir(dir.path())?;
            let mut c = Collection::open(&store, "inventory", adapter())?;
            c.begin(&url, true)?;
            assert_eq!(c.next_url(), Some(next.as_str()));
            c.accept_page(
                &next,
                &json!({"items":[{"id":"B"}],"next":null}),
                &mut |e| {
                    events.push(e.clone());
                    Ok(())
                },
            )?;
            c.finish(&mut |_| Ok(()))?;
            c.begin(&url, true)?;
            assert!(
                c.accept_page(&url, &json!({"items":[]}), &mut |_| Ok(()))
                    .is_err()
            );
            assert!(c.finish(&mut |_| Ok(())).is_err());
            c.accept_page(
                &url,
                &json!({"items":[{"id":"A","url":"/new"}],"next":null}),
                &mut |e| {
                    events.push(e.clone());
                    Ok(())
                },
            )?;
            assert!(c.finish(&mut |_| bail!("deletion sink failed")).is_err());
        }
        {
            let store = Store::open_dir(dir.path())?;
            let mut c = Collection::open(&store, "inventory", adapter())?;
            c.finish(&mut |e| {
                events.push(e.clone());
                Ok(())
            })?;
            assert!(!c.is_active());
            assert_eq!(events.len(), 4);
            assert_eq!(events.last().unwrap().id, "B");
            assert!(events.last().unwrap().value.is_none());
        }
        Ok(())
    }
}

#[cfg(test)]
mod failure_tests {
    use super::*;
    use serde_json::json;
    #[test]
    fn invalid_pages_are_rejected_before_any_delivery_and_abort_keeps_inventory() -> Result<()> {
        let dir = tempfile::tempdir()?;
        let store = Store::open_dir(dir.path())?;
        let adapter = JsonAdapter {
            items_pointer: "/items".into(),
            id_pointer: "/id".into(),
            next_pointer: "/next".into(),
        };
        let mut c = Collection::open(&store, "inventory", adapter)?;
        let url: Url = "https://example.com/api".parse()?;
        c.begin(&url, true)?;
        for bad in [
            json!({"items":[{"id":"A"}]}),
            json!({"items":[{"id":"A"},{"id":"A"}],"next":null}),
            json!({"items":[{"id":"A"},{}],"next":null}),
            json!({"items":[],"next":"https://other.example/api"}),
            json!({"items":[],"next":"/api"}),
        ] {
            assert!(
                c.accept_page(&url, &bad, &mut |_| panic!("invalid page delivered"))
                    .is_err()
            );
            assert_eq!(c.next_url(), Some(url.as_str()));
        }
        c.accept_html_page(
            &url,
            "<div data-item-id='A'>A</div>",
            None,
            true,
            &mut |_| Ok(()),
        )?;
        c.finish(&mut |_| Ok(()))?;
        c.begin(&url, true)?;
        assert!(
            c.accept_html_page(&url, "", None, false, &mut |_| Ok(()))
                .is_err()
        );
        c.abort()?;
        c.begin(&url, true)?;
        c.accept_page(&url, &json!({"items":[],"next":null}), &mut |_| Ok(()))?;
        let mut deleted = Vec::new();
        c.finish(&mut |e| {
            deleted.push(e.id.clone());
            Ok(())
        })?;
        assert_eq!(deleted, ["A"]);
        Ok(())
    }
}

/// Durable JSONL destination for the CLI. Recover an interrupted final write;
/// reject corrupt complete records instead of appending behind damaged data.
pub struct EventLog(std::fs::File);

impl EventLog {
    pub fn open(path: impl AsRef<std::path::Path>) -> Result<Self> {
        use std::io::{BufRead, Read};
        let file = std::fs::OpenOptions::new()
            .read(true)
            .append(true)
            .create(true)
            .open(path)?;
        let mut reader = std::io::BufReader::new(file.try_clone()?);
        let mut valid = 0u64;
        loop {
            let mut line = Vec::new();
            let n = (&mut reader)
                .take(MAX_COLLECTION_BYTES as u64 + 2)
                .read_until(b'\n', &mut line)?;
            if n == 0 {
                break;
            }
            ensure!(n <= MAX_COLLECTION_BYTES, "event log record exceeds 32 MiB");
            if !line.ends_with(b"\n") {
                file.set_len(valid)?;
                file.sync_data()?;
                break;
            }
            serde_json::from_slice::<ItemEvent>(&line)
                .context("corrupt complete event log record")?;
            valid += n as u64;
        }
        Ok(Self(file))
    }

    pub fn append(&mut self, event: &ItemEvent) -> Result<()> {
        use std::io::Write;
        let mut bytes = serde_json::to_vec(event)?;
        bytes.push(b'\n');
        ensure!(bytes.len() <= MAX_COLLECTION_BYTES, "event exceeds 32 MiB");
        self.0.write_all(&bytes)?;
        self.0.sync_data()?;
        Ok(())
    }
}

#[cfg(test)]
mod log_tests {
    use super::*;
    #[test]
    fn repairs_partial_tail_but_rejects_corrupt_complete_records() -> Result<()> {
        let dir = tempfile::tempdir()?;
        let path = dir.path().join("events.jsonl");
        let event = ItemEvent {
            source: "test".into(),
            id: "A".into(),
            value: None,
        };
        EventLog::open(&path)?.append(&event)?;
        let valid = std::fs::read(&path)?;
        use std::io::Write;
        std::fs::OpenOptions::new()
            .append(true)
            .open(&path)?
            .write_all(b"{\"id\":")?;
        EventLog::open(&path)?;
        assert_eq!(std::fs::read(&path)?, valid);
        std::fs::OpenOptions::new()
            .append(true)
            .open(&path)?
            .write_all(b"bad record\n")?;
        assert!(EventLog::open(&path).is_err());
        Ok(())
    }
}

#[cfg(test)]
mod revision_tests {
    use super::*;
    use serde_json::json;
    #[test]
    fn migrates_legacy_payloads_and_bounds_checkpoints_by_identity_not_body_size() -> Result<()> {
        let dir = tempfile::tempdir()?;
        let store = Store::open_dir(dir.path())?;
        let adapter = JsonAdapter {
            items_pointer: "/items".into(),
            id_pointer: "/id".into(),
            next_pointer: "/next".into(),
        };
        let url: Url = "https://example.com/api".parse()?;
        let row = json!({"id":"A", "body":"editorial content ".repeat(100_000)});
        let legacy = json!({"inventory":{"A":row},"seen":{},"next":null,"visited":[],"active":false,"deleting":false,"adapter":"","authoritative":false});
        store
            .tasks()
            .kv_put(b"pagesift:collection:large", &serde_json::to_vec(&legacy)?)?;
        let mut c = Collection::open(&store, "large", adapter.clone())?;
        c.begin(&url, true)?;
        c.accept_page(&url, &json!({"items":[row],"next":null}), &mut |_| {
            bail!("unchanged legacy item was redelivered")
        })?;
        c.finish(&mut |_| Ok(()))?;
        let checkpoint = store.tasks().kv_get(b"pagesift:collection:large")?.unwrap();
        assert!(checkpoint.len() < 2048, "checkpoint retained full payload");
        let state: Value = serde_json::from_slice(&checkpoint)?;
        assert_eq!(state["version"], 1);
        assert_eq!(state["inventory"]["A"].as_str().unwrap().len(), 64);
        store
            .tasks()
            .kv_put(b"pagesift:collection:future", b"{\"version\":99}")?;
        assert!(Collection::open(&store, "future", adapter).is_err());
        Ok(())
    }
}
