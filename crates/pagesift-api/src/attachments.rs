//! 附件的存储与下载：响应里只列 `{id, format, bytes, hash}`，内容用 `GET /v2/attachments/{id}` 取。
//!
//! - 保留期与结果相同（[`crate::jobs::JOB_TTL`]，任务结果的保留期）；每次再出现同一内容就续期。
//! - 内容寻址（id 是 SHA-256 前缀加扩展名），同一内容只存一份。
//! - 占用有上限：内存预算（`pagesift_net::resources::budget`）的 `δ`，超出时先淘汰最早过期的。
//! - 先存在进程内存里。服务开了任务持久化（有存储）时同时写进存储（HTTP API；MCP 只用内存），内存里淘汰了、
//!   或者服务重启了，下载接口从存储里取，到期前链接一直有效；0.90 只存内存，重启或被新图片挤掉后
//!   链接就 404，而任务结果还在。到期的在启动时和下次访问时从存储删掉。
//! - 有温备时（网关的任务镜像），附件的写入和删除同任务记录一样复制到温备，快照里也带着；
//!   单个附件超过一条复制记录的上限（帧上限的一半，[`crate::JobLog::max_op`]）时只存本地、记警告，
//!   温备接管后这个附件的链接 404。
//! - 没有存储时只在内存里：重启后没了，内存满了先淘汰最早过期的（易失缓存，不是可靠交付）。
//! - 下载接口除了 API Key 鉴权外还校验账户授权（`attachment-access/<account>/<id>`）：未授权返回 404，不泄漏存在性。
//! - 全局内容寻址与跨账户内容去重保留；同一内容被多个账户各自抓取时，授权关系独立维护。
//! - 升级前已存、没有授权记录的附件，启动时从已持久化的任务结果回填；回填失败只记警告、不阻塞启动。

use std::collections::{BTreeMap, HashMap, HashSet};
use std::sync::{Arc, Mutex, OnceLock};
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

use axum::extract::{Extension, Path, State};
use axum::http::header;
use axum::response::{IntoResponse, Response};

use crate::error::{ApiError, ErrorCode};
use crate::limits::Caller;
use crate::types::Document;
use crate::types::attachment::mime_of;

/// 存储里附件内容的键（`attachment/<id>`）、到期时刻的键（`attachment-exp/<id>`，Unix 毫秒）
/// 以及账户访问授权的键（`attachment-access/<account>/<id>`，Unix 毫秒）。
fn data_key(id: &str) -> String {
    format!("attachment/{id}")
}

fn exp_key(id: &str) -> String {
    format!("attachment-exp/{id}")
}

fn access_key(account: &str, id: &str) -> String {
    format!("attachment-access/{account}/{id}")
}

fn now_ms() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_or(0, |d| d.as_millis() as u64)
}

/// 附件写进的地方：任务存储，和（网关上）把写入复制到温备的镜像。
#[derive(Clone)]
pub(crate) struct Disk {
    pub store: pagesift_store::Store,
    pub mirror: Option<Arc<crate::jobs::mirror::JobMirror>>,
}

/// 删掉一个附件（经镜像，温备上也删），一并删掉其全部授权记录。
fn remove(d: &Disk, id: &str) {
    let mut access_keys = Vec::new();
    if let Ok(keys) = d.store.tasks().kv_keys("attachment-access/") {
        let suffix = format!("/{id}");
        for k in keys {
            if let Ok(s) = String::from_utf8(k)
                && s.ends_with(&suffix)
            {
                access_keys.push(s);
            }
        }
    }
    let r = crate::jobs::mirror::write(&d.store, d.mirror.as_deref(), |w| {
        w.kv_delete(&data_key(id))?;
        w.kv_delete(&exp_key(id))?;
        for ak in &access_keys {
            w.kv_delete(ak)?;
        }
        Ok(())
    });
    if let Err(e) = r {
        tracing::warn!(attachment = %id, error = %e, "deleting an expired attachment failed");
    }
}

/// （键，值）。
type Kv = (String, Vec<u8>);

/// 存储里的全部附件：（（内容键，内容），（到期键，到期时刻），[（授权键，到期时刻）]）。快照用。
pub(crate) fn stored(t: &pagesift_store::Tasks<'_>) -> Vec<(Kv, Kv, Vec<Kv>)> {
    let mut access_map: HashMap<String, Vec<Kv>> = HashMap::new();
    for k in t.kv_keys("attachment-access/").unwrap_or_default() {
        let Ok(k_str) = String::from_utf8(k) else {
            continue;
        };
        let Some(rest) = k_str.strip_prefix("attachment-access/") else {
            continue;
        };
        let Some((_account, id)) = rest.rsplit_once('/') else {
            continue;
        };
        if let Ok(Some(val)) = t.kv_get(&k_str) {
            access_map
                .entry(id.to_string())
                .or_default()
                .push((k_str, val));
        }
    }
    let mut out = Vec::new();
    for k in t.kv_keys("attachment-exp/").unwrap_or_default() {
        let Ok(k) = String::from_utf8(k) else {
            continue;
        };
        let id = &k["attachment-exp/".len()..];
        if let (Ok(Some(exp)), Ok(Some(data))) = (t.kv_get(&k), t.kv_get(data_key(id))) {
            let access = access_map.remove(id).unwrap_or_default();
            out.push(((data_key(id), data), (k.clone(), exp), access));
        }
    }
    out
}

/// 清掉存储里已经到期的附件与授权记录（建服务时调用，在后台做）。要在 tokio 运行时里调用。
pub(crate) fn sweep_expired(d: Disk) {
    tokio::spawn(async move {
        let s = d.store.clone();
        s.blocking(move |s| {
            let now = now_ms();
            let mut expired_ids = HashSet::new();
            for k in s.tasks().kv_keys("attachment-exp/").unwrap_or_default() {
                let Ok(k) = String::from_utf8(k) else {
                    continue;
                };
                let id = &k["attachment-exp/".len()..];
                if read_exp(s, id).is_none_or(|e| e <= now) {
                    expired_ids.insert(id.to_string());
                    remove(&d, id);
                }
            }
            // 清理已过期附件或已到期的授权记录
            for k in s.tasks().kv_keys("attachment-access/").unwrap_or_default() {
                let Ok(k_str) = String::from_utf8(k) else {
                    continue;
                };
                let Some(rest) = k_str.strip_prefix("attachment-access/") else {
                    continue;
                };
                let Some((_, id)) = rest.rsplit_once('/') else {
                    continue;
                };
                // 已随附件一起删掉的授权不再重复写删除记录
                if expired_ids.contains(id) {
                    continue;
                }
                let should_delete = match s.tasks().kv_get(&k_str) {
                    Ok(Some(v)) if v.len() == 8 => {
                        let exp = u64::from_le_bytes(v.as_slice().try_into().unwrap());
                        exp <= now
                    }
                    Ok(Some(_)) => read_exp(s, id).is_none_or(|e| e <= now),
                    _ => true,
                };
                if should_delete {
                    let _ =
                        crate::jobs::mirror::write(s, d.mirror.as_deref(), |w| w.kv_delete(&k_str));
                }
            }
        })
        .await;
    });
}

fn read_exp(s: &pagesift_store::Store, id: &str) -> Option<u64> {
    let v = s.tasks().kv_get(exp_key(id)).ok()??;
    Some(u64::from_le_bytes(v.as_slice().try_into().ok()?))
}

/// 写进存储（已有就只续期）并写入访问授权，有镜像时同时复制到温备。在后台做，不等。
fn save(disk: Option<&Disk>, id: String, data: Arc<[u8]>, account: String) {
    let (Some(d), Ok(rt)) = (disk.cloned(), tokio::runtime::Handle::try_current()) else {
        return;
    };
    let exp = now_ms() + crate::jobs::JOB_TTL.as_millis() as u64;
    rt.spawn(async move {
        let s = d.store.clone();
        s.blocking(move |s| {
            // 超过一条复制记录上限的只存本地
            let mirror = d
                .mirror
                .as_deref()
                .filter(|m| data.len() + data_key(&id).len() < m.max_op());
            if d.mirror.is_some() && mirror.is_none() {
                tracing::warn!(attachment = %id, bytes = data.len(), "attachment too large to replicate to the standby; stored locally only");
            }
            let fresh = read_exp(s, &id).is_none();
            let r = crate::jobs::mirror::write(s, mirror, |w| {
                if fresh {
                    w.kv_put(&data_key(&id), &data)?;
                }
                w.kv_put(&exp_key(&id), &exp.to_le_bytes())?;
                w.kv_put(&access_key(&account, &id), &exp.to_le_bytes())
            });
            if let Err(e) = r {
                tracing::warn!(attachment = %id, error = %e, "saving an attachment failed; it stays in memory only");
            }
        })
        .await;
    });
}

/// 仅将附件访问授权写进存储（数据已在存储中时），到期时刻与附件一致。
fn save_access(disk: Option<&Disk>, id: String, account: String) {
    let (Some(d), Ok(rt)) = (disk.cloned(), tokio::runtime::Handle::try_current()) else {
        return;
    };
    rt.spawn(async move {
        let s = d.store.clone();
        s.blocking(move |s| {
            let Some(exp) = read_exp(s, &id).filter(|e| *e > now_ms()) else {
                return;
            };
            let r = crate::jobs::mirror::write(s, d.mirror.as_deref(), |w| {
                w.kv_put(&access_key(&account, &id), &exp.to_le_bytes())
            });
            if let Err(e) = r {
                tracing::warn!(attachment = %id, account = %account, error = %e, "saving attachment authorization failed");
            }
        })
        .await;
    });
}

/// 从存储取（内存里没有时）。到期的顺手删掉。
pub(crate) async fn load(disk: Option<Disk>, id: String) -> Option<(Arc<[u8]>, &'static str)> {
    let d = disk?;
    let store = d.store.clone();
    store
        .blocking(move |s| {
            let exp = read_exp(s, &id)?;
            if exp <= now_ms() {
                remove(&d, &id);
                return None;
            }
            let data = s.tasks().kv_get(data_key(&id)).ok()??;
            let ext = id.rsplit_once('.').map_or("", |(_, e)| e);
            Some((Arc::from(data), mime_of(ext)))
        })
        .await
}

struct Entry {
    data: Arc<[u8]>,
    mime: &'static str,
    expires: Instant,
}

#[derive(Default)]
struct Store {
    map: HashMap<String, Entry>,
    /// （过期时刻，id）：按过期先后淘汰。
    order: BTreeMap<(Instant, u64), String>,
    /// （账户, id）-> 过期时刻
    access: HashMap<(String, String), Instant>,
    seq: u64,
    bytes: usize,
}

fn store() -> &'static Mutex<Store> {
    static STORE: OnceLock<Mutex<Store>> = OnceLock::new();
    STORE.get_or_init(Default::default)
}

/// 存储上限：内存预算的 δ（与统计表同一份额）。
fn cap() -> usize {
    // [δ] 全局风险水平的默认值 0.05，作为愿意分给附件的内存份额，与统计表一致
    let delta = 0.05;
    // [策略] 读不到内存预算（非 Linux、刷新之前）时的回退：64 MiB，约 300 张 200 KiB 的图片
    let fallback = 64 * 1024 * 1024;
    pagesift_net::resources::budget()
        .map_or(fallback, |b| (b as f64 * delta) as usize)
        .max(1)
}

/// 记下文档里的附件内容（没有内容的跳过）；有 `disk`（任务持久化的存储）时也写进去。
pub fn keep(doc: &Document, disk: Option<&pagesift_store::Store>) {
    let disk = disk.map(|store| Disk {
        store: store.clone(),
        mirror: None,
    });
    keep_in_for(doc, disk.as_ref(), "anonymous");
}

/// 记下附件并授予调用方账户访问权限；写进任务存储并经镜像复制到温备（HTTP API 用）。
pub(crate) fn keep_in_for(doc: &Document, disk: Option<&Disk>, account: &str) {
    keep_inner(doc, disk, true, account);
}

/// Job pages persist opt-in assets synchronously with their receipt instead.
pub(crate) fn keep_job_in_for(doc: &Document, disk: Option<&Disk>, account: &str) {
    keep_inner(doc, disk, false, account);
}

fn keep_inner(doc: &Document, disk: Option<&Disk>, save_inline: bool, account: &str) {
    if doc.attachments.is_empty() {
        return;
    }
    let expires = Instant::now() + crate::jobs::JOB_TTL;
    let cap = cap();
    let mut st = store().lock().unwrap_or_else(|e| e.into_inner());
    let now = Instant::now();
    // 先清掉过期的
    while let Some((&key, _)) = st.order.first_key_value() {
        if key.0 > now {
            break;
        }
        drop_oldest(&mut st);
    }
    st.access.retain(|_, exp| *exp > now);

    for a in &doc.attachments {
        st.access
            .insert((account.to_string(), a.id.clone()), expires);
        let Some(data) = &a.data else {
            if save_inline {
                save_access(disk, a.id.clone(), account.to_string());
            }
            continue;
        };
        if data.len() > cap {
            // 超过内存上限的只进存储（有的话）
            if save_inline || !a.inline_data {
                save(disk, a.id.clone(), data.clone(), account.to_string());
            }
            continue;
        }
        if let Some(old) = st.map.remove(&a.id) {
            st.bytes -= old.data.len();
            let id = a.id.clone();
            st.order.retain(|_, v| *v != id);
        }
        while st.bytes + data.len() > cap && !st.order.is_empty() {
            drop_oldest(&mut st);
        }
        st.seq += 1;
        let seq = st.seq;
        st.order.insert((expires, seq), a.id.clone());
        st.bytes += data.len();
        st.map.insert(
            a.id.clone(),
            Entry {
                data: data.clone(),
                mime: a.mime(),
                expires,
            },
        );
        if save_inline || !a.inline_data {
            save(disk, a.id.clone(), data.clone(), account.to_string());
        }
    }
}

/// Store the one canonical copy before the job receipt that refers to it.
pub(crate) fn persist_job_asset(
    w: &mut crate::jobs::mirror::Writer<'_>,
    id: &str,
    data: &[u8],
) -> pagesift_store::Result<()> {
    let key = data_key(id);
    if w.kv_get(&key)?.is_none() {
        w.kv_put(&key, data)?;
    }
    let exp = now_ms() + crate::jobs::JOB_TTL.as_millis() as u64;
    w.kv_put(&exp_key(id), &exp.to_le_bytes())
}

/// 为某个账户持久化附件访问授权，到期时刻与附件一致。
pub(crate) fn persist_access(
    w: &mut crate::jobs::mirror::Writer<'_>,
    account: &str,
    id: &str,
) -> pagesift_store::Result<()> {
    let exp = match w.kv_get(&exp_key(id))? {
        Some(v) if v.len() == 8 => u64::from_le_bytes(v.as_slice().try_into().unwrap()),
        _ => now_ms() + crate::jobs::JOB_TTL.as_millis() as u64,
    };
    w.kv_put(&access_key(account, id), &exp.to_le_bytes())
}

/// 检查调用方是否有权访问该附件。先查内存，没有再查存储。
pub(crate) async fn check_access(disk: Option<&Disk>, account: &str, id: &str) -> bool {
    let now = Instant::now();
    let in_memory = {
        let mut st = store().lock().unwrap_or_else(|e| e.into_inner());
        st.access.retain(|_, exp| *exp > now);
        st.access
            .contains_key(&(account.to_string(), id.to_string()))
    };
    if in_memory {
        return true;
    }
    let Some(d) = disk else {
        return false;
    };
    let s = d.store.clone();
    let account_owned = account.to_string();
    let id_owned = id.to_string();
    let mirror = d.mirror.clone();
    let has = s
        .blocking(move |s| {
            let key = access_key(&account_owned, &id_owned);
            let Some(val) = s.tasks().kv_get(&key).ok().flatten() else {
                return false;
            };
            if val.len() == 8 {
                let exp = u64::from_le_bytes(val.as_slice().try_into().unwrap());
                if exp <= now_ms() {
                    let _ = crate::jobs::mirror::write(s, mirror.as_deref(), |w| w.kv_delete(&key));
                    return false;
                }
            }
            true
        })
        .await;
    if has {
        let mut st = store().lock().unwrap_or_else(|e| e.into_inner());
        st.access.insert(
            (account.to_string(), id.to_string()),
            Instant::now() + crate::jobs::JOB_TTL,
        );
        true
    } else {
        false
    }
}

/// 存量附件迁移：从已持久化的任务结果中回填 (job.owner, id) 授权。
pub(crate) fn backfill_from_jobs(d: &Disk) -> pagesift_store::Result<usize> {
    let s = &d.store;
    let t = s.tasks();
    let keys = t.kv_keys("job/")?;
    let now = SystemTime::now();
    let now_ms_val = now_ms();
    let mut count = 0;
    for key in keys {
        let Some(job_id) = std::str::from_utf8(&key)
            .ok()
            .and_then(|k| k.strip_prefix("job/"))
            .filter(|id| !id.contains('/'))
        else {
            continue;
        };
        let Some(meta) = t
            .kv_get(&key)
            .ok()
            .flatten()
            .and_then(|v| serde_json::from_slice::<crate::jobs::store::Meta>(&v).ok())
        else {
            continue;
        };
        let created = UNIX_EPOCH + Duration::from_millis(meta.created_ms);
        if created + crate::jobs::JOB_TTL <= now {
            continue;
        }
        let owner = meta.owner.as_deref().unwrap_or("anonymous");
        let pages_key = crate::jobs::store::list_key(job_id, "pages");
        let len = t.list_len(&pages_key);
        for start in (0..len).step_by(256) {
            let Ok(batch) = t.list_range(&pages_key, start, 256) else {
                continue;
            };
            for raw in batch {
                let Ok(val) = serde_json::from_slice::<serde_json::Value>(&raw) else {
                    continue;
                };
                let Some(attachments) = val.get("attachments").and_then(|v| v.as_array()) else {
                    continue;
                };
                for a in attachments {
                    let Some(id) = a.get("id").and_then(|v| v.as_str()) else {
                        continue;
                    };
                    let Some(exp) = read_exp(s, id).filter(|e| *e > now_ms_val) else {
                        continue;
                    };
                    let acc_key = access_key(owner, id);
                    if t.kv_get(&acc_key).ok().flatten().is_none() {
                        let _ = crate::jobs::mirror::write(s, d.mirror.as_deref(), |w| {
                            w.kv_put(&acc_key, &exp.to_le_bytes())
                        });
                        count += 1;
                    }
                }
            }
        }
    }
    Ok(count)
}

/// 服务启动时在后台回填授权，回填失败只记警告、不阻塞启动。
pub(crate) fn backfill_access(d: Disk) {
    tokio::spawn(async move {
        let d_clone = d.clone();
        let r = d
            .store
            .blocking(move |_| backfill_from_jobs(&d_clone))
            .await;
        match r {
            Ok(n) => {
                if n > 0 {
                    tracing::info!(
                        count = n,
                        "backfilled attachment authorizations from saved jobs"
                    );
                }
            }
            Err(e) => {
                tracing::warn!(error = %e, "backfilling attachment authorizations failed");
            }
        }
    });
}

fn drop_oldest(st: &mut Store) {
    if let Some((_, id)) = st.order.pop_first()
        && let Some(e) = st.map.remove(&id)
    {
        st.bytes -= e.data.len();
    }
}

/// 取一个附件：（内容，MIME 类型）。过期或不存在时为 `None`。
pub fn get(id: &str) -> Option<(Arc<[u8]>, &'static str)> {
    let st = store().lock().unwrap_or_else(|e| e.into_inner());
    st.map
        .get(id)
        .filter(|e| e.expires > Instant::now())
        .map(|e| (e.data.clone(), e.mime))
}

/// `GET /v2/attachments/{id}`。
pub(crate) async fn download(
    State(s): State<crate::server::AppState>,
    caller: Option<Extension<Caller>>,
    Path(id): Path<String>,
) -> Result<Response, ApiError> {
    let account = caller
        .as_ref()
        .map_or("anonymous", |Extension(c)| &*c.account);
    if !check_access(s.jobs.disk().as_ref(), account, &id).await {
        return Err(ApiError::new(
            ErrorCode::NotFound,
            format!("attachment {id} not found or expired"),
        ));
    }
    let found = match get(&id) {
        Some(x) => Some(x),
        None => load(s.jobs.disk(), id.clone()).await,
    };
    let Some((data, mime)) = found else {
        return Err(ApiError::new(
            ErrorCode::NotFound,
            format!("attachment {id} not found or expired"),
        ));
    };
    Ok((
        [
            (header::CONTENT_TYPE, mime),
            (header::CACHE_CONTROL, "private, max-age=86400, immutable"),
        ],
        data.to_vec(),
    )
        .into_response())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::types::Attachment;

    #[test]
    fn kept_attachments_can_be_fetched_by_id() {
        let mut doc = Document::default();
        doc.attachments
            .push(Attachment::new(b"GIF89a-test".to_vec(), "gif", None));
        keep(&doc, None);
        let id = &doc.attachments[0].id;
        let (data, mime) = get(id).unwrap();
        assert_eq!(&*data, b"GIF89a-test");
        assert_eq!(mime, "image/gif");
        assert!(get("0000.png").is_none());
    }

    #[tokio::test]
    async fn synchronous_job_asset_expires_and_is_removed() {
        let dir = tempfile::tempdir().unwrap();
        let store = pagesift_store::Store::open_dir(dir.path()).unwrap();
        let a = Attachment::new(b"\x89PNG\r\n\x1a\nexpiry-test".to_vec(), "png", None);
        crate::jobs::mirror::write(&store, None, |w| {
            persist_job_asset(w, &a.id, a.data.as_deref().unwrap())
        })
        .unwrap();
        let disk = Disk {
            store: store.clone(),
            mirror: None,
        };
        let (bytes, mime) = load(Some(disk.clone()), a.id.clone()).await.unwrap();
        assert_eq!(bytes.as_ref(), a.data.as_deref().unwrap());
        assert_eq!(mime, "image/png");
        store
            .tasks()
            .kv_put(exp_key(&a.id), &1u64.to_le_bytes())
            .unwrap();
        assert!(load(Some(disk), a.id.clone()).await.is_none());
        assert!(store.tasks().kv_get(data_key(&a.id)).unwrap().is_none());
    }

    /// 有存储时，内存里没有了（重启、被挤掉）也能从存储读回；0.90 这时 404。
    #[tokio::test(flavor = "multi_thread")]
    async fn persisted_attachments_survive_losing_the_memory_copy() {
        let dir = tempfile::tempdir().unwrap();
        let store = pagesift_store::Store::open_dir(dir.path()).unwrap();
        let mut doc = Document::default();
        doc.attachments
            .push(Attachment::new(b"\x89PNG-persisted".to_vec(), "png", None));
        keep(&doc, Some(&store));
        let id = doc.attachments[0].id.clone();
        // 等后台写完
        for _ in 0..500 {
            if read_exp(&store, &id).is_some() {
                break;
            }
            tokio::time::sleep(std::time::Duration::from_millis(5)).await;
        }
        // 模拟重启：清空内存
        {
            let mut st = super::store().lock().unwrap();
            let e = st.map.remove(&id).unwrap();
            st.bytes -= e.data.len();
            st.order.retain(|_, v| *v != id);
        }
        assert!(get(&id).is_none());
        let (data, mime) = load(
            Some(Disk {
                store: store.clone(),
                mirror: None,
            }),
            id.clone(),
        )
        .await
        .unwrap();
        assert_eq!((&*data, mime), (&b"\x89PNG-persisted"[..], "image/png"));
        // 到期的不再给出，并从存储删掉
        store
            .tasks()
            .kv_put(exp_key(&id), &1u64.to_le_bytes())
            .unwrap();
        assert!(
            load(
                Some(Disk {
                    store: store.clone(),
                    mirror: None
                }),
                id.clone()
            )
            .await
            .is_none()
        );
        assert!(store.tasks().kv_get(data_key(&id)).unwrap().is_none());
    }

    struct Ops(Mutex<Vec<Vec<u8>>>);

    impl crate::JobLog for Ops {
        fn append(&self, op: Vec<u8>) -> Result<(), String> {
            self.0.lock().unwrap().push(op);
            Ok(())
        }
    }

    /// 网关上的附件随任务记录复制：温备回放日志后能下载，从快照重建的副本也能（0.90 不复制附件）。
    #[tokio::test(flavor = "multi_thread")]
    async fn attachments_replicate_with_the_job_records() {
        let dirs: Vec<_> = (0..3).map(|_| tempfile::tempdir().unwrap()).collect();
        let open = |i: usize| pagesift_store::Store::open_dir(dirs[i].path()).unwrap();
        let (src, dst, from_snapshot) = (open(0), open(1), open(2));
        let log = Arc::new(Ops(Mutex::default()));
        let mirror = crate::JobMirror::new(log.clone());
        let mut doc = Document::default();
        doc.attachments
            .push(Attachment::new(b"GIF89a-replicated".to_vec(), "gif", None));
        let disk = Disk {
            store: src.clone(),
            mirror: Some(mirror.clone()),
        };
        keep_in_for(&doc, Some(&disk), "alice");
        let id = doc.attachments[0].id.clone();
        for _ in 0..500 {
            if log.0.lock().unwrap().len() >= 3 {
                break;
            }
            tokio::time::sleep(std::time::Duration::from_millis(5)).await;
        }
        for op in log.0.lock().unwrap().iter() {
            crate::apply_op(&dst, op).unwrap();
        }
        for op in mirror.dump(&src) {
            crate::apply_op(&from_snapshot, &op).unwrap();
        }
        for s in [dst, from_snapshot] {
            // 授权键本身也随日志/快照复制；直接查存储，避免内存缓存掩盖缺失
            assert!(
                s.tasks()
                    .kv_get(access_key("alice", &id))
                    .unwrap()
                    .is_some()
            );
            assert!(s.tasks().kv_get(access_key("bob", &id)).unwrap().is_none());
            let d = Disk {
                store: s,
                mirror: None,
            };
            assert!(check_access(Some(&d), "alice", &id).await);
            assert!(!check_access(Some(&d), "bob", &id).await);
            let (data, mime) = load(Some(d), id.clone()).await.expect("replicated");
            assert_eq!((&*data, mime), (&b"GIF89a-replicated"[..], "image/gif"));
        }
    }

    /// 存量附件在升级后通过任务结果回填授权（0.90 / 1.2.3 存量无 access 键）。
    #[tokio::test(flavor = "multi_thread")]
    async fn legacy_attachments_are_backfilled_from_saved_jobs() {
        let dir = tempfile::tempdir().unwrap();
        let store = pagesift_store::Store::open_dir(dir.path()).unwrap();
        let disk = Disk {
            store: store.clone(),
            mirror: None,
        };
        let a = Attachment::new(b"\x89PNG-legacy".to_vec(), "png", None);
        let id = a.id.clone();
        // 模拟升级前存入的附件：只有 attachment/{id} 和 attachment-exp/{id}，无 attachment-access
        crate::jobs::mirror::write(&store, None, |w| {
            persist_job_asset(w, &id, a.data.as_deref().unwrap())
        })
        .unwrap();

        // 存一个属于 alice 的任务，结果中引用该附件
        let mut doc = Document::default();
        doc.attachments.push(a);
        let meta = crate::jobs::store::Meta {
            kind: crate::jobs::JobKind::BatchScrape,
            state: crate::jobs::JobState::Completed,
            created_ms: now_ms(),
            total: 1,
            completed: 1,
            owner: Some("alice".into()),
            webhook: None,
            crawl: None,
            batch: None,
            summary: None,
        };
        store
            .tasks()
            .kv_put("job/job-1", &serde_json::to_vec(&meta).unwrap())
            .unwrap();
        let page_raw = serde_json::to_vec(&doc).unwrap();
        store
            .tasks()
            .list_push("job/job-1/pages", &page_raw)
            .unwrap();

        // 回填前：alice 和 bob 均无授权
        assert!(!check_access(Some(&disk), "alice", &id).await);
        assert!(!check_access(Some(&disk), "bob", &id).await);

        // 执行回填
        let count = backfill_from_jobs(&disk).unwrap();
        assert_eq!(count, 1);

        // 回填后：alice 获得授权，bob 依然无权
        assert!(check_access(Some(&disk), "alice", &id).await);
        assert!(!check_access(Some(&disk), "bob", &id).await);
    }
}
