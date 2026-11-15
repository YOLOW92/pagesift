//! P0-01 crash atomicity, short-write, corruption, compaction, and compatibility tests for `AtomicTx`.

use std::fs::OpenOptions;
use std::io::Write;
use std::path::PathBuf;

use crate::engine::ns;
use crate::record::{self, MAX_KEY, Op, SEG_MAGIC};
use crate::{Config, Error, Store, SyncPolicy};

struct TempDir(PathBuf);
impl TempDir {
    fn new(tag: &str) -> Self {
        let dir = std::env::temp_dir().join(format!(
            "pagesift-tx-crash-{tag}-{}-{}",
            std::process::id(),
            crate::now_store_ms()
        ));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        Self(dir)
    }
}
impl Drop for TempDir {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.0);
    }
}

fn cfg(dir: &std::path::Path, seg_bytes: u64) -> Config {
    let mut c = Config::new(dir);
    c.max_segment_bytes = seg_bytes;
    c.background = false;
    c.sync = SyncPolicy::Always;
    c
}

#[test]
fn atomic_tx_recovers_all_or_nothing_across_every_partial_write_boundary() {
    // First compute exact batch size of our 3-op AtomicTx by measuring segment growth once.
    let probe_dir = TempDir::new("probe");
    let total_batch_bytes = {
        let s = Store::open(cfg(&probe_dir.0, 1 << 20)).unwrap();
        let mut tx = s.atomic_tx();
        tx.put(ns::KV, b"k1", b"base-1", 100).unwrap();
        tx.put(ns::KV, b"k2", b"base-2", 100).unwrap();
        tx.commit().unwrap();
        drop(s);
        let seg_before = std::fs::metadata(probe_dir.0.join("00000001.seg"))
            .unwrap()
            .len();

        let s = Store::open(cfg(&probe_dir.0, 1 << 20)).unwrap();
        let mut tx = s.atomic_tx();
        tx.put(ns::KV, b"k1", b"new-val-1111", 200).unwrap();
        tx.delete(ns::KV, b"k2").unwrap();
        tx.put(
            ns::LIST_ITEM,
            b"\x02\x00g1\0\0\0\0\0\0\0\x00",
            b"item0",
            200,
        )
        .unwrap();
        tx.commit().unwrap();
        drop(s);
        let seg_after = std::fs::metadata(probe_dir.0.join("00000001.seg"))
            .unwrap()
            .len();
        (seg_after - seg_before) as usize
    };

    // Sweep partial writes at byte offsets across records, pre-TxCommit boundary, and mid-TxCommit.
    let mut cuts: Vec<usize> = (0..=40).collect();
    for offset in [
        total_batch_bytes.saturating_sub(80),
        total_batch_bytes.saturating_sub(68),
        total_batch_bytes.saturating_sub(64),
        total_batch_bytes.saturating_sub(50),
        total_batch_bytes.saturating_sub(32),
        total_batch_bytes.saturating_sub(16),
        total_batch_bytes.saturating_sub(8),
        total_batch_bytes.saturating_sub(1),
    ] {
        cuts.push(offset);
    }
    // Also test exact boundary right after op1/op2 (around half batch) and every single byte around TxCommit!
    for cut in total_batch_bytes / 3..total_batch_bytes {
        cuts.push(cut);
    }
    cuts.sort_unstable();
    cuts.dedup();

    for cut in cuts {
        let dir = TempDir::new(&format!("cut-{cut}"));
        {
            let s = Store::open(cfg(&dir.0, 1 << 20)).unwrap();
            let mut tx = s.atomic_tx();
            tx.put(ns::KV, b"k1", b"base-1", 100).unwrap();
            tx.put(ns::KV, b"k2", b"base-2", 100).unwrap();
            tx.commit().unwrap();

            s.set_crash_after_tx_bytes(Some(cut));
            let mut tx2 = s.atomic_tx();
            tx2.put(ns::KV, b"k1", b"new-val-1111", 200).unwrap();
            tx2.delete(ns::KV, b"k2").unwrap();
            tx2.put(
                ns::LIST_ITEM,
                b"\x02\x00g1\0\0\0\0\0\0\0\x00",
                b"item0",
                200,
            )
            .unwrap();
            let err = tx2.commit().unwrap_err();
            assert!(matches!(err, Error::Io(_)), "expected injected crash err");
        }

        let reopened = Store::open(cfg(&dir.0, 1 << 20)).unwrap();
        assert_eq!(
            reopened.stats().open.truncated_bytes,
            cut as u64,
            "cut={cut}"
        );
        assert_eq!(
            reopened.get(ns::KV, b"k1").unwrap().unwrap().1,
            b"base-1",
            "cut={cut} must not partially overwrite k1"
        );
        assert_eq!(
            reopened.get(ns::KV, b"k2").unwrap().unwrap().1,
            b"base-2",
            "cut={cut} must not partially delete k2"
        );
        assert!(
            reopened
                .get(ns::LIST_ITEM, b"\x02\x00g1\0\0\0\0\0\0\0\x00")
                .unwrap()
                .is_none(),
            "cut={cut} must not partially insert list item"
        );

        // Subsequent clean AtomicTx commits cleanly onto truncated segment and survives reopen.
        let mut tx3 = reopened.atomic_tx();
        tx3.put(ns::KV, b"k3", b"clean-after-recovery", 300)
            .unwrap();
        tx3.commit().unwrap();
        drop(reopened);

        let reopened2 = Store::open(cfg(&dir.0, 1 << 20)).unwrap();
        assert_eq!(reopened2.stats().open.truncated_bytes, 0);
        assert_eq!(
            reopened2.get(ns::KV, b"k3").unwrap().unwrap().1,
            b"clean-after-recovery"
        );
    }
}

#[test]
fn bitflip_corruption_in_tx_op_or_commit_rolls_back_entire_tx() {
    let dir = TempDir::new("bitflip");
    let before_len;
    {
        let s = Store::open(cfg(&dir.0, 1 << 20)).unwrap();
        let mut tx = s.atomic_tx();
        tx.put(ns::KV, b"base", b"ok", 10).unwrap();
        tx.commit().unwrap();
        before_len = std::fs::metadata(dir.0.join("00000001.seg")).unwrap().len();

        let mut tx2 = s.atomic_tx();
        tx2.put(ns::KV, b"a", b"val-a", 20).unwrap();
        tx2.put(ns::KV, b"b", b"val-b", 20).unwrap();
        tx2.commit().unwrap();
    }

    // Corrupt a byte inside the TxCommit trailer (near EOF): whole tx2 must roll back atomically!
    let seg_path = dir.0.join("00000001.seg");
    let mut data = std::fs::read(&seg_path).unwrap();
    let last = data.len() - 5;
    data[last] ^= 0xFF;
    std::fs::write(&seg_path, &data).unwrap();

    let s2 = Store::open(cfg(&dir.0, 1 << 20)).unwrap();
    assert!(s2.get(ns::KV, b"a").unwrap().is_none());
    assert!(s2.get(ns::KV, b"b").unwrap().is_none());
    assert_eq!(s2.get(ns::KV, b"base").unwrap().unwrap().1, b"ok");
    drop(s2);
    assert_eq!(std::fs::metadata(&seg_path).unwrap().len(), before_len);
}

#[test]
fn sync_failure_and_enospc_roll_back_file_and_memory_state() {
    let dir = TempDir::new("sync-fail");
    let s = Store::open(cfg(&dir.0, 1 << 20)).unwrap();
    let mut tx = s.atomic_tx();
    tx.put(ns::KV, b"k", b"v1", 10).unwrap();
    tx.commit().unwrap();
    let pre_len = std::fs::metadata(dir.0.join("00000001.seg")).unwrap().len();

    s.set_fail_sync(true);
    let mut tx2 = s.atomic_tx();
    tx2.put(ns::KV, b"k", b"v2", 20).unwrap();
    tx2.put(ns::KV, b"new", b"v3", 20).unwrap();
    assert!(tx2.commit().is_err());
    s.set_fail_sync(false);

    // Both in-memory state and physical file length must be rolled back immediately.
    assert_eq!(s.get(ns::KV, b"k").unwrap().unwrap().1, b"v1");
    assert!(s.get(ns::KV, b"new").unwrap().is_none());
    assert_eq!(
        std::fs::metadata(dir.0.join("00000001.seg")).unwrap().len(),
        pre_len
    );

    s.fail_writes(true);
    let mut tx3 = s.atomic_tx();
    tx3.put(ns::KV, b"k", b"v3", 30).unwrap();
    assert!(tx3.commit().is_err());
    s.fail_writes(false);
    assert_eq!(s.get(ns::KV, b"k").unwrap().unwrap().1, b"v1");

    // Reopen check.
    drop(s);
    let s2 = Store::open(cfg(&dir.0, 1 << 20)).unwrap();
    assert_eq!(s2.get(ns::KV, b"k").unwrap().unwrap().1, b"v1");
}

#[test]
fn legacy_v151_records_remain_readable_and_compact_safely_with_atomic_tx() {
    let dir = TempDir::new("compat");
    {
        let seg1 = dir.0.join("00000001.seg");
        let mut f = OpenOptions::new()
            .create(true)
            .truncate(true)
            .write(true)
            .open(&seg1)
            .unwrap();
        f.write_all(SEG_MAGIC).unwrap();
        f.write_all(&record::encode(ns::KV, Op::Put, 1, b"legacy1", b"one"))
            .unwrap();
        f.write_all(&record::encode(ns::KV, Op::Put, 2, b"legacy2", b"two"))
            .unwrap();
        f.write_all(&record::encode(ns::KV, Op::Del, 3, b"legacy2", b""))
            .unwrap();
        f.sync_all().unwrap();
    }

    let s = Store::open(cfg(&dir.0, 320)).unwrap();
    assert_eq!(s.get(ns::KV, b"legacy1").unwrap().unwrap().1, b"one");
    assert!(s.get(ns::KV, b"legacy2").unwrap().is_none());

    // Multi-op AtomicTx + roll across segments + compaction of sealed segment with TxPut/TxDel/TxCommit.
    let mut tx1 = s.atomic_tx();
    tx1.put(ns::KV, b"tx_live", b"live_payload_val", 10)
        .unwrap();
    tx1.put(ns::KV, b"tx_dead", b"will_be_overwritten", 10)
        .unwrap();
    tx1.commit().unwrap();

    let mut tx2 = s.atomic_tx();
    tx2.delete(ns::KV, b"tx_dead").unwrap();
    tx2.put(ns::KV, b"tx_later", &[b'x'; 220], 20).unwrap();
    tx2.commit().unwrap();

    let compacted = s.compact().unwrap();
    assert!(compacted >= 1, "expected sealed segments compacted");
    assert_eq!(s.get(ns::KV, b"legacy1").unwrap().unwrap().1, b"one");
    assert_eq!(
        s.get(ns::KV, b"tx_live").unwrap().unwrap().1,
        b"live_payload_val"
    );
    assert!(s.get(ns::KV, b"tx_dead").unwrap().is_none());
    drop(s);

    let reopened = Store::open(cfg(&dir.0, 320)).unwrap();
    assert_eq!(reopened.get(ns::KV, b"legacy1").unwrap().unwrap().1, b"one");
    assert_eq!(
        reopened.get(ns::KV, b"tx_live").unwrap().unwrap().1,
        b"live_payload_val"
    );
    assert!(reopened.get(ns::KV, b"tx_dead").unwrap().is_none());

    // Delete oversized key rejected cleanly.
    let mut tx3 = reopened.atomic_tx();
    let too_big = vec![b'k'; MAX_KEY + 1];
    assert!(matches!(
        tx3.delete(ns::KV, &too_big),
        Err(Error::KeyTooLarge(_))
    ));
}
