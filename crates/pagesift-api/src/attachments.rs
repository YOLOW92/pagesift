//! 附件的存储与下载：响应里只列 `{id, format, bytes, hash}`，内容用 `GET /v2/attachments/{id}` 取。
//!
//! - 保留期与结果相同（[`crate::jobs::JOB_TTL`]，任务结果的保留期）；每次再出现同一内容就续期。
//! - 内容寻址（id 是 SHA-256 前缀加扩展名），同一内容只存一份。
//! - 占用有上限：内存预算（`pagesift_net::resources::budget`）的 `δ`，超出时先淘汰最早过期的。
//! - 先存在进程内存里。服务开了任务持久化（有存储）时同时写进存储（HTTP API；MCP 只用内存），内存里淘汰了、
//!   或者服务重启了，下载接口从存储里取，到期前链接一直有效；0.90 只存内存，重启或被新图片挤掉后
//!   链接就 404，而任务结果还在。到期的在启动时和下次访问时从存储删掉。
//! - 附件不随任务记录复制到温备（只复制任务记录本身）：温备接管后，接管前的附件链接 404，需要重抓。
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

/// 清掉 `store` 里已经到期的附件（建服务时调用，在后台做）。要在 tokio 运行时里调用。
pub(crate) fn sweep_expired(store: pagesift_store::Store) {
    tokio::spawn(async move {
        store
            .blocking(|s| {
                let t = s.tasks();
                let now = now_ms();
                for k in t.kv_keys("attachment-exp/").unwrap_or_default() {
                    let Ok(k) = String::from_utf8(k) else {
                        continue;
                    };
                    let id = &k["attachment-exp/".len()..];
                    if read_exp(s, id).is_none_or(|e| e <= now) {
                        let _ = t.kv_delete(data_key(id));
                        let _ = t.kv_delete(&k);
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

/// 写进存储（已有就只续期）。在后台做，不等。
fn save(store: Option<&pagesift_store::Store>, id: String, data: Arc<[u8]>) {
    let (Some(store), Ok(rt)) = (store.cloned(), tokio::runtime::Handle::try_current()) else {
        return;
    };
    let exp = now_ms() + crate::jobs::JOB_TTL.as_millis() as u64;
    rt.spawn(async move {
        store
            .blocking(move |s| {
                let t = s.tasks();
                let r = match read_exp(s, &id) {
                    Some(_) => t.kv_put(exp_key(&id), &exp.to_le_bytes()),
                    None => t
                        .kv_put(data_key(&id), &data)
                        .and_then(|()| t.kv_put(exp_key(&id), &exp.to_le_bytes())),
                };
                if let Err(e) = r {
                    tracing::warn!(attachment = %id, error = %e, "saving an attachment failed; it stays in memory only");
                }
            })
            .await;
    });
}

/// 从存储取（内存里没有时）。到期的顺手删掉。
async fn load(
    store: Option<pagesift_store::Store>,
    id: String,
) -> Option<(Arc<[u8]>, &'static str)> {
    let store = store?;
    store
        .blocking(move |s| {
            let t = s.tasks();
            let exp = read_exp(s, &id)?;
            if exp <= now_ms() {
                let _ = t.kv_delete(data_key(&id));
                let _ = t.kv_delete(exp_key(&id));
                return None;
            }
            let data = t.kv_get(data_key(&id)).ok()??;
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
        None => load(s.jobs.store().cloned(), id.clone()).await,
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
        let (data, mime) = load(Some(store.clone()), id.clone()).await.unwrap();
        assert_eq!((&*data, mime), (&b"\x89PNG-persisted"[..], "image/png"));
        // 到期的不再给出，并从存储删掉
        store
            .tasks()
            .kv_put(exp_key(&id), &1u64.to_le_bytes())
            .unwrap();
        assert!(load(Some(store.clone()), id.clone()).await.is_none());
        assert!(store.tasks().kv_get(data_key(&id)).unwrap().is_none());
    }
}
