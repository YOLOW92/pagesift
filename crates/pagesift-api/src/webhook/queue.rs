//! 一个任务的 webhook 事件队列：给事件编号，按序号顺序逐个投递，不丢事件。
//!
//! - 序号 `seq` 在任务内从 1 起单调递增，`eventId` 是 `{任务 id}:{seq}`。编号和入队在同一把锁里，
//!   队列里的顺序就是序号顺序，投递顺序也就是序号顺序。
//! - 有存储时事件先落盘（列表 `job/{id}/hook`），爬取不等投递；投递任务从盘上按顺序读，事件有了
//!   结果（送达、重试用尽或地址被拦）先推进已确认游标（KV `job/{id}/hook_ack`）再弹出。
//!   最后发出的序号记在 KV `job/{id}/hook_seq`，重启后接着编号；重启恢复任务时从游标之后续投，
//!   确认后、弹出前崩溃留下的事件按游标跳过，不重发。
//! - 没有存储时用有界内存队列，满了发送方等待（背压），爬取跟着慢下来，事件不丢。
//! - 存储读写出错时降级：这个任务之后的事件改走内存队列（同样有界、背压），投递任务把盘上读得出的
//!   事件发完后转去投内存队列，不停下等重启。出错时盘上读不出来的事件留在盘上，重启后续投。

use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};

use pagesift_store::Store;
use serde_json::{Value, json};
use tokio::sync::{Mutex, Notify, mpsc};

use super::{Event, WebhookConfig, Webhooks};
use crate::metrics::Metrics;
use crate::types::Document;

/// 待投递事件的落盘列表。
pub(crate) fn outbox_key(job: &str) -> String {
    format!("job/{job}/hook")
}

/// 最后发出的事件序号。
pub(crate) fn seq_key(job: &str) -> String {
    format!("job/{job}/hook_seq")
}

/// 已确认（投递有了结果）的最大序号。
pub(crate) fn ack_key(job: &str) -> String {
    format!("job/{job}/hook_ack")
}

fn read_u64(store: &Store, key: &str) -> pagesift_store::Result<u64> {
    Ok(store
        .tasks()
        .kv_get(key)?
        .and_then(|v| std::str::from_utf8(&v).ok()?.parse().ok())
        .unwrap_or(0))
}

/// 落盘队列和投递任务之间的共享状态。
#[derive(Default)]
struct Bell {
    /// 有新事件、句柄全部释放了、或者降级了，叫醒投递任务。
    notify: Notify,
    closed: AtomicBool,
    /// 存储出过错：之后的事件走内存队列。
    degraded: AtomicBool,
}

impl Bell {
    fn degrade(&self) {
        self.degraded.store(true, Ordering::SeqCst);
        self.notify.notify_one();
    }

    fn degraded(&self) -> bool {
        self.degraded.load(Ordering::SeqCst)
    }
}

/// 待投递事件的去处。
enum Outbox {
    Memory(mpsc::Sender<Value>),
    /// 落盘队列；`spill` 是降级后用的内存队列。
    Stored {
        store: Store,
        bell: Arc<Bell>,
        spill: mpsc::Sender<Value>,
        mirror: Option<Arc<crate::jobs::mirror::JobMirror>>,
    },
}

struct Core {
    cfg: Arc<WebhookConfig>,
    kind: &'static str,
    job_id: Arc<str>,
    metrics: Arc<Metrics>,
    /// 下一个事件的序号；落盘队列在第一次发送时从存储读出。发送全程持有这把锁。
    next: Mutex<Option<u64>>,
    out: Outbox,
}

impl Drop for Core {
    fn drop(&mut self) {
        if let Outbox::Stored { bell, .. } = &self.out {
            bell.closed.store(true, Ordering::SeqCst);
            bell.notify.notify_one();
        }
    }
}

/// 一个任务的 webhook 句柄。全部句柄释放后，投递任务发完排队的事件就退出。
#[derive(Clone)]
pub(crate) struct Hook {
    core: Arc<Core>,
}

impl Webhooks {
    /// 为一个任务启动投递任务。`kind` 是事件类型前缀：`crawl` 或 `batch_scrape`。
    /// 给了存储就用落盘队列，并先把上次没投完的事件（从已确认游标之后）发出去。
    pub(crate) fn start(
        &self,
        job_id: &str,
        kind: &'static str,
        cfg: WebhookConfig,
        store: Option<Store>,
    ) -> Hook {
        let cfg = Arc::new(cfg);
        let job_id: Arc<str> = job_id.into();
        let (tx, rx) = mpsc::channel::<Value>(self.settings.queue.max(1));
        let (out, next) = match store {
            Some(store) => {
                let bell = Arc::<Bell>::default();
                let (this, store2, job, cfg, bell2) = (
                    self.clone(),
                    store.clone(),
                    job_id.clone(),
                    cfg.clone(),
                    bell.clone(),
                );
                tokio::spawn(async move {
                    this.drain_stored(&store2, &job, &cfg, &bell2).await;
                    // 落盘这条路走完了（句柄全部释放，或者存储出错降级）：接着投内存队列
                    this.drain_memory(rx, &cfg).await;
                });
                let out = Outbox::Stored {
                    store,
                    bell,
                    spill: tx,
                    mirror: self.mirror.clone(),
                };
                (out, None)
            }
            None => {
                let (this, cfg) = (self.clone(), cfg.clone());
                tokio::spawn(async move { this.drain_memory(rx, &cfg).await });
                (Outbox::Memory(tx), Some(1))
            }
        };
        Hook {
            core: Arc::new(Core {
                cfg,
                kind,
                job_id,
                metrics: self.metrics.clone(),
                next: Mutex::new(next),
                out,
            }),
        }
    }

    async fn drain_memory(&self, mut rx: mpsc::Receiver<Value>, cfg: &WebhookConfig) {
        while let Some(body) = rx.recv().await {
            self.deliver(cfg, &body).await;
        }
    }

    /// 按顺序投递盘上的事件，每个事件有了结果就推进游标、弹出。句柄全部释放且盘上发完、
    /// 或者降级（存储出错）且盘上已没有读得出的事件时返回。
    async fn drain_stored(&self, store: &Store, job: &str, cfg: &WebhookConfig, bell: &Bell) {
        let (list, ack) = (outbox_key(job), ack_key(job));
        let key = ack.clone();
        let mut acked = match store.blocking(move |s| read_u64(s, &key)).await {
            Ok(n) => n,
            Err(e) => {
                tracing::error!(job, error = %e, "reading the webhook cursor failed; switching to the memory queue");
                bell.degrade();
                return;
            }
        };
        // 句柄全部释放后再看一次盘：释放前的最后一个事件可能在上次看完之后才落盘
        let mut last_look = false;
        loop {
            let l = list.clone();
            let front = match store.blocking(move |s| s.tasks().list_front(&l)).await {
                Ok(f) => f,
                Err(e) => {
                    tracing::error!(job, error = %e, "reading the webhook queue failed; switching to the memory queue");
                    bell.degrade();
                    return;
                }
            };
            let Some(raw) = front else {
                if bell.degraded() || (bell.closed.load(Ordering::SeqCst) && last_look) {
                    return;
                }
                if bell.closed.load(Ordering::SeqCst) {
                    last_look = true;
                } else {
                    bell.notify.notified().await;
                }
                continue;
            };
            let body = serde_json::from_slice::<Value>(&raw).ok();
            let seq = body.as_ref().and_then(|b| b["seq"].as_u64());
            // 读不懂的，或者确认后、弹出前崩溃留下的（序号不大于游标）：直接弹出
            let fresh = match (&body, seq) {
                (Some(b), Some(seq)) if seq > acked => {
                    self.deliver(cfg, b).await;
                    acked = seq;
                    Some(seq)
                }
                _ => None,
            };
            let (l, a, m) = (list.clone(), ack.clone(), self.mirror.clone());
            let r = store
                .blocking(move |s| -> pagesift_store::Result<()> {
                    crate::jobs::mirror::write(s, m.as_deref(), |t| {
                        if let Some(seq) = fresh {
                            t.kv_put(&a, seq.to_string().as_bytes())?;
                        }
                        t.list_pop_front(&l)
                    })
                })
                .await;
            if let Err(e) = r {
                // 这个事件已经投过了；弹不掉的话重启后会再投一次（接收方按 eventId 去重）
                tracing::error!(job, error = %e, "advancing the webhook cursor failed; switching to the memory queue");
                bell.degrade();
                return;
            }
        }
    }
}

impl Hook {
    /// 发一个事件。走内存队列时队列满了等着（背压）；落盘时写完就返回。
    pub(crate) async fn send(&self, event: Event, data: Vec<Document>, error: Option<String>) {
        let c = &*self.core;
        if !c.cfg.wants(event) {
            return;
        }
        let mut next = c.next.lock().await;
        let seq = match *next {
            Some(n) => n,
            None => c.stored_next().await,
        };
        let mut body = json!({
            "success": error.is_none(),
            "type": format!("{}.{}", c.kind, event.name()),
            "id": &*c.job_id,
            "eventId": format!("{}:{seq}", c.job_id),
            "seq": seq,
            "data": data,
            "metadata": c.cfg.metadata,
        });
        if let Some(e) = error {
            body["error"] = e.into();
        }
        let memory = match &c.out {
            Outbox::Memory(tx) => Some((tx, body)),
            Outbox::Stored {
                store,
                bell,
                spill,
                mirror,
            } => c
                .persist(store, mirror.clone(), bell, seq, &body)
                .await
                .then_some((spill, body)),
        };
        // 只有运行时关闭、投递任务没了才会失败
        if let Some((tx, body)) = memory
            && tx.send(body).await.is_err()
        {
            c.metrics.webhook("dropped");
        }
        *next = Some(seq + 1);
    }
}

impl Core {
    /// 把事件写进落盘队列。返回 true 表示还要交给内存队列：已经降级、这次写失败，
    /// 或者写进去时投递任务刚好降级了、不会再读盘（重启后可能再投一次，接收方按 eventId 去重）。
    async fn persist(
        &self,
        store: &Store,
        mirror: Option<Arc<crate::jobs::mirror::JobMirror>>,
        bell: &Bell,
        seq: u64,
        body: &Value,
    ) -> bool {
        if bell.degraded() {
            return true;
        }
        let (seq_k, list) = (seq_key(&self.job_id), outbox_key(&self.job_id));
        let raw = body.to_string().into_bytes();
        // 先记序号再入队：两步之间崩溃只会空出一个序号，不会重复
        let m = mirror;
        let r = store
            .blocking(move |s| -> pagesift_store::Result<()> {
                crate::jobs::mirror::write(s, m.as_deref(), |t| {
                    t.kv_put(&seq_k, seq.to_string().as_bytes())?;
                    t.list_push(&list, &raw)
                })
            })
            .await;
        match r {
            Ok(()) => {
                bell.notify.notify_one();
                bell.degraded()
            }
            Err(e) => {
                tracing::error!(job = %self.job_id, seq, error = %e, "queueing a webhook event on disk failed; switching to the memory queue");
                bell.degrade();
                true
            }
        }
    }

    /// 落盘队列的下一个序号：上次发出的序号加一（新任务从 1 开始）。
    async fn stored_next(&self) -> u64 {
        let Outbox::Stored { store, .. } = &self.out else {
            return 1;
        };
        let key = seq_key(&self.job_id);
        match store.blocking(move |s| read_u64(s, &key)).await {
            Ok(n) => n + 1,
            Err(e) => {
                // 读不出来就从已确认游标之后编号，至少不和已经投递过的事件重号
                let key = ack_key(&self.job_id);
                let acked = store.blocking(move |s| read_u64(s, &key)).await;
                tracing::warn!(job = %self.job_id, error = %e, "reading the webhook sequence failed");
                acked.unwrap_or(0) + 1
            }
        }
    }
}
