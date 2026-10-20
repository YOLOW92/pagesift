use std::time::Duration;

use pagesift_store::{Config, Store};

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn blocking_runs_on_tokio() {
    let dir = tempfile::tempdir().unwrap();
    let store = Store::open(Config::new(dir.path())).unwrap();
    let mut handles = Vec::new();
    for i in 0..16 {
        let store = store.clone();
        handles.push(tokio::spawn(async move {
            let key = format!("https://a.test/{i}");
            let k = key.clone();
            store
                .blocking(move |s| s.cache().put(&k, b"body"))
                .await
                .unwrap();
            store
                .blocking(move |s| s.cache().get(&key, Duration::from_secs(60)))
                .await
                .unwrap()
                .unwrap()
                .value
        }));
    }
    for h in handles {
        assert_eq!(h.await.unwrap(), b"body");
    }
}
