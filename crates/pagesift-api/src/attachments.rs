//! 附件的存储与下载：响应里只列 `{id, format, bytes, hash}`，内容用 `GET /v2/attachments/{id}` 取。
//!
//! - 保留期与结果相同（[`crate::jobs::JOB_TTL`]，任务结果的保留期）；每次再出现同一内容就续期。
//! - 内容寻址（id 是 SHA-256 前缀加扩展名），同一内容只存一份。
//! - 占用有上限：内存预算（`pagesift_net::resources::budget`）的 `δ`，超出时先淘汰最早过期的。
//! - 存在进程内存里，服务重启后附件不在了（任务结果若持久化，重启后仍在，附件需要重抓）。
//! - 下载接口和其他抓取接口一样在鉴权之后；id 是 128 位哈希前缀，猜不到。

use std::collections::{BTreeMap, HashMap};
use std::sync::{Arc, Mutex, OnceLock};
use std::time::Instant;

use axum::extract::Path;
use axum::http::header;
use axum::response::{IntoResponse, Response};

use crate::error::{ApiError, ErrorCode};
use crate::types::Document;

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

/// 记下文档里的附件内容（没有内容的跳过）。
pub fn keep(doc: &Document) {
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
pub(crate) async fn download(Path(id): Path<String>) -> Result<Response, ApiError> {
    let Some((data, mime)) = get(&id) else {
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
        keep(&doc);
        let id = &doc.attachments[0].id;
        let (data, mime) = get(id).unwrap();
        assert_eq!(&*data, b"GIF89a-test");
        assert_eq!(mime, "image/gif");
        assert!(get("0000.png").is_none());
    }
}
