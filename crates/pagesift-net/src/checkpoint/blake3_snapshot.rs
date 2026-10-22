//! 基于 BLAKE3 增量校验与原子落盘的任务快照生成器。

use std::collections::{HashMap, HashSet};
use std::fs::File;
use std::io::{self, Write};
use std::path::Path;
use serde::{Deserialize, Serialize};

/// 检查点快照元数据与状态结构
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct TaskSnapshot {
    pub task_id: String,
    pub timestamp_secs: u64,
    pub completed_urls: HashSet<String>,
    pub pending_urls: Vec<String>,
    pub domain_counts: HashMap<String, usize>,
    /// BLAKE3 校验哈希（不包含自身字段），确保断电/崩溃时不加载半写入的撕裂文件
    pub blake3_hex: String,
}

impl TaskSnapshot {
    pub fn new(
        task_id: &str,
        completed_urls: HashSet<String>,
        pending_urls: Vec<String>,
        domain_counts: HashMap<String, usize>,
    ) -> Self {
        let timestamp_secs = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap_or_default()
            .as_secs();

        let mut snapshot = Self {
            task_id: task_id.to_string(),
            timestamp_secs,
            completed_urls,
            pending_urls,
            domain_counts,
            blake3_hex: String::new(),
        };

        let digest = snapshot.compute_checksum();
        snapshot.blake3_hex = digest;
        snapshot
    }

    /// 计算当前快照内容的 BLAKE3 校验和。
    pub fn compute_checksum(&self) -> String {
        let mut hasher = blake3::Hasher::new();
        hasher.update(self.task_id.as_bytes());
        hasher.update(&self.timestamp_secs.to_le_bytes());

        // 有序计算以保证确定性哈希
        let mut sorted_completed: Vec<_> = self.completed_urls.iter().collect();
        sorted_completed.sort();
        for u in sorted_completed {
            hasher.update(u.as_bytes());
            hasher.update(b"\n");
        }

        for u in &self.pending_urls {
            hasher.update(u.as_bytes());
            hasher.update(b"\n");
        }

        let mut sorted_domains: Vec<_> = self.domain_counts.iter().collect();
        sorted_domains.sort();
        for (d, cnt) in sorted_domains {
            hasher.update(d.as_bytes());
            hasher.update(&cnt.to_le_bytes());
        }

        hasher.finalize().to_hex().to_string()
    }

    /// 校验当前快照数据是否完整未被篡改/撕裂。
    pub fn verify_integrity(&self) -> bool {
        let expected = self.compute_checksum();
        self.blake3_hex == expected
    }

    /// 原子安全落盘：先写入临时文件，再做原子重命名，防止进程在写入中途被 kill 破坏快照。
    pub fn save_atomic<P: AsRef<Path>>(&self, path: P) -> io::Result<()> {
        let path = path.as_ref();
        let tmp_path = path.with_extension("tmp");

        let json_bytes = serde_json::to_vec(self)
            .map_err(|e| io::Error::new(io::ErrorKind::InvalidData, e))?;

        {
            let mut file = File::create(&tmp_path)?;
            file.write_all(&json_bytes)?;
            file.sync_all()?;
        }

        std::fs::rename(&tmp_path, path)?;
        Ok(())
    }
}
