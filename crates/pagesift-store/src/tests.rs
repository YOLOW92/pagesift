//! 需要访问内部配置（哈希截短制造冲突）的测试。

use std::collections::{HashMap, HashSet, VecDeque};

use proptest::prelude::*;

use crate::{Config, Store, SyncPolicy};

fn config(dir: &std::path::Path, mask: u64) -> Config {
    let mut c = Config::new(dir);
    c.max_segment_bytes = 2048;
    c.background = false;
    c.sync = SyncPolicy::Never;
    c.hash_mask = mask;
    c
}

#[derive(Debug, Clone)]
enum Op {
    Put(u8, Vec<u8>),
    Delete(u8),
    SetInsert(u8),
    SetRemove(u8),
    Push(u8),
    Pop,
    Reopen,
    Compact,
    CompactOne(u8),
}

fn op() -> impl Strategy<Value = Op> {
    prop_oneof![
        6 => (0u8..24, prop::collection::vec(any::<u8>(), 0..200)).prop_map(|(k, v)| Op::Put(k, v)),
        3 => (0u8..24).prop_map(Op::Delete),
        2 => (0u8..16).prop_map(Op::SetInsert),
        1 => (0u8..16).prop_map(Op::SetRemove),
        2 => any::<u8>().prop_map(Op::Push),
        2 => Just(Op::Pop),
        1 => Just(Op::Reopen),
        1 => Just(Op::Compact),
        2 => any::<u8>().prop_map(Op::CompactOne),
    ]
}

fn key(k: u8) -> Vec<u8> {
    format!("task/{k}").into_bytes()
}

fn run(ops: Vec<Op>, mask: u64) -> Result<(), TestCaseError> {
    let dir = tempfile::tempdir().unwrap();
    let mut store = Store::open(config(dir.path(), mask)).unwrap();
    let mut kv: HashMap<u8, Vec<u8>> = HashMap::new();
    let mut set: HashSet<u8> = HashSet::new();
    let mut queue: VecDeque<u8> = VecDeque::new();
    for op in ops {
        let t = store.tasks();
        match op {
            Op::Put(k, v) => {
                t.kv_put(key(k), &v).unwrap();
                kv.insert(k, v);
            }
            Op::Delete(k) => {
                prop_assert_eq!(t.kv_delete(key(k)).unwrap(), kv.remove(&k).is_some());
            }
            Op::SetInsert(m) => prop_assert_eq!(t.set_insert("done", [m]).unwrap(), set.insert(m)),
            Op::SetRemove(m) => prop_assert_eq!(t.set_remove("done", [m]).unwrap(), set.remove(&m)),
            Op::Push(x) => {
                t.list_push("queue", &[x]).unwrap();
                queue.push_back(x);
            }
            Op::Pop => {
                let got = t.list_pop_front("queue").unwrap();
                prop_assert_eq!(got, queue.pop_front().map(|x| vec![x]));
            }
            Op::Reopen => {
                drop(store);
                store = Store::open(config(dir.path(), mask)).unwrap();
            }
            Op::Compact => {
                store.compact().unwrap();
            }
            Op::CompactOne(i) => {
                let ids = store.sealed_segments();
                if !ids.is_empty() {
                    store.compact_segment(ids[i as usize % ids.len()]).unwrap();
                }
            }
        }
        let t = store.tasks();
        for k in 0..24 {
            prop_assert_eq!(t.kv_get(key(k)).unwrap(), kv.get(&k).cloned());
        }
        prop_assert_eq!(t.set_len("done"), set.len() as u64);
        prop_assert_eq!(t.list_len("queue"), queue.len() as u64);
    }
    // 最后再重开一次，比对全部内容。
    drop(store);
    let store = Store::open(config(dir.path(), mask)).unwrap();
    let t = store.tasks();
    let mut keys: Vec<Vec<u8>> = kv.keys().map(|k| key(*k)).collect();
    keys.sort();
    prop_assert_eq!(t.kv_keys("task/").unwrap(), keys);
    let mut members: Vec<Vec<u8>> = set.iter().map(|m| vec![*m]).collect();
    members.sort();
    prop_assert_eq!(t.set_members("done").unwrap(), members);
    let items: Vec<Vec<u8>> = queue.iter().map(|x| vec![*x]).collect();
    prop_assert_eq!(t.list_items("queue", usize::MAX).unwrap(), items);
    Ok(())
}

proptest! {
    #![proptest_config(ProptestConfig::with_cases(64))]

    #[test]
    fn matches_model(ops in prop::collection::vec(op(), 1..120)) {
        run(ops, u64::MAX)?;
    }

    /// 哈希只留 3 位，几乎每个键都冲突，走 overflow 路径。
    #[test]
    fn matches_model_with_collisions(ops in prop::collection::vec(op(), 1..120)) {
        run(ops, 0b111)?;
    }
}

#[test]
fn collisions_survive_compaction_and_reopen() {
    let dir = tempfile::tempdir().unwrap();
    let store = Store::open(config(dir.path(), 0)).unwrap();
    let t = store.tasks();
    for i in 0..50u32 {
        t.kv_put(i.to_le_bytes(), &[i as u8; 100]).unwrap();
    }
    for i in (0..50u32).step_by(2) {
        assert!(t.kv_delete(i.to_le_bytes()).unwrap());
    }
    store.compact().unwrap();
    drop(store);
    let store = Store::open(config(dir.path(), 0)).unwrap();
    for i in 0..50u32 {
        let want = (i % 2 == 1).then(|| vec![i as u8; 100]);
        assert_eq!(store.tasks().kv_get(i.to_le_bytes()).unwrap(), want);
    }
    assert_eq!(store.stats().entries, 25);
}

#[test]
fn list_range_pages_by_sequence() {
    let dir = tempfile::tempdir().unwrap();
    let store = Store::open(config(dir.path(), u64::MAX)).unwrap();
    let t = store.tasks();
    for i in 0..10u8 {
        t.list_push("pages", &[i]).unwrap();
    }
    assert_eq!(
        t.list_range("pages", 3, 2).unwrap(),
        vec![vec![3u8], vec![4u8]]
    );
    assert_eq!(t.list_range("pages", 9, 5).unwrap(), vec![vec![9u8]]);
    assert!(t.list_range("pages", 10, 5).unwrap().is_empty());
    t.list_pop_front("pages").unwrap();
    assert_eq!(t.list_range("pages", 0, 1).unwrap(), vec![vec![1u8]]);
    assert!(t.list_range("missing", 0, 1).unwrap().is_empty());
}

#[test]
fn item_identity_versions_survive_compaction_and_are_not_cache_entries() {
    let dir = tempfile::tempdir().unwrap();
    // Force index collisions and segment rotation; identity must use full keys.
    let mut cfg = config(dir.path(), 0);
    cfg.max_bytes = Some(1);
    {
        let store = Store::open(cfg.clone()).unwrap();
        let items = store.items();
        for i in 0..40 {
            items
                .commit(b"catalog", format!("id-{i}").as_bytes(), b"v1")
                .unwrap();
        }
        // Obsolete alternating records so compaction must copy live neighbors.
        for i in (0..40).step_by(2) {
            items
                .commit(b"catalog", format!("id-{i}").as_bytes(), b"v1")
                .unwrap();
        }
        items.commit(b"a:b", b"c", b"one").unwrap();
        items.commit(b"a", b"b:c", b"two").unwrap();
        items
            .commit("日本語".as_bytes(), "同名".as_bytes(), b"")
            .unwrap();
        assert!(
            store.compact().unwrap() > 0,
            "must actually compact a segment"
        );
        store.gc().unwrap();
    }
    let store = Store::open(cfg.clone()).unwrap();
    let items = store.items();
    for i in 0..40 {
        assert!(
            items
                .matches(b"catalog", format!("id-{i}").as_bytes(), b"v1")
                .unwrap()
        );
    }
    assert!(items.matches(b"a:b", b"c", b"one").unwrap());
    assert!(items.matches(b"a", b"b:c", b"two").unwrap());
    assert!(
        items
            .matches("日本語".as_bytes(), "同名".as_bytes(), b"")
            .unwrap()
    );
    assert!(!items.matches(b"catalog", b"id-0", b"v2").unwrap());
    items.commit(b"catalog", b"id-0", b"v2").unwrap();
    assert!(!items.matches(b"catalog", b"id-0", b"v1").unwrap());
    assert!(items.matches(b"catalog", b"id-0", b"v2").unwrap());
    assert!(!items.matches(b"other", b"id-0", b"v2").unwrap());
    assert!(items.forget(b"catalog", b"id-1").unwrap());
    drop(store);
    let store = Store::open(cfg).unwrap();
    assert!(!store.items().matches(b"catalog", b"id-1", b"v1").unwrap());
    assert!(store.items().matches(b"catalog", b"id-0", b"v2").unwrap());
}
