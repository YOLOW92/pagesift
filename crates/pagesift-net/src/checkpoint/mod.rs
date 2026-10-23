//! 原子检查点与崩溃恢复系统。

pub mod blake3_snapshot;
pub mod recovery;

pub use blake3_snapshot::TaskSnapshot;
pub use recovery::RecoveryEngine;

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::{HashMap, HashSet};

    #[test]
    fn test_snapshot_integrity_and_tamper_detection() {
        let mut completed = HashSet::new();
        completed.insert("https://example.com/p1".into());
        completed.insert("https://example.com/p2".into());

        let pending = vec![
            "https://example.com/p3".into(),
            "https://example.com/p4".into(),
        ];
        let mut domain_counts = HashMap::new();
        domain_counts.insert("example.com".into(), 2);

        let mut snapshot = TaskSnapshot::new("crawl_job_1", completed, pending, domain_counts);
        assert!(snapshot.verify_integrity());

        // 模拟篡改已完成列表
        snapshot
            .completed_urls
            .insert("https://example.com/fake".into());
        assert!(!snapshot.verify_integrity());
    }

    #[test]
    fn test_atomic_save_and_recovery() {
        let dir = std::env::temp_dir();
        let path = dir.join("pagesift_checkpoint_test.json");

        let mut completed = HashSet::new();
        completed.insert("https://a.com/1".into());
        let pending = vec!["https://a.com/2".into(), "https://b.com/1".into()];
        let mut domains = HashMap::new();
        domains.insert("a.com".into(), 1);

        let snap = TaskSnapshot::new("job_test", completed, pending, domains);
        snap.save_atomic(&path).expect("save atomic");

        let loaded = RecoveryEngine::load_checkpoint(&path).expect("load checkpoint");
        assert_eq!(loaded.task_id, "job_test");
        assert!(loaded.verify_integrity());

        let (done, to_crawl) = RecoveryEngine::restore_tasks(loaded);
        assert_eq!(done.len(), 1);
        assert_eq!(to_crawl.len(), 2);
        assert_eq!(to_crawl[0].url, "https://a.com/2");
        assert_eq!(to_crawl[1].url, "https://b.com/1");

        let _ = std::fs::remove_file(path);
    }
}
