//! 指纹模拟的本地测试：真的走握手，服务端录下字节。和真 Chrome 的逐项比较在
//! `benchmarks/history/1.0/fingerprint/`，这里守住档案和线上字节一致、证书校验没有被绕过。

use std::io;
use std::net::{IpAddr, Ipv4Addr};
use std::pin::Pin;
use std::sync::{Arc, Mutex};
use std::task::{Context, Poll};

use pagesift_rustls::client::{HelloInput, HelloShaper};
use tokio::io::{AsyncRead, AsyncReadExt, AsyncWriteExt, ReadBuf};
use tokio::net::{TcpListener, TcpStream};
use tokio_rustls::TlsAcceptor;
use tokio_rustls::rustls::ServerConfig;
use tokio_rustls::rustls::pki_types::pem::PemObject;
use tokio_rustls::rustls::pki_types::{CertificateDer, PrivateKeyDer};

use super::hello::ChromeShaper;
use super::*;
use crate::{BoxFuture, DnsLookup, FetchRequest, Fetcher, FetcherConfig, SsrfPolicy};

// 测试专用的 CA 和叶子证书（localhost、fp.test，有效期一百年），私钥只用于这些测试。
const CA: &[u8] = include_bytes!("testdata/ca.pem");
const LEAF: &[u8] = include_bytes!("testdata/leaf.pem");
const LEAF_KEY: &[u8] = include_bytes!("testdata/leaf-key.pem");

struct LoopbackDns;

impl DnsLookup for LoopbackDns {
    fn lookup<'a>(&'a self, _host: &'a str) -> BoxFuture<'a, io::Result<Vec<IpAddr>>> {
        Box::pin(async { Ok(vec![IpAddr::V4(Ipv4Addr::LOCALHOST)]) })
    }
}

fn config() -> FetcherConfig {
    FetcherConfig {
        env_proxy: false,
        ssrf: SsrfPolicy {
            allow_ips: vec![IpAddr::V4(Ipv4Addr::LOCALHOST)],
            ..Default::default()
        },
        dns: Arc::new(LoopbackDns),
        browser_fingerprint: Some(FingerprintProfile::Chrome156),
        ..Default::default()
    }
}

fn trusting_fetcher() -> Fetcher {
    let fetcher = Fetcher::new(config()).unwrap();
    let mut roots = RootCertStore::empty();
    for cert in CertificateDer::pem_slice_iter(CA) {
        roots.add(cert.unwrap()).unwrap();
    }
    fetcher.direct_client().preset_fingerprint_roots(roots);
    fetcher
}

/// 记下读到的所有字节，握手之后用来看 ClientHello。
struct Recording {
    inner: TcpStream,
    seen: Arc<Mutex<Vec<u8>>>,
}

impl AsyncRead for Recording {
    fn poll_read(
        self: Pin<&mut Self>,
        cx: &mut Context<'_>,
        buf: &mut ReadBuf<'_>,
    ) -> Poll<io::Result<()>> {
        let this = self.get_mut();
        let before = buf.filled().len();
        let poll = Pin::new(&mut this.inner).poll_read(cx, buf);
        if poll.is_ready() {
            this.seen
                .lock()
                .unwrap()
                .extend_from_slice(&buf.filled()[before..]);
        }
        poll
    }
}

impl tokio::io::AsyncWrite for Recording {
    fn poll_write(
        self: Pin<&mut Self>,
        cx: &mut Context<'_>,
        buf: &[u8],
    ) -> Poll<io::Result<usize>> {
        Pin::new(&mut self.get_mut().inner).poll_write(cx, buf)
    }

    fn poll_flush(self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<io::Result<()>> {
        Pin::new(&mut self.get_mut().inner).poll_flush(cx)
    }

    fn poll_shutdown(self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<io::Result<()>> {
        Pin::new(&mut self.get_mut().inner).poll_shutdown(cx)
    }
}

fn acceptor(alpn: &[u8]) -> TlsAcceptor {
    let certs: Vec<_> = CertificateDer::pem_slice_iter(LEAF)
        .map(|c| c.unwrap())
        .collect();
    let key = PrivateKeyDer::from_pem_slice(LEAF_KEY).unwrap();
    let provider = Arc::new(tokio_rustls::rustls::crypto::ring::default_provider());
    let mut server = ServerConfig::builder_with_provider(provider)
        .with_safe_default_protocol_versions()
        .unwrap()
        .with_no_client_auth()
        .with_single_cert(certs, key)
        .unwrap();
    server.alpn_protocols = vec![alpn.to_vec()];
    TlsAcceptor::from(Arc::new(server))
}

fn frame(kind: u8, flags: u8, stream: u32, payload: &[u8]) -> Vec<u8> {
    let mut out = (payload.len() as u32).to_be_bytes()[1..].to_vec();
    out.extend_from_slice(&[kind, flags]);
    out.extend_from_slice(&stream.to_be_bytes());
    out.extend_from_slice(payload);
    out
}

#[derive(Debug, Default)]
struct H2Capture {
    client_hello: Vec<u8>,
    frames: Vec<(u8, u8, u32, Vec<u8>)>,
}

/// 只够收一个请求：读前言和帧，直到第一个 HEADERS，回 200 和一段正文。
async fn serve_h2_once(listener: TcpListener, body: &'static [u8]) -> H2Capture {
    let (tcp, _) = listener.accept().await.unwrap();
    let seen = Arc::new(Mutex::new(Vec::new()));
    let recording = Recording {
        inner: tcp,
        seen: seen.clone(),
    };
    let mut tls = acceptor(b"h2").accept(recording).await.unwrap();
    let client_hello = seen.lock().unwrap().clone();
    let mut preface = [0; 24];
    tls.read_exact(&mut preface).await.unwrap();
    assert_eq!(&preface, b"PRI * HTTP/2.0\r\n\r\nSM\r\n\r\n");
    tls.write_all(&frame(4, 0, 0, &[])).await.unwrap();
    let mut frames = Vec::new();
    loop {
        let mut head = [0; 9];
        tls.read_exact(&mut head).await.unwrap();
        let len = usize::from(head[0]) << 16 | usize::from(head[1]) << 8 | usize::from(head[2]);
        let mut payload = vec![0; len];
        tls.read_exact(&mut payload).await.unwrap();
        let stream = u32::from_be_bytes([head[5], head[6], head[7], head[8]]) & 0x7fff_ffff;
        let kind = head[3];
        frames.push((kind, head[4], stream, payload));
        if kind == 1 {
            // :status 200，然后正文
            tls.write_all(&frame(1, 4, stream, &[0x88])).await.unwrap();
            tls.write_all(&frame(0, 1, stream, body)).await.unwrap();
            tls.flush().await.unwrap();
            break;
        }
    }
    H2Capture {
        client_hello,
        frames,
    }
}

/// 只够收一个请求：HTTP/1.1。
async fn serve_h1_once(listener: TcpListener, body: &'static str) -> String {
    let (tcp, _) = listener.accept().await.unwrap();
    let mut tls = acceptor(b"http/1.1").accept(tcp).await.unwrap();
    let mut head = Vec::new();
    while !head.ends_with(b"\r\n\r\n") {
        let mut byte = [0];
        tls.read_exact(&mut byte).await.unwrap();
        head.push(byte[0]);
    }
    let response = format!(
        "HTTP/1.1 200 OK\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}",
        body.len()
    );
    tls.write_all(response.as_bytes()).await.unwrap();
    tls.shutdown().await.unwrap();
    String::from_utf8(head).unwrap()
}

/// 解析 ClientHello，给出 `(套件, 扩展类型的线上顺序)`。
fn parse_hello(record: &[u8]) -> (Vec<u16>, Vec<u16>) {
    assert_eq!(record[0], 22);
    let msg = &record[5..];
    assert_eq!(msg[0], 1);
    let body = &msg[4..];
    let mut pos = 34;
    pos += 1 + usize::from(body[pos]);
    let len = usize::from(u16::from_be_bytes([body[pos], body[pos + 1]]));
    let suites = body[pos + 2..pos + 2 + len]
        .chunks(2)
        .map(|c| u16::from_be_bytes([c[0], c[1]]))
        .collect();
    pos += 2 + len;
    pos += 1 + usize::from(body[pos]);
    let total = usize::from(u16::from_be_bytes([body[pos], body[pos + 1]]));
    pos += 2;
    let end = pos + total;
    let mut exts = Vec::new();
    while pos < end {
        exts.push(u16::from_be_bytes([body[pos], body[pos + 1]]));
        pos += 4 + usize::from(u16::from_be_bytes([body[pos + 2], body[pos + 3]]));
    }
    (suites, exts)
}

fn is_grease(value: u16) -> bool {
    value & 0x0f0f == 0x0a0a && value >> 8 == value & 0xff
}

/// 实录的 Chrome 156（`results/` 里的记录）。
const CHROME_EXTENSIONS: [u16; 17] = [
    0x0000, 0x0005, 0x000a, 0x000b, 0x000d, 0x0010, 0x0012, 0x0017, 0x001b, 0x0023, 0x002b, 0x002d,
    0x0033, 0x44cd, 0xca34, 0xfe0d, 0xff01,
];

#[test]
fn hello_shape_is_stable_within_a_connection_and_random_across() {
    let shaper = ChromeShaper::new(FingerprintProfile::Chrome156, [5; 32]);
    let key = vec![1; 32];
    let shares: [(u16, &[u8]); 1] = [(0x001d, key.as_slice())];
    let input = |random, retry| HelloInput {
        random,
        session_id: &[2; 32],
        server_name: Some("a.example"),
        key_shares: &shares,
        cookie: None,
        retry,
    };
    let record = |body: Vec<u8>| {
        let mut out = vec![22, 3, 1, 0, 0, 1, 0, 0, 0];
        out.extend_from_slice(&body);
        out
    };
    let first = shaper.shape(&input(&[1; 32], false)).unwrap();
    let again = shaper.shape(&input(&[1; 32], false)).unwrap();
    assert_eq!(
        first.body, again.body,
        "同一个 client random 字节相同（HRR 靠这个）"
    );
    let other = shaper.shape(&input(&[9; 32], false)).unwrap();
    let (suites, exts) = parse_hello(&record(first.body));
    let (_, other_exts) = parse_hello(&record(other.body));
    assert_ne!(exts, other_exts, "每连接重排");
    assert!(is_grease(suites[0]));
    assert_eq!(
        &suites[1..],
        FingerprintProfile::Chrome156.data().cipher_suites
    );
    assert!(is_grease(exts[0]) && is_grease(exts[exts.len() - 1]));
    assert_ne!(exts[0], exts[exts.len() - 1]);
    let mut middle: Vec<u16> = exts[1..exts.len() - 1].to_vec();
    middle.sort_unstable();
    assert_eq!(middle, CHROME_EXTENSIONS);
}

#[test]
fn retry_hello_keeps_order_and_echoes_cookie() {
    let shaper = ChromeShaper::new(FingerprintProfile::Chrome156, [5; 32]);
    let key = vec![1; 65];
    let shares: [(u16, &[u8]); 1] = [(0x0017, key.as_slice())];
    let hello = |retry, cookie| {
        shaper
            .shape(&HelloInput {
                random: &[1; 32],
                session_id: &[2; 32],
                server_name: Some("a.example"),
                key_shares: &shares,
                cookie,
                retry,
            })
            .unwrap()
    };
    let first = hello(false, None);
    let second = hello(true, Some(&[7, 7, 7]));
    assert_eq!(
        second.offered_extensions[..first.offered_extensions.len()],
        first.offered_extensions[..],
        "第二次 hello 的扩展顺序不变，cookie 接在后面"
    );
    assert_eq!(second.offered_extensions.last(), Some(&0x002c));
}

#[tokio::test]
async fn http2_matches_profile_on_the_wire() {
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let port = listener.local_addr().unwrap().port();
    let server = tokio::spawn(serve_h2_once(listener, b"hello h2"));
    let fetcher = trusting_fetcher();
    let response = fetcher
        .fetch(FetchRequest::new(format!("https://fp.test:{port}/a?b=1")))
        .await
        .unwrap();
    assert_eq!(response.status, 200);
    assert_eq!(response.body, b"hello h2");
    assert_eq!(
        response.browser_fingerprint,
        Some(FingerprintProfile::Chrome156)
    );
    assert!(response.warnings.is_empty(), "{:?}", response.warnings);

    let capture = server.await.unwrap();
    let (suites, exts) = parse_hello(&capture.client_hello);
    assert_eq!(
        &suites[1..],
        FingerprintProfile::Chrome156.data().cipher_suites
    );
    assert!(exts.contains(&0xca34) && exts.contains(&0x44cd));

    let kinds: Vec<u8> = capture.frames.iter().map(|f| f.0).collect();
    assert_eq!(kinds, [4, 8, 1], "SETTINGS、WINDOW_UPDATE、HEADERS 的顺序");
    let settings: Vec<(u16, u32)> = capture.frames[0]
        .3
        .chunks(6)
        .map(|c| {
            (
                u16::from_be_bytes([c[0], c[1]]),
                u32::from_be_bytes([c[2], c[3], c[4], c[5]]),
            )
        })
        .collect();
    assert_eq!(settings, [(1, 65536), (2, 0), (4, 6291456), (6, 262144)]);
    assert_eq!(capture.frames[1].3, 15663105u32.to_be_bytes());
    let (_, flags, stream, payload) = &capture.frames[2];
    assert_eq!((*flags, *stream), (0x25, 1));
    assert_eq!(
        payload[..5],
        [0x80, 0, 0, 0, 255],
        "exclusive、依赖 0、权重 256"
    );
    // 伪头部 m,a,s,p：先是 :method GET（静态表），接着 :authority 的字面量，不是 :scheme
    assert_eq!(payload[5], 0x82);
    assert_eq!(payload[6] & 0xc0, 0x40);
    assert!(payload.windows(1).any(|b| b[0] == 0x87));
}

#[tokio::test]
async fn http1_matches_profile_on_the_wire() {
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let port = listener.local_addr().unwrap().port();
    let server = tokio::spawn(serve_h1_once(listener, "hello h1"));
    let fetcher = trusting_fetcher();
    let response = fetcher
        .fetch(FetchRequest::new(format!("https://fp.test:{port}/")))
        .await
        .unwrap();
    assert_eq!(response.body, b"hello h1");
    let head = server.await.unwrap();
    let names: Vec<&str> = head
        .lines()
        .skip(1)
        .filter_map(|line| line.split(':').next())
        .filter(|name| !name.is_empty())
        .collect();
    assert_eq!(
        names,
        [
            "Host",
            "Connection",
            "sec-ch-ua",
            "sec-ch-ua-mobile",
            "sec-ch-ua-platform",
            "Upgrade-Insecure-Requests",
            "User-Agent",
            "Accept",
            "Sec-Fetch-Site",
            "Sec-Fetch-Mode",
            "Sec-Fetch-User",
            "Sec-Fetch-Dest",
            "Accept-Encoding",
            "Accept-Language",
        ]
    );
    assert!(head.contains(&format!(
        "User-Agent: {}\r\n",
        FingerprintProfile::Chrome156.user_agent()
    )));
}

/// 明文 HTTP 没有 TLS 指纹，请求头照样按档案发。
#[tokio::test]
async fn plain_http_uses_profile_headers() {
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let port = listener.local_addr().unwrap().port();
    let server = tokio::spawn(async move {
        let (mut tcp, _) = listener.accept().await.unwrap();
        let mut head = Vec::new();
        while !head.ends_with(b"\r\n\r\n") {
            let mut byte = [0];
            tcp.read_exact(&mut byte).await.unwrap();
            head.push(byte[0]);
        }
        tcp.write_all(b"HTTP/1.1 200 OK\r\nContent-Length: 2\r\nConnection: close\r\n\r\nok")
            .await
            .unwrap();
        String::from_utf8(head).unwrap()
    });
    let fetcher = Fetcher::new(config()).unwrap();
    let response = fetcher
        .fetch(FetchRequest::new(format!("http://fp.test:{port}/")))
        .await
        .unwrap();
    assert_eq!(response.body, b"ok");
    let head = server.await.unwrap();
    assert!(head.contains("\r\nsec-ch-ua-mobile: ?0\r\n"), "{head}");
    assert!(head.contains("\r\nSec-Fetch-Mode: navigate\r\n"), "{head}");
}

/// 证书不被信任就是不被信任：不退回、不重试普通连接。
#[tokio::test]
async fn untrusted_certificate_is_rejected() {
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let port = listener.local_addr().unwrap().port();
    let accepts = Arc::new(Mutex::new(0));
    let counter = accepts.clone();
    tokio::spawn(async move {
        loop {
            let Ok((tcp, _)) = listener.accept().await else {
                return;
            };
            *counter.lock().unwrap() += 1;
            let _ = acceptor(b"h2").accept(tcp).await;
        }
    });
    // 只信任系统根证书，测试 CA 不在里面
    let fetcher = Fetcher::new(config()).unwrap();
    let error = fetcher
        .fetch(FetchRequest::new(format!("https://fp.test:{port}/")))
        .await
        .unwrap_err();
    assert!(
        matches!(
            error,
            crate::Error::Tls {
                kind: TlsErrorKind::UntrustedRoot,
                ..
            }
        ),
        "{error:?}"
    );
    assert_eq!(*accepts.lock().unwrap(), 1, "证书错误不重连");
    assert!(
        !fetcher
            .direct_client()
            .has_fallen_back(true, "fp.test", port)
    );
}

#[tokio::test]
async fn wrong_hostname_is_rejected() {
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let port = listener.local_addr().unwrap().port();
    tokio::spawn(async move {
        let (tcp, _) = listener.accept().await.unwrap();
        let _ = acceptor(b"h2").accept(tcp).await;
    });
    let fetcher = trusting_fetcher();
    let error = fetcher
        .fetch(FetchRequest::new(format!("https://other.test:{port}/")))
        .await
        .unwrap_err();
    assert!(
        matches!(
            error,
            crate::Error::Tls {
                kind: TlsErrorKind::HostnameMismatch,
                ..
            }
        ),
        "{error:?}"
    );
}

/// 服务器选了只声明的 CBC 套件：明确失败，换普通连接，记住这个主机。这个测试里普通路径信任系统根证书，
/// 所以第二条连接以证书错误结束；成功和警告在 `benchmarks/history/1.0/fingerprint/run.sh` 里走完。
#[tokio::test]
async fn declared_only_suite_falls_back_to_an_ordinary_connection() {
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let port = listener.local_addr().unwrap().port();
    let hellos = Arc::new(Mutex::new(Vec::new()));
    let seen = hellos.clone();
    let server = tokio::spawn(async move {
        // 第一条连接：看到什么 hello 都选 0xc013
        let (mut tcp, _) = listener.accept().await.unwrap();
        let mut buf = vec![0; 4096];
        let n = tcp.read(&mut buf).await.unwrap();
        seen.lock().unwrap().push(buf[..n].to_vec());
        let mut body = vec![0x03, 0x03];
        body.extend_from_slice(&[0x42; 32]);
        body.extend_from_slice(&[0, 0xc0, 0x13, 0, 0, 0]);
        let mut record = vec![22, 3, 3];
        record.extend_from_slice(&((body.len() + 4) as u16).to_be_bytes());
        record.extend_from_slice(&[2, 0, 0, body.len() as u8]);
        record.extend_from_slice(&body);
        tcp.write_all(&record).await.unwrap();
        let _ = tcp.read(&mut buf).await;
        // 第二条连接：正常 TLS，录下 hello
        let (tcp, _) = listener.accept().await.unwrap();
        let bytes = Arc::new(Mutex::new(Vec::new()));
        let recording = Recording {
            inner: tcp,
            seen: bytes.clone(),
        };
        let _ = acceptor(b"h2").accept(recording).await;
        seen.lock().unwrap().push(bytes.lock().unwrap().clone());
    });
    let fetcher = trusting_fetcher();
    let error = fetcher
        .fetch(FetchRequest::new(format!("https://fp.test:{port}/")))
        .await
        .unwrap_err();
    assert!(
        matches!(
            error,
            crate::Error::Tls {
                kind: crate::tls::TlsErrorKind::UntrustedRoot,
                ..
            }
        ),
        "{error:?}"
    );
    assert!(
        fetcher
            .direct_client()
            .has_fallen_back(true, "fp.test", port)
    );
    server.await.unwrap();
    let hellos = hellos.lock().unwrap();
    let (_, shaped) = parse_hello(&hellos[0]);
    let (_, ordinary) = parse_hello(&hellos[1]);
    assert!(shaped.contains(&0xca34), "第一次按档案发");
    assert!(!ordinary.contains(&0xca34), "第二次是普通 rustls");
}

#[test]
fn off_by_default_and_rejected_with_a_proxy() {
    assert!(FetcherConfig::default().browser_fingerprint.is_none());
    let error = Fetcher::new(FetcherConfig {
        proxy: Some("http://127.0.0.1:1".into()),
        ..config()
    })
    .unwrap_err();
    assert!(error.to_string().contains("browser_fingerprint"), "{error}");
}

/// robots.txt 的匹配依据没变，一次请求也改不了线上的 UA。
#[tokio::test]
async fn user_agent_rules() {
    let fetcher = Fetcher::new(config()).unwrap();
    assert_eq!(crate::product_token(fetcher.user_agent()), "pagesift");
    let error = fetcher
        .fetch(FetchRequest::new("https://fp.test/").header("user-agent", "other-bot/1.0"))
        .await
        .unwrap_err();
    assert!(error.to_string().contains("browser_fingerprint"), "{error}");
}
