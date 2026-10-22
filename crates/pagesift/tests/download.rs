//! Direct HTTP bodies, connection failure, validators, archive integrity and policy.
#[path = "../../pagesift-net/tests/common/mod.rs"]
mod common;
use anyhow::Result;
use common::{Server, serve};
use pagesift::download::{Archive, DownloadRequest};
use pagesift_net::{Fetcher, FetcherConfig};
use pagesift_policy::{Policy, PolicyConfig};
use std::sync::{
    Arc,
    atomic::{AtomicUsize, Ordering},
};
use std::time::Duration;

fn policy() -> Policy {
    let mut config = FetcherConfig::default();
    config.ssrf.allow_private_network = true;
    config.max_body_bytes = 1024; // Streaming must not buffer/truncate like fetch().
    let mut pc = PolicyConfig::default();
    pc.rate.default_interval = Duration::ZERO;
    Policy::new(Arc::new(Fetcher::new(config).unwrap()), pc)
}
fn request(srv: &Server) -> DownloadRequest {
    DownloadRequest {
        source: "site".into(),
        id: "item".into(),
        revision: "1".into(),
        url: srv.url("/file").parse().unwrap(),
        max_bytes: 8 * 1024 * 1024,
        expected_blake3: None,
        restart: false,
    }
}
fn raw(status: u16, headers: &str, body: &[u8], length: usize) -> Vec<u8> {
    let mut response = format!("HTTP/1.1 {status} Response\r\nConnection: close\r\nContent-Length: {length}\r\n{headers}\r\n").into_bytes();
    response.extend_from_slice(body);
    response
}
fn robots(path: &str) -> Option<Vec<u8>> {
    (path == "/robots.txt").then(|| common::ok("User-agent: *\nDisallow: /blocked\n"))
}

#[tokio::test]
async fn bounded_ranges_resume_and_never_accept_full_replacement_bytes() -> Result<()> {
    let count = Arc::new(AtomicUsize::new(0));
    let c = count.clone();
    let srv = serve(move |s| {
        robots(&s.path).or_else(|| {
            Some(match c.fetch_add(1, Ordering::SeqCst) {
                0 => {
                    assert_eq!(s.header("range"), Some("bytes=10-19"));
                    raw(
                        206,
                        "ETag: \"v1\"\r\nContent-Range: bytes 10-19/100\r\n",
                        b"abc",
                        10,
                    )
                }
                1 => {
                    assert_eq!(s.header("range"), Some("bytes=13-19"));
                    assert_eq!(s.header("if-range"), Some("\"v1\""));
                    raw(
                        200,
                        "ETag: \"v2\"\r\n",
                        b"whole replacement must not be used",
                        33,
                    )
                }
                _ => {
                    assert_eq!(s.header("range"), Some("bytes=10-19"));
                    assert!(s.header("if-range").is_none());
                    raw(
                        206,
                        "ETag: \"v2\"\r\nContent-Range: bytes 10-19/100\r\n",
                        b"0123456789",
                        10,
                    )
                }
            })
        })
    })
    .await;
    let dir = tempfile::tempdir()?;
    let mut archive = Archive::open(dir.path())?;
    let req = request(&srv);
    assert!(
        archive
            .download_range(&policy(), &req, 10, 19)
            .await
            .is_err()
    );
    assert!(
        archive
            .download_range(&policy(), &req, 10, 19)
            .await
            .is_err()
    );
    let result = archive.download_range(&policy(), &req, 10, 19).await?;
    assert_eq!(std::fs::read(&result.path)?, b"0123456789");
    assert_eq!(result.resumed_bytes, 0);
    assert!(
        archive
            .download_range(&policy(), &req, 10, 19)
            .await?
            .cached
    );
    assert!(
        archive
            .download_range(&policy(), &req, 20, 10)
            .await
            .is_err()
    );
    Ok(())
}

#[tokio::test]
async fn streams_large_file_and_archives_by_identity_not_url() -> Result<()> {
    let payload = vec![b'a'; 4 * 1024 * 1024];
    let bytes = payload.clone();
    let srv = serve(move |s| {
        robots(&s.path).or_else(|| {
            assert_eq!(s.header("accept-encoding"), Some("identity"));
            Some(raw(200, "ETag: \"v1\"\r\n", &bytes, bytes.len()))
        })
    })
    .await;
    let dir = tempfile::tempdir()?;
    let mut archive = Archive::open(dir.path())?;
    let mut req = request(&srv);
    req.expected_blake3 = Some(blake3::hash(&payload).to_hex().to_string());
    let result = archive.download(&policy(), &req).await?;
    assert_eq!(result.bytes, payload.len() as u64);
    assert!(!result.cached);
    assert_eq!(std::fs::read(&result.path)?, payload);
    req.url = srv.url("/renamed").parse()?;
    assert!(archive.download(&policy(), &req).await?.cached);
    assert!(!srv.seen().iter().any(|s| s.path == "/renamed"));
    req.revision = "2".into();
    let newer = archive.download(&policy(), &req).await?;
    assert_ne!(newer.path, result.path);
    assert!(!newer.cached);
    Ok(())
}

#[tokio::test]
async fn disconnect_resumes_validated_prefix_and_discards_uncommitted_tail() -> Result<()> {
    let payload = vec![b'x'; 8000];
    let body = payload.clone();
    let count = Arc::new(AtomicUsize::new(0));
    let c = count.clone();
    let srv = serve(move |s| {
        robots(&s.path).or_else(|| {
            if c.fetch_add(1, Ordering::SeqCst) == 0 {
                Some(raw(200, "ETag: \"v1\"\r\n", &body[..3000], body.len()))
            } else {
                assert_eq!(s.header("range"), Some("bytes=3000-"));
                assert_eq!(s.header("if-range"), Some("\"v1\""));
                Some(raw(
                    206,
                    "ETag: \"v1\"\r\nContent-Range: bytes 3000-7999/8000\r\n",
                    &body[3000..],
                    5000,
                ))
            }
        })
    })
    .await;
    let dir = tempfile::tempdir()?;
    let req = request(&srv);
    {
        let mut archive = Archive::open(dir.path())?;
        assert!(archive.download(&policy(), &req).await.is_err());
    }
    let part = std::fs::read_dir(dir.path())?
        .filter_map(|e| e.ok())
        .find(|e| e.path().extension().is_some_and(|x| x == "part"))
        .unwrap()
        .path();
    use std::io::Write;
    std::fs::OpenOptions::new()
        .append(true)
        .open(part)?
        .write_all(b"uncommitted")?;
    let mut archive = Archive::open(dir.path())?;
    let result = archive.download(&policy(), &req).await?;
    assert_eq!(result.resumed_bytes, 3000);
    assert_eq!(std::fs::read(result.path)?, payload);
    Ok(())
}

#[tokio::test]
async fn changed_validator_or_ignored_range_replaces_instead_of_appending() -> Result<()> {
    let count = Arc::new(AtomicUsize::new(0));
    let c = count.clone();
    let srv = serve(move |s| {
        robots(&s.path).or_else(|| {
            if c.fetch_add(1, Ordering::SeqCst) == 0 {
                Some(raw(200, "ETag: \"v1\"\r\n", b"old", 10))
            } else {
                assert_eq!(s.header("range"), Some("bytes=3-"));
                Some(raw(200, "ETag: \"v2\"\r\n", b"replacement", 11))
            }
        })
    })
    .await;
    let dir = tempfile::tempdir()?;
    let mut archive = Archive::open(dir.path())?;
    let req = request(&srv);
    assert!(archive.download(&policy(), &req).await.is_err());
    let result = archive.download(&policy(), &req).await?;
    assert_eq!(result.resumed_bytes, 0);
    assert_eq!(std::fs::read(result.path)?, b"replacement");
    Ok(())
}

#[tokio::test]
async fn bad_range_and_416_never_publish_incomplete_bytes() -> Result<()> {
    let count = Arc::new(AtomicUsize::new(0));
    let c = count.clone();
    let srv = serve(move |s| {
        robots(&s.path).or_else(|| {
            Some(match c.fetch_add(1, Ordering::SeqCst) {
                0 => raw(200, "ETag: \"v1\"\r\n", b"abc", 10),
                1 => raw(
                    206,
                    "ETag: \"v1\"\r\nContent-Range: bytes 2-9/10\r\n",
                    b"cdefghij",
                    8,
                ),
                _ => raw(416, "Content-Range: bytes */10\r\n", b"", 0),
            })
        })
    })
    .await;
    let dir = tempfile::tempdir()?;
    let mut archive = Archive::open(dir.path())?;
    let req = request(&srv);
    for _ in 0..3 {
        assert!(archive.download(&policy(), &req).await.is_err());
    }
    assert!(
        !std::fs::read_dir(dir.path())?
            .filter_map(|e| e.ok())
            .any(|e| e.path().extension().is_some_and(|x| x == "bin"))
    );
    Ok(())
}

#[tokio::test]
async fn no_strong_validator_restarts_and_checksum_failure_does_not_commit() -> Result<()> {
    for etag in ["", "ETag: W/\"weak\"\r\n"] {
        let etag = etag.to_owned();
        let count = Arc::new(AtomicUsize::new(0));
        let c = count.clone();
        let srv = serve(move |s| {
            robots(&s.path).or_else(|| {
                assert!(s.header("range").is_none());
                Some(if c.fetch_add(1, Ordering::SeqCst) == 0 {
                    raw(200, &etag, b"abc", 6)
                } else {
                    raw(200, &etag, b"abcdef", 6)
                })
            })
        })
        .await;
        let dir = tempfile::tempdir()?;
        let mut archive = Archive::open(dir.path())?;
        let mut req = request(&srv);
        assert!(archive.download(&policy(), &req).await.is_err());
        req.expected_blake3 = Some("0".repeat(64));
        assert!(archive.download(&policy(), &req).await.is_err());
        assert!(
            !std::fs::read_dir(dir.path())?
                .filter_map(|e| e.ok())
                .any(|e| e.path().extension().is_some_and(|x| x == "bin"))
        );
    }
    Ok(())
}

#[tokio::test]
async fn archive_lock_integrity_and_ready_receipt_recovery() -> Result<()> {
    let srv =
        serve(|s| robots(&s.path).or_else(|| Some(raw(200, "ETag: \"v1\"\r\n", b"complete", 8))))
            .await;
    let dir = tempfile::tempdir()?;
    let mut archive = Archive::open(dir.path())?;
    assert!(Archive::open(dir.path()).is_err());
    let req = request(&srv);
    let result = archive.download(&policy(), &req).await?;
    // The receipt was synced but the process died before publishing the name.
    std::fs::rename(&result.path, result.path.with_extension("part"))?;
    drop(archive);
    let mut archive = Archive::open(dir.path())?;
    assert!(archive.download(&policy(), &req).await?.cached);
    std::fs::write(&result.path, b"tampered")?;
    assert!(archive.download(&policy(), &req).await.is_err());
    Ok(())
}

#[tokio::test]
async fn policy_and_size_guards_reject_without_finished_files() -> Result<()> {
    let srv = serve(|s| {
        robots(&s.path).or_else(|| {
            Some(match s.path.as_str() {
                "/html" => raw(200, "Content-Type: text/html\r\n", b"login", 5),
                "/redirect" => common::redirect(302, "/blocked"),
                _ => raw(200, "", b"1234567890", 10),
            })
        })
    })
    .await;
    let dir = tempfile::tempdir()?;
    let mut archive = Archive::open(dir.path())?;
    for path in ["/file", "/blocked", "/redirect", "/html"] {
        let mut req = request(&srv);
        req.id = path.into();
        req.url = srv.url(path).parse()?;
        req.max_bytes = 5;
        assert!(archive.download(&policy(), &req).await.is_err());
    }
    assert!(!srv.seen().iter().any(|s| s.path == "/blocked"));
    assert!(
        !std::fs::read_dir(dir.path())?
            .filter_map(|e| e.ok())
            .any(|e| e.path().extension().is_some_and(|x| x == "bin"))
    );
    Ok(())
}
