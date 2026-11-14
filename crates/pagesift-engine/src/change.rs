//! 变化检测（Firecrawl 的 `changeTracking` 格式）：和存储里同一 URL、同一 `tag` 上一次抓到的
//! 正文 Markdown 比较，给出 `new`/`same`/`changed`/`removed`，按需附上 git diff 风格的差异。
//!
//! 每次抓取都把这次的正文追加成一份快照（内容按哈希去重，没变的页面不多占空间）。
//! 快照的第一个字节标明种类：正文，或者页面已经不存在（404/410）。

use std::time::SystemTime;

use pagesift_api::types::{ChangeTracking, Format, ScrapeOptions};
use pagesift_api::{ApiError, rfc3339};
use pagesift_net::Url;
use pagesift_store::{ContentHash, Store};
use serde_json::{Map, Value, json};

const PAGE: u8 = b'M';
const GONE: u8 = b'R';
/// git diff 默认的上下文行数。
const CONTEXT_LINES: usize = 3;

/// 服务端可信的变化检测隔离作用域：由认证租户、鉴权/Cookie 头部指纹和会话标识共同决定，
/// 不信任客户端通过 `tag` 自行声明的隔离值。
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct ChangeScope {
    pub owner: Option<String>,
    pub credentials_hash: Option<String>,
    pub session_scope: Option<String>,
}

impl ChangeScope {
    pub fn from_scrape_options(opts: &ScrapeOptions) -> Self {
        let mut creds: Vec<(String, &str)> = opts
            .headers
            .iter()
            .filter_map(|(k, v)| {
                let lower = k.to_ascii_lowercase();
                is_identity_header(&lower).then_some((lower, v.as_str()))
            })
            .collect();
        creds.sort_unstable();
        let credentials_hash = (!creds.is_empty()).then(|| {
            let mut h = blake3::Hasher::new();
            h.update(b"pagesift:changeTracking:creds:v1");
            for (k, v) in creds {
                update_len_prefixed(&mut h, k.as_bytes());
                update_len_prefixed(&mut h, v.as_bytes());
            }
            h.finalize().to_hex().to_string()
        });
        let session_scope = match (opts.client_session.as_deref(), opts.keep_session) {
            (Some(s), true) => Some(format!("{s}:keep_session")),
            (Some(s), false) => Some(s.to_string()),
            (None, true) => Some("keep_session".to_string()),
            (None, false) => None,
        };
        Self {
            owner: opts.owner.clone(),
            credentials_hash,
            session_scope,
        }
    }
}

fn is_identity_header(lower: &str) -> bool {
    matches!(
        lower,
        "authorization"
            | "proxy-authorization"
            | "cookie"
            | "cookie2"
            | "x-api-key"
            | "api-key"
            | "x-auth-token"
            | "x-access-token"
            | "x-session-id"
            | "x-session-token"
            | "x-csrf-token"
            | "x-xsrf-token"
            | "x-user-id"
            | "x-tenant-id"
            | "x-org-id"
    ) || [
        "auth", "cookie", "session", "token", "secret", "key", "csrf", "xsrf", "cred", "user",
        "tenant",
    ]
    .iter()
    .any(|needle| lower.contains(needle))
}

fn update_len_prefixed(h: &mut blake3::Hasher, bytes: &[u8]) {
    h.update(&(bytes.len() as u64).to_le_bytes());
    h.update(bytes);
}

fn update_opt_field(h: &mut blake3::Hasher, value: Option<&str>) {
    match value {
        Some(v) => {
            h.update(&[1]);
            update_len_prefixed(h, v.as_bytes());
        }
        None => {
            h.update(&[0]);
        }
    }
}

/// 解析好的 `changeTracking` 格式参数。
#[derive(Debug, Clone, Default, PartialEq)]
pub struct Tracking {
    pub git_diff: bool,
    pub json: bool,
    pub schema: Option<Value>,
    pub prompt: Option<String>,
    pub tag: Option<String>,
}

impl Tracking {
    pub fn parse(f: &Format) -> Result<Self, ApiError> {
        let mut t = Tracking {
            schema: f.options.get("schema").cloned(),
            prompt: f
                .options
                .get("prompt")
                .and_then(Value::as_str)
                .map(String::from),
            tag: f
                .options
                .get("tag")
                .and_then(Value::as_str)
                .map(String::from),
            ..Default::default()
        };
        match f.options.get("modes") {
            None | Some(Value::Null) => {}
            Some(Value::Array(modes)) => {
                for m in modes {
                    match m.as_str() {
                        Some("git-diff") => t.git_diff = true,
                        Some("json") => t.json = true,
                        _ => {
                            return Err(ApiError::invalid(format!(
                                "changeTracking mode must be \"git-diff\" or \"json\", got {m}"
                            )));
                        }
                    }
                }
            }
            Some(other) => {
                return Err(ApiError::invalid(format!(
                    "changeTracking modes must be an array, got {other}"
                )));
            }
        }
        Ok(t)
    }

    /// 按隔离作用域生成存储键：同一 URL 在不同认证租户、鉴权请求头、会话或 `tag` 下各自独立。
    pub fn key_for(&self, scope: &ChangeScope, url: &str) -> String {
        let url = Url::parse(url)
            .map(|u| crate::crawl::normalize(&u, false))
            .unwrap_or_else(|_| url.to_string());
        let mut h = blake3::Hasher::new();
        h.update(b"pagesift:changeTracking:v2");
        update_len_prefixed(&mut h, url.as_bytes());
        update_opt_field(&mut h, self.tag.as_deref());
        update_opt_field(&mut h, scope.owner.as_deref());
        update_opt_field(&mut h, scope.credentials_hash.as_deref());
        update_opt_field(&mut h, scope.session_scope.as_deref());
        format!("ct:v2:{}", h.finalize().to_hex())
    }

    /// 默认无租户、无凭据头部作用域下的存储键。
    #[cfg(test)]
    fn key(&self, url: &str) -> String {
        self.key_for(&ChangeScope::default(), url)
    }
}

/// 上一份快照的内容。
#[derive(Debug, Clone, PartialEq)]
pub enum Previous {
    Page(String),
    Gone,
    /// 快照还在历史里，但内容已经被存储淘汰：只能按哈希判断变没变，给不出差异。
    Evicted,
}

/// 比较的结果：给 Firecrawl 的字段，加上计算 json 模式需要的上一版正文。
pub struct Outcome {
    pub tracking: ChangeTracking,
    pub previous: Previous,
    pub warnings: Vec<String>,
}

/// 这一次抓到的内容。
#[derive(Debug, Clone, Copy, PartialEq)]
pub enum Current<'a> {
    Page(&'a str),
    /// 404/410。
    Gone,
    /// 条件请求得到 304：内容和上一次一样，不另存快照。
    NotModified,
}

fn encode(current: Current<'_>) -> Vec<u8> {
    match current {
        Current::Page(md) => {
            let mut v = Vec::with_capacity(md.len() + 1);
            v.push(PAGE);
            v.extend_from_slice(md.as_bytes());
            v
        }
        Current::Gone | Current::NotModified => vec![GONE],
    }
}

fn decode(bytes: &[u8]) -> Previous {
    match bytes.split_first() {
        Some((&PAGE, md)) => Previous::Page(String::from_utf8_lossy(md).into_owned()),
        _ => Previous::Gone,
    }
}

/// 上一份快照：(时间, 内容哈希, 内容)。
type Snapshot = (SystemTime, ContentHash, Previous);

/// 取出上一份快照，再存入这一次的（`current` 为 `None` 时不存）。
fn load_and_store(
    store: &Store,
    key: &str,
    current: Option<&[u8]>,
    now: SystemTime,
) -> Result<(Option<Snapshot>, Vec<String>), pagesift_store::Error> {
    let snaps = store.snapshots();
    let previous = match snaps.latest(key)? {
        Some(meta) => {
            let content = match snaps.get(&meta.hash)? {
                Some(bytes) => decode(&bytes),
                None => Previous::Evicted,
            };
            Some((meta.at, meta.hash, content))
        }
        None => None,
    };
    let mut warnings = Vec::new();
    let Some(current) = current else {
        return Ok((previous, warnings));
    };
    match snaps.put_at(key, current, now) {
        Ok(_) => {}
        Err(pagesift_store::Error::ValueTooLarge { .. }) => {
            warnings.push("change_tracking_snapshot_too_large".into());
        }
        Err(e) => return Err(e),
    }
    Ok((previous, warnings))
}

/// 在可信作用域下与上一次比较并记录这一次。
pub async fn track_scoped(
    store: &Store,
    tracking: &Tracking,
    scope: &ChangeScope,
    url: &str,
    current: Current<'_>,
) -> Result<Outcome, ApiError> {
    let key = tracking.key_for(scope, url);
    let bytes = encode(current);
    let hash = ContentHash::of(&bytes);
    let now = SystemTime::now();
    let stored = (current != Current::NotModified).then_some(bytes);
    let (previous, mut warnings) = store
        .blocking(move |s| load_and_store(s, &key, stored.as_deref(), now))
        .await
        .map_err(|e| {
            ApiError::new(
                pagesift_api::ErrorCode::Internal,
                format!("change tracking store: {e}"),
            )
        })?;
    let Some((at, prev_hash, prev)) = previous else {
        return Ok(Outcome {
            tracking: result(None, "new", None, None),
            previous: Previous::Gone,
            warnings,
        });
    };
    let at = Some(rfc3339(at));
    let cur = match current {
        Current::Page(md) => md,
        Current::Gone => {
            return Ok(Outcome {
                tracking: result(at, "removed", None, None),
                previous: prev,
                warnings,
            });
        }
        Current::NotModified => "",
    };
    if prev_hash == hash || current == Current::NotModified {
        return Ok(Outcome {
            tracking: result(at, "same", None, None),
            previous: prev,
            warnings,
        });
    }
    let old = match &prev {
        Previous::Page(md) => Some(md.as_str()),
        Previous::Gone => Some(""),
        Previous::Evicted => None,
    };
    let (diff, ext) = match old {
        Some(old) => {
            let report = pagesift_diff::diff(old, cur);
            let ext = json!({
                "substantive": report.substantive,
                "stats": report.stats,
            });
            (tracking.git_diff.then(|| git_diff(old, cur)), Some(ext))
        }
        None => {
            if tracking.git_diff {
                warnings.push("change_tracking_previous_evicted".into());
            }
            (None, None)
        }
    };
    Ok(Outcome {
        tracking: result(at, "changed", diff, ext),
        previous: prev,
        warnings,
    })
}

/// 在默认作用域下与上一次比较并记录这一次（仅供本地/单测直连路径使用）。
pub async fn track(
    store: &Store,
    tracking: &Tracking,
    url: &str,
    current: Current<'_>,
) -> Result<Outcome, ApiError> {
    track_scoped(store, tracking, &ChangeScope::default(), url, current).await
}

fn result(
    previous_scrape_at: Option<String>,
    status: &str,
    diff: Option<Value>,
    pagesift: Option<Value>,
) -> ChangeTracking {
    ChangeTracking {
        previous_scrape_at,
        change_status: status.into(),
        visibility: "visible".into(),
        diff,
        json: None,
        pagesift,
    }
}

/// `{"text": ..., "json": ...}`：文本是统一格式的 hunk，JSON 是 parse-diff（Firecrawl 用的库）的结构。
pub fn git_diff(old: &str, new: &str) -> Value {
    let d = pagesift_diff::unified_diff(old, new, CONTEXT_LINES);
    let chunks: Vec<Value> = d
        .hunks
        .iter()
        .map(|h| {
            let changes: Vec<Value> = h
                .lines
                .iter()
                .map(|l| {
                    let content = l.prefixed();
                    match l.kind {
                        pagesift_diff::LineKind::Normal => json!({
                            "type": "normal", "normal": true,
                            "ln1": l.old_line, "ln2": l.new_line, "content": content,
                        }),
                        pagesift_diff::LineKind::Add => json!({
                            "type": "add", "add": true, "ln": l.new_line, "content": content,
                        }),
                        pagesift_diff::LineKind::Del => json!({
                            "type": "del", "del": true, "ln": l.old_line, "content": content,
                        }),
                    }
                })
                .collect();
            json!({
                "content": h.header(),
                "changes": changes,
                "oldStart": h.old_start,
                "oldLines": h.old_lines,
                "newStart": h.new_start,
                "newLines": h.new_lines,
            })
        })
        .collect();
    json!({
        "text": d.text(),
        "json": {
            "files": [{
                "from": null,
                "to": null,
                "chunks": chunks,
                "additions": d.additions,
                "deletions": d.deletions,
            }],
        },
    })
}

/// json 模式：两版提取结果里值不同的字段，写成 `{"字段": {"previous": 旧值, "current": 新值}}`。
pub fn json_changes(previous: &Value, current: &Value) -> Value {
    let empty = Map::new();
    let (p, c) = match (previous, current) {
        (Value::Object(p), Value::Object(c)) => (p, c),
        (Value::Null, Value::Object(c)) => (&empty, c),
        _ if previous == current => return Value::Object(Map::new()),
        _ => return json!({ "previous": previous, "current": current }),
    };
    let mut out = Map::new();
    for k in p.keys().chain(c.keys().filter(|k| !p.contains_key(*k))) {
        let (pv, cv) = (
            p.get(k).unwrap_or(&Value::Null),
            c.get(k).unwrap_or(&Value::Null),
        );
        if pv != cv {
            out.insert(k.clone(), json!({ "previous": pv, "current": cv }));
        }
    }
    Value::Object(out)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn fmt(v: Value) -> Format {
        serde_json::from_value(v).unwrap()
    }

    #[test]
    fn parses_modes_and_tag() {
        let t = Tracking::parse(&fmt(json!({"type": "changeTracking"}))).unwrap();
        assert_eq!(t, Tracking::default());
        let t = Tracking::parse(&fmt(json!({
            "type": "changeTracking", "modes": ["git-diff", "json"], "tag": "daily",
            "schema": {"type": "object"}, "prompt": "price",
        })))
        .unwrap();
        assert!(t.git_diff && t.json);
        assert_eq!(t.tag.as_deref(), Some("daily"));
        assert_eq!(t.prompt.as_deref(), Some("price"));
        assert!(
            Tracking::parse(&fmt(json!({"type": "changeTracking", "modes": ["word"]}))).is_err()
        );
        assert!(Tracking::parse(&fmt(json!({"type": "changeTracking", "modes": "json"}))).is_err());
        assert_ne!(
            t.key("https://example.com/a#x"),
            Tracking::default().key("https://example.com/a"),
            "tags keep separate histories"
        );
        assert_eq!(
            Tracking::default().key("https://EXAMPLE.com/a#x"),
            Tracking::default().key("https://example.com/a")
        );
    }

    #[test]
    fn git_diff_follows_parse_diff_shape() {
        let v = git_diff("# T\n\nold content\n", "# T\n\nnew content\n");
        assert_eq!(
            v["text"],
            "@@ -1,3 +1,3 @@\n # T\n \n-old content\n+new content"
        );
        let file = &v["json"]["files"][0];
        assert_eq!(file["from"], Value::Null);
        assert_eq!(
            (file["additions"].as_u64(), file["deletions"].as_u64()),
            (Some(1), Some(1))
        );
        let chunk = &file["chunks"][0];
        assert_eq!(chunk["content"], "@@ -1,3 +1,3 @@");
        assert_eq!(
            chunk["changes"][0],
            json!({"type": "normal", "normal": true, "ln1": 1, "ln2": 1, "content": " # T"})
        );
        assert_eq!(
            chunk["changes"][2],
            json!({"type": "del", "del": true, "ln": 3, "content": "-old content"})
        );
        assert_eq!(
            chunk["changes"][3],
            json!({"type": "add", "add": true, "ln": 3, "content": "+new content"})
        );
    }

    #[test]
    fn json_changes_lists_changed_fields_only() {
        let p = json!({"title": "A", "price": 10, "gone": 1});
        let c = json!({"title": "A", "price": 12, "new": true});
        assert_eq!(
            json_changes(&p, &c),
            json!({
                "price": {"previous": 10, "current": 12},
                "gone": {"previous": 1, "current": null},
                "new": {"previous": null, "current": true},
            })
        );
        assert_eq!(json_changes(&json!([1]), &json!([1])), json!({}));
        assert_eq!(
            json_changes(&json!([1]), &json!([2])),
            json!({"previous": [1], "current": [2]})
        );
    }

    #[tokio::test]
    async fn statuses_follow_the_snapshot_history() {
        let dir = tempfile::tempdir().unwrap();
        let store = Store::open_dir(dir.path()).unwrap();
        let t = Tracking {
            git_diff: true,
            ..Default::default()
        };
        let url = "https://example.com/p";
        let first = track(&store, &t, url, Current::Page("# P\n\nv1\n"))
            .await
            .unwrap();
        assert_eq!(first.tracking.change_status, "new");
        assert_eq!(first.tracking.previous_scrape_at, None);
        let same = track(&store, &t, url, Current::Page("# P\n\nv1\n"))
            .await
            .unwrap();
        assert_eq!(same.tracking.change_status, "same");
        assert!(same.tracking.previous_scrape_at.is_some());
        assert!(same.tracking.diff.is_none());
        let changed = track(&store, &t, url, Current::Page("# P\n\nv2\n"))
            .await
            .unwrap();
        assert_eq!(changed.tracking.change_status, "changed");
        assert_eq!(changed.previous, Previous::Page("# P\n\nv1\n".into()));
        assert_eq!(
            changed.tracking.diff.as_ref().unwrap()["text"],
            "@@ -1,3 +1,3 @@\n # P\n \n-v1\n+v2"
        );
        assert_eq!(
            changed.tracking.pagesift.as_ref().unwrap()["substantive"],
            true
        );
        // 304：和上一次一样，也不另存快照
        let unchanged = track(&store, &t, url, Current::NotModified).await.unwrap();
        assert_eq!(unchanged.tracking.change_status, "same");
        assert_eq!(unchanged.previous, Previous::Page("# P\n\nv2\n".into()));
        assert_eq!(store.snapshots().list(&t.key(url), ..).unwrap().len(), 3);
        let removed = track(&store, &t, url, Current::Gone).await.unwrap();
        assert_eq!(removed.tracking.change_status, "removed");
        let back = track(&store, &t, url, Current::Page("# P\n\nv3\n"))
            .await
            .unwrap();
        assert_eq!(back.tracking.change_status, "changed");
        assert_eq!(back.previous, Previous::Gone);
        // 另一个 tag 有自己的历史
        let tagged = Tracking {
            tag: Some("x".into()),
            ..Default::default()
        };
        let other = track(&store, &tagged, url, Current::Page("# P\n\nv3\n"))
            .await
            .unwrap();
        assert_eq!(other.tracking.change_status, "new");
    }

    #[tokio::test]
    async fn noise_only_changes_are_marked_not_substantive() {
        let dir = tempfile::tempdir().unwrap();
        let store = Store::open_dir(dir.path()).unwrap();
        let t = Tracking::default();
        let url = "https://example.com/n";
        track(
            &store,
            &t,
            url,
            Current::Page("Updated 2026-01-01.\n\nBody text.\n"),
        )
        .await
        .unwrap();
        let o = track(
            &store,
            &t,
            url,
            Current::Page("Updated 2026-02-03.\n\nBody text.\n"),
        )
        .await
        .unwrap();
        assert_eq!(o.tracking.change_status, "changed");
        assert!(o.tracking.diff.is_none(), "git-diff mode not requested");
        assert_eq!(o.tracking.pagesift.unwrap()["substantive"], false);
    }

    #[tokio::test]
    async fn isolates_owners_anonymous_credentials_and_tag_delimiters() {
        let dir = tempfile::tempdir().unwrap();
        let store = Store::open_dir(dir.path()).unwrap();
        let url = "https://example.com/account";
        let t = Tracking {
            git_diff: true,
            tag: Some("daily".into()),
            ..Default::default()
        };

        let mut opts_a = ScrapeOptions {
            owner: Some("acct:alice".into()),
            ..Default::default()
        };
        opts_a
            .headers
            .insert("Cookie".into(), "session=alice-secret".into());
        let scope_a = ChangeScope::from_scrape_options(&opts_a);

        let opts_b = ScrapeOptions {
            owner: Some("acct:bob".into()),
            ..Default::default()
        };
        let scope_b = ChangeScope::from_scrape_options(&opts_b);

        let opts_anon = ScrapeOptions {
            owner: Some(ScrapeOptions::ANONYMOUS_OWNER.into()),
            ..Default::default()
        };
        let scope_anon = ChangeScope::from_scrape_options(&opts_anon);

        let a1 = track_scoped(&store, &t, &scope_a, url, Current::Page("secret A"))
            .await
            .unwrap();
        assert_eq!(a1.tracking.change_status, "new");

        // 不同租户（同 URL + 同 tag）不能读到 Alice 的历史或 diff
        let b1 = track_scoped(&store, &t, &scope_b, url, Current::Page("public B"))
            .await
            .unwrap();
        assert_eq!(b1.tracking.change_status, "new");
        assert!(b1.tracking.diff.is_none());

        // 匿名 HTTP 不能读到认证租户或本地无作用域的历史
        let anon1 = track_scoped(&store, &t, &scope_anon, url, Current::Page("anon"))
            .await
            .unwrap();
        assert_eq!(anon1.tracking.change_status, "new");
        let local1 = track_scoped(
            &store,
            &t,
            &ChangeScope::default(),
            url,
            Current::Page("local"),
        )
        .await
        .unwrap();
        assert_eq!(local1.tracking.change_status, "new");

        // 同租户下不同 Cookie / Authorization 头也必须隔离（忽略 header 大小写差异）
        let mut opts_a_other_cookie = ScrapeOptions {
            owner: Some("acct:alice".into()),
            ..Default::default()
        };
        opts_a_other_cookie
            .headers
            .insert("cookie".into(), "session=other".into());
        let scope_a_other = ChangeScope::from_scrape_options(&opts_a_other_cookie);
        let a_other = track_scoped(&store, &t, &scope_a_other, url, Current::Page("other"))
            .await
            .unwrap();
        assert_eq!(a_other.tracking.change_status, "new");

        // 大小写不同但同名 Cookie 的同一租户请求应命中同一历史
        let mut opts_a_same = ScrapeOptions {
            owner: Some("acct:alice".into()),
            ..Default::default()
        };
        opts_a_same
            .headers
            .insert("COOKIE".into(), "session=alice-secret".into());
        let scope_a_same = ChangeScope::from_scrape_options(&opts_a_same);
        let a2 = track_scoped(&store, &t, &scope_a_same, url, Current::Page("secret A v2"))
            .await
            .unwrap();
        assert_eq!(a2.tracking.change_status, "changed");
        assert_eq!(a2.previous, Previous::Page("secret A".into()));

        // 客户端在 tag 中注入 `\u{1f}` 或目标 owner 字符串无法越界碰撞
        let forged_tag = Tracking {
            git_diff: true,
            tag: Some("daily\u{1f}acct:alice".into()),
            ..Default::default()
        };
        let forged = track_scoped(&store, &forged_tag, &scope_b, url, Current::Page("forged"))
            .await
            .unwrap();
        assert_eq!(forged.tracking.change_status, "new");
        assert!(forged.tracking.diff.is_none());
    }
}
