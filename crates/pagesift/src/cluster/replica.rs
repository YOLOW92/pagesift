//! 任务记录随账本复制到温备：API 任务存储的写入（`pagesift_api::JobMirror`）作为账本的 `App` 记录
//! 追加进日志，经已有的日志传送到温备，温备写进自己的存储；账本快照时连同全部任务记录一起压缩。
//! 温备接管后用这份存储建 HTTP 服务，任务照常恢复：结束的能查，进行中的从断点继续。

use std::sync::Arc;

use pagesift_api::{JobLog, JobMirror, apply_op};
use pagesift_cluster::{AppState, Ledger};
use pagesift_store::Store;

struct LedgerLog(Arc<Ledger>);

impl JobLog for LedgerLog {
    fn append(&self, op: Vec<u8>) {
        if let Err(e) = self.0.append_app(op) {
            tracing::warn!("replicating a job record failed: {e}");
        }
    }
}

struct JobRecords {
    mirror: Arc<JobMirror>,
    store: Store,
}

impl AppState for JobRecords {
    fn apply(&self, op: &[u8]) {
        if let Err(e) = apply_op(&self.store, op) {
            tracing::warn!("applying a replicated job record failed: {e}");
        }
    }

    fn dump(&self) -> Vec<Vec<u8>> {
        self.mirror.dump(&self.store)
    }
}

/// 把 `store` 里的任务记录接到 `ledger` 上复制；返回交给 HTTP 服务（`api::Config::job_mirror`）的镜像。
/// 网关和温备都调用：网关一侧写入经它进日志，温备一侧回放的记录经它写进存储。
pub fn replicate_jobs(ledger: &Arc<Ledger>, store: &Store) -> Arc<JobMirror> {
    let mirror = JobMirror::new(Arc::new(LedgerLog(ledger.clone())));
    ledger.set_app(Arc::new(JobRecords {
        mirror: mirror.clone(),
        store: store.clone(),
    }));
    mirror
}
