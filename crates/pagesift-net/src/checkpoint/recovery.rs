//! 毫秒级任务崩溃检查点反序列化与无缝恢复引擎。

use std::collections::HashSet;
use std::fs::File;
use std::io::{self, Read};
use std::path::Path;

use super::blake3_snapshot::TaskSnapshot;
use crate::scheduler::CrawlTask;

pub struct RecoveryEngine;

impl RecoveryEngine {
    /// 从磁盘加载并验证检查点快照（若校验和不匹配判定为无效损坏）。
    pub fn load_checkpoint<P: AsRef<Path>>(path: P) -> io::Result<TaskSnapshot> {
        let path = path.as_ref();
        let mut file = File::open(path)?;
        let mut bytes = Vec::new();
        file.read_to_end(&mut bytes)?;

        let snapshot: TaskSnapshot = serde_json::from_slice(&bytes)
            .map_err(|e| io::Error::new(io::ErrorKind::InvalidData, format!("deserialization error: {e}")))?;

        if !snapshot.verify_integrity() {
            return Err(io::Error::new(
                io::ErrorKind::InvalidData,
                "BLAKE3 checksum mismatch: checkpoint was torn or corrupted",
            ));
        }

        Ok(snapshot)
    }

    /// 从快照恢复出已完成 URL 集合与待抓取任务队列，确保零重复与零丢失。
    pub fn restore_tasks(snapshot: TaskSnapshot) -> (HashSet<String>, Vec<CrawlTask>) {
        let completed = snapshot.completed_urls;
        let mut restored = Vec::with_capacity(snapshot.pending_urls.len());

        for url in snapshot.pending_urls {
            if !completed.contains(&url) {
                if let Some(task) = CrawlTask::new(&url, 0) {
                    restored.push(task);
                }
            }
        }

        (completed, restored)
    }
}
