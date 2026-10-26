use std::fs::{self, OpenOptions};
use std::io::Write;
use std::path::{Path, PathBuf};
use std::time::{Duration, SystemTime};

use pagesift_store::{CacheLookup, CachePolicy, Config, Error, Store, SyncPolicy};

fn config(dir: &Path) -> Config {
    let mut c = Config::new(dir);
    c.max_segment_bytes = 4096;
    c.background = false;
    c.sync = SyncPolicy::Never;
    c
}

fn segments(dir: &Path) -> Vec<PathBuf> {
    let mut v: Vec<PathBuf> = fs::read_dir(dir)
        .unwrap()
        .map(|e| e.unwrap().path())
        .filter(|p| p.extension().is_some_and(|e| e == "seg"))
        .collect();
    v.sort();
    v
}

#[test]
fn crash_recovery_truncates_torn_tail() {
    let dir = tempfile::tempdir().unwrap();
    let mut c = config(dir.path());
    c.max_segment_bytes = 1 << 20;
    {
        let store = Store::open(c.clone()).unwrap();
        for i in 0..10 {
            store.tasks().kv_put(format!("k{i}"), &[i; 50]).unwrap();
        }
    }
    let seg = segments(dir.path()).pop().unwrap();
    let full = fs::metadata(&seg).unwrap().len();
    // 最后一条记录只写了一半。
    OpenOptions::new()
        .write(true)
        .open(&seg)
        .unwrap()
        .set_len(full - 20)
        .unwrap();
    {
        let store = Store::open(c.clone()).unwrap();
        let rep = store.stats().open;
        assert_eq!(rep.records, 9);
        assert!(rep.truncated_bytes > 0);
        for i in 0..9 {
            assert_eq!(
                store.tasks().kv_get(format!("k{i}")).unwrap(),
                Some(vec![i; 50])
            );
        }
        assert_eq!(store.tasks().kv_get("k9").unwrap(), None);
        // 截断后可以继续写。
        store.tasks().kv_put("after", b"crash").unwrap();
    }
    // 尾部追加垃圾（例如断电后的随机数据）。
    OpenOptions::new()
        .append(true)
        .open(&seg)
        .unwrap()
        .write_all(&[0xAB; 37])
        .unwrap();
    let store = Store::open(c).unwrap();
    assert_eq!(store.stats().open.truncated_bytes, 37);
    assert_eq!(
        store.tasks().kv_get("after").unwrap(),
        Some(b"crash".to_vec())
    );
    assert_eq!(store.tasks().kv_get("k8").unwrap(), Some(vec![8; 50]));
}

#[test]
fn crash_recovery_flipped_bit_stops_at_bad_record() {
    let dir = tempfile::tempdir().unwrap();
    let mut c = config(dir.path());
    c.max_segment_bytes = 1 << 20;
    {
        let store = Store::open(c.clone()).unwrap();
        for i in 0..5 {
            store.tasks().kv_put(format!("k{i}"), &[i; 100]).unwrap();
        }
    }
    let seg = segments(dir.path()).pop().unwrap();
    let mut data = fs::read(&seg).unwrap();
    let n = data.len();
    data[n - 10] ^= 0xFF;
    fs::write(&seg, data).unwrap();
    let store = Store::open(c).unwrap();
    assert_eq!(store.stats().open.records, 4);
    assert_eq!(store.tasks().kv_get("k4").unwrap(), None);
    assert_eq!(store.tasks().kv_get("k3").unwrap(), Some(vec![3; 100]));
}

#[test]
fn torn_segment_header_is_repaired() {
    let dir = tempfile::tempdir().unwrap();
    {
        let store = Store::open(config(dir.path())).unwrap();
        store.tasks().kv_put("a", b"1").unwrap();
    }
    // 换段时崩溃：新段文件只写了 3 个字节。
    fs::write(dir.path().join("00000099.seg"), b"PSS").unwrap();
    let store = Store::open(config(dir.path())).unwrap();
    assert_eq!(store.tasks().kv_get("a").unwrap(), Some(b"1".to_vec()));
    store.tasks().kv_put("b", b"2").unwrap();
    drop(store);
    let store = Store::open(config(dir.path())).unwrap();
    assert_eq!(store.tasks().kv_get("b").unwrap(), Some(b"2".to_vec()));
}

#[test]
fn foreign_file_is_rejected() {
    let dir = tempfile::tempdir().unwrap();
    fs::write(dir.path().join("00000001.seg"), b"NOTASTORE-FILE").unwrap();
    assert!(matches!(
        Store::open(config(dir.path())),
        Err(Error::Corrupt(_))
    ));
}

#[test]
fn compaction_keeps_data_and_reclaims_space() {
    let dir = tempfile::tempdir().unwrap();
    let store = Store::open(config(dir.path())).unwrap();
    let t = store.tasks();
    for round in 0..5u8 {
        for i in 0..100u32 {
            t.kv_put(i.to_le_bytes(), &[round; 64]).unwrap();
        }
    }
    for i in 0..50u32 {
        t.kv_delete(i.to_le_bytes()).unwrap();
    }
    let before = store.stats();
    assert!(before.segments > 5);
    let n = store.compact().unwrap();
    assert!(n > 0);
    let after = store.stats();
    assert!(
        after.disk_bytes < before.disk_bytes / 4,
        "{before:?} -> {after:?}"
    );
    assert_eq!(after.live_bytes, before.live_bytes);
    let check = |s: &Store| {
        for i in 0..100u32 {
            let want = (i >= 50).then(|| vec![4u8; 64]);
            assert_eq!(s.tasks().kv_get(i.to_le_bytes()).unwrap(), want);
        }
    };
    check(&store);
    drop(store);
    let store = Store::open(config(dir.path())).unwrap();
    check(&store);
    assert_eq!(store.stats().entries, 50);
    // 再压缩一次没有东西可做。
    assert_eq!(store.compact().unwrap(), 0);
}

#[test]
fn deleted_keys_stay_deleted_after_partial_compaction() {
    let dir = tempfile::tempdir().unwrap();
    let mut c = config(dir.path());
    c.max_segment_bytes = 1024;
    let store = Store::open(c.clone()).unwrap();
    let t = store.tasks();
    // 旧段里是存活数据和将被删除的旧值，删除标记在较新的段里。
    t.kv_put("victim", &[1; 300]).unwrap();
    for i in 0..10u8 {
        t.kv_put([b'x', i], &[i; 300]).unwrap();
    }
    t.kv_delete("victim").unwrap();
    for i in 0..10u8 {
        t.kv_put([b'x', i], &[i; 300]).unwrap();
    }
    store.compact().unwrap();
    drop(store);
    let store = Store::open(c).unwrap();
    assert_eq!(store.tasks().kv_get("victim").unwrap(), None);
    assert_eq!(store.tasks().kv_get([b'x', 3]).unwrap(), Some(vec![3; 300]));
}

#[test]
fn lru_eviction_respects_cap_and_spares_tasks() {
    let dir = tempfile::tempdir().unwrap();
    let mut c = config(dir.path());
    c.max_bytes = Some(40_000);
    let store = Store::open(c.clone()).unwrap();
    store.tasks().kv_put("crawl/1", &[7; 2000]).unwrap();
    let cache = store.cache();
    for i in 0..100 {
        cache
            .put(format!("https://a.test/{i}"), &[0; 1000])
            .unwrap();
    }
    // 访问时间是秒级，回填一个“很早以前”的状态：先让 0..10 显得最近被用过。
    std::thread::sleep(Duration::from_millis(1100));
    for i in 0..10 {
        assert!(
            cache
                .entry(format!("https://a.test/{i}"))
                .unwrap()
                .is_some()
        );
    }
    let rep = store.gc().unwrap();
    assert!(rep.evicted_entries > 0);
    let st = store.stats();
    assert!(st.disk_bytes <= 40_000, "{st:?}");
    assert!(st.live_bytes <= 36_000);
    for i in 0..10 {
        assert!(
            cache
                .entry(format!("https://a.test/{i}"))
                .unwrap()
                .is_some(),
            "recently used entry {i} evicted"
        );
    }
    assert!(cache.entry("https://a.test/10").unwrap().is_none());
    assert_eq!(
        store.tasks().kv_get("crawl/1").unwrap(),
        Some(vec![7; 2000])
    );
    drop(store);
    let store = Store::open(c).unwrap();
    assert!(store.cache().entry("https://a.test/10").unwrap().is_none());
    assert!(store.cache().entry("https://a.test/5").unwrap().is_some());
}

#[test]
fn background_maintenance_enforces_cap() {
    let dir = tempfile::tempdir().unwrap();
    let mut c = config(dir.path());
    c.background = true;
    c.max_bytes = Some(50_000);
    let store = Store::open(c).unwrap();
    for i in 0..200 {
        store.cache().put(format!("u{i}"), &[1; 1000]).unwrap();
    }
    let deadline = std::time::Instant::now() + Duration::from_secs(10);
    while store.stats().disk_bytes > 50_000 {
        assert!(std::time::Instant::now() < deadline, "{:?}", store.stats());
        std::thread::sleep(Duration::from_millis(50));
        // 后台任务有 1 秒的最小间隔，继续写入以便再次触发。
        store.cache().put("tick", b"x").unwrap();
    }
    assert!(store.stats().last_error.is_none());
}

#[test]
fn cache_max_age_and_min_age() {
    let dir = tempfile::tempdir().unwrap();
    let store = Store::open(config(dir.path())).unwrap();
    let cache = store.cache();
    let now = SystemTime::now();
    let hour = Duration::from_secs(3600);
    cache
        .put_at("fresh", b"F", now - Duration::from_secs(10))
        .unwrap();
    cache.put_at("old", b"O", now - 3 * 24 * hour).unwrap();

    assert_eq!(cache.get("fresh", hour).unwrap().unwrap().value, b"F");
    assert!(
        cache
            .get("fresh", Duration::from_secs(5))
            .unwrap()
            .is_none()
    );
    assert!(cache.get("fresh", Duration::ZERO).unwrap().is_none());
    assert!(cache.get("missing", hour).unwrap().is_none());

    let default = CachePolicy::default();
    assert!(matches!(
        cache.lookup("fresh", &default).unwrap(),
        CacheLookup::Hit(_)
    ));
    assert_eq!(cache.lookup("old", &default).unwrap(), CacheLookup::Miss);
    let long = CachePolicy::from_firecrawl(Some(7 * 24 * 3600 * 1000), None, None);
    assert!(matches!(
        cache.lookup("old", &long).unwrap(),
        CacheLookup::Hit(_)
    ));

    // minAge：只查缓存，年龄不够或没有缓存都是 NoCachedData。
    let min_any = CachePolicy::from_firecrawl(None, Some(1), None);
    assert!(matches!(
        cache.lookup("fresh", &min_any).unwrap(),
        CacheLookup::Hit(_)
    ));
    assert_eq!(
        cache.lookup("missing", &min_any).unwrap(),
        CacheLookup::NoCachedData
    );
    let min_day = CachePolicy::from_firecrawl(None, Some(24 * 3600 * 1000), None);
    assert_eq!(
        cache.lookup("fresh", &min_day).unwrap(),
        CacheLookup::NoCachedData
    );
    assert!(matches!(
        cache.lookup("old", &min_day).unwrap(),
        CacheLookup::Hit(_)
    ));

    // storeInCache = false 不写入。
    let no_store = CachePolicy::from_firecrawl(None, None, Some(false));
    assert!(!cache.store("private", b"P", &no_store).unwrap());
    assert!(cache.entry("private").unwrap().is_none());
    assert!(cache.store("public", b"P", &default).unwrap());
    assert!(cache.entry("public").unwrap().is_some());

    // 写入时间在重启后保留，压缩也不改变。
    store.compact().unwrap();
    drop(store);
    let store = Store::open(config(dir.path())).unwrap();
    let e = store.cache().entry("old").unwrap().unwrap();
    let age = e.age_at(SystemTime::now());
    assert!(age >= 3 * 24 * hour && age < 3 * 24 * hour + Duration::from_secs(60));
}

#[test]
fn snapshots_dedup_and_time_range() {
    let dir = tempfile::tempdir().unwrap();
    let store = Store::open(config(dir.path())).unwrap();
    let snaps = store.snapshots();
    let t0 = SystemTime::UNIX_EPOCH + Duration::from_secs(1_700_000_000);
    let day = Duration::from_secs(86_400);
    let page_a = "<html><body>".to_string() + &"hello ".repeat(2000) + "</body></html>";
    let page_b = page_a.replace("hello", "world");

    let p1 = snaps
        .put_at("https://a.test/", page_a.as_bytes(), t0)
        .unwrap();
    assert!(!p1.deduped);
    assert!(
        p1.stored_bytes < page_a.len() / 10,
        "zstd should compress repetitive html"
    );
    let p2 = snaps
        .put_at("https://a.test/", page_a.as_bytes(), t0 + day)
        .unwrap();
    assert!(p2.deduped);
    assert_eq!(p1.hash, p2.hash);
    // 别的 URL 同样内容也去重。
    assert!(
        snaps
            .put_at("https://b.test/", page_a.as_bytes(), t0)
            .unwrap()
            .deduped
    );
    let p3 = snaps
        .put_at("https://a.test/", page_b.as_bytes(), t0 + 2 * day)
        .unwrap();
    assert!(!p3.deduped);
    assert_eq!(store.stats().entries, 2 + 4, "two blobs, four list items");

    let all = snaps.list("https://a.test/", ..).unwrap();
    assert_eq!(all.len(), 3);
    assert_eq!(all[0].at, t0);
    assert_eq!(all[2].hash, p3.hash);
    assert!(
        all.iter()
            .all(|m| m.available && m.raw_len as usize == page_a.len())
    );
    let mid = snaps
        .list("https://a.test/", t0 + day..t0 + 2 * day)
        .unwrap();
    assert_eq!(mid.len(), 1);
    assert_eq!(mid[0].at, t0 + day);
    assert_eq!(
        snaps.latest("https://a.test/").unwrap().unwrap().hash,
        p3.hash
    );
    assert!(snaps.list("https://c.test/", ..).unwrap().is_empty());

    assert_eq!(snaps.get(&p1.hash).unwrap().unwrap(), page_a.as_bytes());
    assert_eq!(snaps.get(&p3.hash).unwrap().unwrap(), page_b.as_bytes());

    let too_big = vec![b'x'; 512 * 1024 + 1];
    assert!(matches!(
        snaps.put("https://a.test/", &too_big),
        Err(Error::ValueTooLarge { .. })
    ));

    drop(store);
    let store = Store::open(config(dir.path())).unwrap();
    assert_eq!(
        store.snapshots().list("https://a.test/", ..).unwrap().len(),
        3
    );
    assert_eq!(
        store.snapshots().get(&p3.hash).unwrap().unwrap(),
        page_b.as_bytes()
    );
}

#[test]
fn crawl_state_resumes_after_restart() {
    let dir = tempfile::tempdir().unwrap();
    {
        let store = Store::open(config(dir.path())).unwrap();
        let t = store.tasks();
        t.kv_put("crawl/42/meta", br#"{"limit":100}"#).unwrap();
        for i in 0..20 {
            t.list_push("crawl/42/queue", format!("https://a.test/{i}").as_bytes())
                .unwrap();
        }
        for _ in 0..5 {
            let url = t.list_pop_front("crawl/42/queue").unwrap().unwrap();
            assert!(t.set_insert("crawl/42/done", &url).unwrap());
        }
        assert!(!t.set_insert("crawl/42/done", "https://a.test/0").unwrap());
    }
    let store = Store::open(config(dir.path())).unwrap();
    let t = store.tasks();
    assert_eq!(
        t.kv_keys("crawl/").unwrap(),
        vec![b"crawl/42/meta".to_vec()]
    );
    assert_eq!(t.list_len("crawl/42/queue"), 15);
    assert_eq!(t.set_len("crawl/42/done"), 5);
    assert!(t.set_contains("crawl/42/done", "https://a.test/4").unwrap());
    assert!(!t.set_contains("crawl/42/done", "https://a.test/5").unwrap());
    assert_eq!(
        t.list_front("crawl/42/queue").unwrap().unwrap(),
        b"https://a.test/5"
    );
    // 新元素接在后面，不会覆盖未弹出的元素。
    let seq = t
        .list_push("crawl/42/queue", b"https://a.test/new")
        .unwrap();
    assert_eq!(seq, 20);
    let items = t.list_items("crawl/42/queue", 100).unwrap();
    assert_eq!(items.len(), 16);
    assert_eq!(items.last().unwrap(), b"https://a.test/new");
    assert_eq!(t.list_clear("crawl/42/queue").unwrap(), 16);
    assert_eq!(t.set_clear("crawl/42/done").unwrap(), 5);
    assert_eq!(t.list_len("crawl/42/queue"), 0);
    assert!(t.list_pop_front("crawl/42/queue").unwrap().is_none());
}

#[test]
fn concurrent_readers_and_writer() {
    let dir = tempfile::tempdir().unwrap();
    let mut c = config(dir.path());
    c.max_segment_bytes = 64 << 10;
    let store = Store::open(c).unwrap();
    for i in 0..200 {
        store
            .cache()
            .put(format!("k{i}"), format!("v{i}").as_bytes())
            .unwrap();
    }
    std::thread::scope(|s| {
        for _ in 0..4 {
            let store = store.clone();
            s.spawn(move || {
                for round in 0..20 {
                    for i in 0..200 {
                        let e = store.cache().entry(format!("k{i}")).unwrap().unwrap();
                        assert!(e.value.starts_with(b"v"), "round {round}");
                    }
                }
            });
        }
        let store = store.clone();
        s.spawn(move || {
            for i in 0..200 {
                store
                    .cache()
                    .put(format!("k{i}"), format!("v{i}-2").as_bytes())
                    .unwrap();
            }
            store.compact().unwrap();
        });
    });
    assert_eq!(store.cache().entry("k7").unwrap().unwrap().value, b"v7-2");
}

#[test]
fn oversized_key_is_rejected() {
    let dir = tempfile::tempdir().unwrap();
    let store = Store::open(config(dir.path())).unwrap();
    let key = vec![b'k'; 70_000];
    assert!(matches!(
        store.cache().put(&key, b"v"),
        Err(Error::KeyTooLarge(_))
    ));
}

#[test]
fn a_second_open_of_the_same_directory_is_refused_until_the_first_is_dropped() {
    let dir = tempfile::tempdir().unwrap();
    let first = Store::open(config(dir.path())).unwrap();
    let err = Store::open(config(dir.path()))
        .err()
        .expect("second open must fail");
    assert!(err.is_locked(), "{err}");
    drop(first);
    Store::open(config(dir.path())).expect("lock released with the store");
}
