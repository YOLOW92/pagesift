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
//! - 下载接口和其他抓取接口一样在鉴权之后；id 是 128 位哈希前缀，猜不到。

use std::collections::{BTreeMap, HashMap};
use std::sync::{Arc, Mutex, OnceLock};
use std::time::Instant;

use axum::extract::{Path, State};
use axum::http::header;
use axum::response::{IntoResponse, Response};

use crate::error::{ApiError, ErrorCode};
use crate::types::Document;
use crate::types::attachment::mime_of;

/// 存储里附件内容的键（`attachment/<id>`）和到期时刻的键（`attachment-exp/<id>`，Unix 毫秒）。
/// 先写内容再写到期时刻：有到期时刻就一定有内容。
fn data_key(id: &str) -> String {
    format!("attachment/{id}")
}

fn exp_key(id: &str) -> String {
    format!("attachment-exp/{id}")
}

fn now_ms() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map_or(0, |d| d.as_millis() as u64)
}

/// 附件写进的地方：任务存储，和（网关上）把写入复制到温备的镜像。
#[derive(Clone)]
pub(crate) struct Disk {
    pub store: pagesift_store::Store,
    pub mirror: Option<Arc<crate::jobs::mirror::JobMirror>>,
}

/// 删掉一个附件（经镜像，温备上也删）。
fn remove(d: &Disk, id: &str) {
    let r = crate::jobs::mirror::write(&d.store, d.mirror.as_deref(), |w| {
        w.kv_delete(&data_key(id))?;
        w.kv_delete(&exp_key(id))
    });
    if let Err(e) = r {
        tracing::warn!(attachment = %id, error = %e, "deleting an expired attachment failed");
    }
}

/// 存储里的全部附件：（（内容键，内容），（到期键，到期时刻））。快照用。
pub(crate) fn stored(t: &pagesift_store::Tasks<'_>) -> Vec<((String, Vec<u8>), (String, Vec<u8>))> {
    let mut out = Vec::new();
    for k in t.kv_keys("attachment-exp/").unwrap_or_default() {
        let Ok(k) = String::from_utf8(k) else {
            continue;
        };
        let id = &k["attachment-exp/".len()..];
        if let (Ok(Some(exp)), Ok(Some(data))) = (t.kv_get(&k), t.kv_get(data_key(id))) {
            out.push(((data_key(id), data), (k.clone(), exp)));
        }
    }
    out
}

/// 清掉存储里已经到期的附件（建服务时调用，在后台做）。要在 tokio 运行时里调用。
pub(crate) fn sweep_expired(d: Disk) {
    tokio::spawn(async move {
        let s = d.store.clone();
        s.blocking(move |s| {
            let now = now_ms();
            for k in s.tasks().kv_keys("attachment-exp/").unwrap_or_default() {
                let Ok(k) = String::from_utf8(k) else {
                    continue;
                };
                let id = &k["attachment-exp/".len()..];
                if read_exp(s, id).is_none_or(|e| e <= now) {
                    remove(&d, id);
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

/// 写进存储（已有就只续期），有镜像时同时复制到温备。在后台做，不等。
fn save(disk: Option<&Disk>, id: String, data: Arc<[u8]>) {
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
                w.kv_put(&exp_key(&id), &exp.to_le_bytes())
            });
            if let Err(e) = r {
                tracing::warn!(attachment = %id, error = %e, "saving an attachment failed; it stays in memory only");
            }
        })
        .await;
    });
}

/// 从存储取（内存里没有时）。到期的顺手删掉。
async fn load(disk: Option<Disk>, id: String) -> Option<(Arc<[u8]>, &'static str)> {
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
    keep_in(doc, disk.as_ref());
}

/// 同 [`keep`]，写进任务存储并经镜像复制到温备（HTTP API 用）。
pub(crate) fn keep_in(doc: &Document, disk: Option<&Disk>) {
    if doc.attachments.iter().all(|a| a.data.is_none()) {
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
    for a in &doc.attachments {
        let Some(data) = &a.data else { continue };
        if data.len() > cap {
            // 超过内存上限的只进存储（有的话）
            save(disk, a.id.clone(), data.clone());
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
        save(disk, a.id.clone(), data.clone());
    }
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
    Path(id): Path<String>,
) -> Result<Response, ApiError> {
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
        keep_in(&doc, Some(&disk));
        let id = doc.attachments[0].id.clone();
        for _ in 0..500 {
            if log.0.lock().unwrap().len() >= 2 {
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
            let d = Disk {
                store: s,
                mirror: None,
            };
            let (data, mime) = load(Some(d), id.clone()).await.expect("replicated");
            assert_eq!((&*data, mime), (&b"GIF89a-replicated"[..], "image/gif"));
        }
    }
}
