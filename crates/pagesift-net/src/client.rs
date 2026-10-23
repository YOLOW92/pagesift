//! 直连用的 HTTP 客户端，直接建在 hyper 的连接层上（配置了出口代理时仍走 reqwest）。
//!
//! 自己做这一层是为了内存：hyper 默认每条 HTTP/1 连接的读缓冲会自适应涨到约 400 KB，
//! 连接回到池里以后这块缓冲一直留着。并发爬几百个站时，空闲连接的缓冲就占了堆的四成；
//! reqwest 没有暴露这个参数。这里每次固定读 16 KB，正文边读边交给调用方，缓冲不会变大。
//! 连接池也在这里：HTTP/1 每个主机最多留 `max_idle` 条空闲连接，HTTP/2 每个主机一条共享连接。
//! DNS 走带 SSRF 校验的解析器，TLS 用 rustls 和系统根证书，ALPN 协商 h2。

use std::collections::HashMap;
use std::io::Read;
use std::net::IpAddr;
use std::sync::{Arc, Mutex};
use std::time::Duration;

use bytes::Bytes;
use http_body_util::{BodyExt, Empty};
use hyper::body::Incoming;
use hyper::client::conn::{http1, http2};
use hyper::header::{self, HeaderMap, HeaderValue};
use hyper::{Request, Uri};
use hyper_util::rt::{TokioExecutor, TokioIo};
use tokio::net::TcpStream;
use tokio::time::{Instant, timeout_at};
use tokio_rustls::TlsConnector;
use tokio_rustls::rustls::{self, pki_types::ServerName};
use url::Url;

use crate::dns::{GuardedResolver, ResolveError};
use crate::fetch::{Error, Result, TimeoutPhase};
use crate::ssrf::Blocked;

/// HTTP/1 每次读取的固定大小，也就是每条连接读缓冲的常驻大小。
const READ_BUF: usize = 16 * 1024;
/// 空闲连接总数超过这个值时，顺手清掉过期的。
const SWEEP_AT: usize = 4096;
const ACCEPT_ENCODING: &str = "gzip, br, zstd, deflate";

#[derive(Clone, PartialEq, Eq, Hash)]
struct Key {
    https: bool,
    host: String,
    port: u16,
}

type H1 = http1::SendRequest<Empty<Bytes>>;
type H2 = http2::SendRequest<Empty<Bytes>>;

#[derive(Default)]
struct Pools {
    h1: HashMap<Key, Vec<(H1, Instant)>>,
    h2: HashMap<Key, H2>,
    idle: usize,
}

struct Inner {
    resolver: Arc<GuardedResolver>,
    tls: TlsConnector,
    connect_timeout: Duration,
    idle_timeout: Duration,
    max_idle: usize,
    user_agent: HeaderValue,
    pools: Mutex<Pools>,
}

#[derive(Clone)]
pub(crate) struct Client(Arc<Inner>);

pub(crate) struct Response {
    pub status: u16,
    pub headers: HeaderMap,
    body: Incoming,
    /// HTTP/1 连接：正文读完后放回池里。
    release: Option<(Arc<Inner>, Key, H1)>,
}

impl Response {
    /// 下一段正文；读完返回 `None`，此时连接回到池里。
    pub async fn chunk(&mut self) -> Result<Option<Bytes>> {
        loop {
            match self.body.frame().await {
                None => {
                    if let Some((inner, key, conn)) = self.release.take() {
                        inner.put_h1(key, conn);
                    }
                    return Ok(None);
                }
                Some(Err(e)) => return Err(network(&e)),
                Some(Ok(frame)) => {
                    if let Ok(data) = frame.into_data() {
                        return Ok(Some(data));
                    }
                }
            }
        }
    }
}

impl Client {
    pub(crate) fn new(
        resolver: Arc<GuardedResolver>,
        connect_timeout: Duration,
        idle_timeout: Duration,
        max_idle: usize,
        user_agent: HeaderValue,
    ) -> Result<Self> {
        let mut roots = rustls::RootCertStore::empty();
        let native = rustls_native_certs::load_native_certs();
        for cert in native.certs {
            // 个别系统证书格式不对时跳过，不影响其他证书
            let _ = roots.add(cert);
        }
        if roots.is_empty() {
            return Err(Error::Config("no system root certificates found".into()));
        }
        let provider = Arc::new(rustls::crypto::ring::default_provider());
        let mut tls = rustls::ClientConfig::builder_with_provider(provider)
            .with_safe_default_protocol_versions()
            .map_err(|e| Error::Config(e.to_string()))?
            .with_root_certificates(roots)
            .with_no_client_auth();
        tls.alpn_protocols = vec![b"h2".to_vec(), b"http/1.1".to_vec()];
        Ok(Self(Arc::new(Inner {
            resolver,
            tls: TlsConnector::from(Arc::new(tls)),
            connect_timeout,
            idle_timeout,
            max_idle,
            user_agent,
            pools: Mutex::new(Pools::default()),
        })))
    }

    /// 发一个 GET。`headers` 是调用方的请求头（凭证、条件请求等），连接相关的头由这里加。
    pub(crate) async fn get(&self, url: &Url, headers: HeaderMap) -> Result<Response> {
        let https = match url.scheme() {
            "https" => true,
            "http" => false,
            s => return Err(Error::InvalidUrl(format!("unsupported scheme {s}"))),
        };
        let host = url
            .host_str()
            .ok_or_else(|| Error::InvalidUrl(crate::redact_url(url.as_str())))?;
        let port = url
            .port_or_known_default()
            .unwrap_or(if https { 443 } else { 80 });
        let key = Key {
            https,
            host: host.to_string(),
            port,
        };
        let path = match url.query() {
            Some(q) => format!("{}?{q}", url.path()),
            None => url.path().to_string(),
        };
        let authority = match url.port() {
            Some(p) => format!("{host}:{p}"),
            None => host.to_string(),
        };

        // 先用池里的连接；拿不到再新建
        let conn = match self.0.take(&key).await {
            Some(c) => c,
            None => self.connect(&key).await?,
        };
        let mut req = Request::get(
            match &conn {
                Conn::H1(_) => path.parse::<Uri>(),
                Conn::H2(_) => format!("{}://{authority}{path}", url.scheme()).parse::<Uri>(),
            }
            .map_err(|_| Error::InvalidUrl(crate::redact_url(url.as_str())))?,
        )
        .body(Empty::<Bytes>::new())
        .map_err(|e| Error::Network(e.to_string()))?;
        let h = req.headers_mut();
        if matches!(conn, Conn::H1(_)) {
            h.insert(
                header::HOST,
                HeaderValue::from_str(&authority).map_err(|_| Error::Header("host".into()))?,
            );
        }
        h.insert(header::USER_AGENT, self.0.user_agent.clone());
        h.insert(header::ACCEPT, HeaderValue::from_static("*/*"));
        h.insert(
            header::ACCEPT_ENCODING,
            HeaderValue::from_static(ACCEPT_ENCODING),
        );
        for (k, v) in headers.iter() {
            h.insert(k, v.clone());
        }

        match conn {
            Conn::H1(mut s) => {
                let resp = s.send_request(req).await.map_err(|e| network(&e))?;
                let (parts, body) = resp.into_parts();
                Ok(Response {
                    status: parts.status.as_u16(),
                    headers: parts.headers,
                    body,
                    release: Some((self.0.clone(), key, s)),
                })
            }
            Conn::H2(mut s) => {
                let resp = s.send_request(req).await.map_err(|e| network(&e))?;
                let (parts, body) = resp.into_parts();
                Ok(Response {
                    status: parts.status.as_u16(),
                    headers: parts.headers,
                    body,
                    release: None,
                })
            }
        }
    }

    async fn connect(&self, key: &Key) -> Result<Conn> {
        let deadline = Instant::now() + self.0.connect_timeout;
        let ips: Vec<IpAddr> = match key.host.trim_matches(['[', ']']).parse::<IpAddr>() {
            // IP 字面量在 check_url 里已经按 SSRF 规则校验过
            Ok(ip) => vec![ip],
            Err(_) => {
                match timeout_at(deadline, self.0.resolver.resolve_checked(&key.host)).await {
                    Err(_) => return Err(Error::Timeout(TimeoutPhase::Connect)),
                    Ok(Err(ResolveError::Blocked(Blocked::NoAddress(h)))) => {
                        return Err(Error::Dns(format!("no address for `{h}`")));
                    }
                    Ok(Err(ResolveError::Blocked(b))) => return Err(Error::Blocked(b)),
                    Ok(Err(ResolveError::Io(e))) => {
                        return Err(Error::Dns(format!("{}: {e}", key.host)));
                    }
                    Ok(Ok(ips)) => ips,
                }
            }
        };
        let mut last = None;
        let mut tcp = None;
        for ip in ips {
            match timeout_at(deadline, TcpStream::connect((ip, key.port))).await {
                Err(_) => return Err(Error::Timeout(TimeoutPhase::Connect)),
                Ok(Ok(s)) => {
                    tcp = Some(s);
                    break;
                }
                Ok(Err(e)) => last = Some(e),
            }
        }
        let tcp = tcp.ok_or_else(|| {
            Error::Network(match last {
                Some(e) => format!("connect error: {e}"),
                None => "no addresses to connect to".into(),
            })
        })?;
        let _ = tcp.set_nodelay(true);
        if !key.https {
            return h1_handshake(TokioIo::new(tcp)).await;
        }
        let name = ServerName::try_from(key.host.trim_matches(['[', ']']).to_string())
            .map_err(|_| Error::InvalidUrl(key.host.clone()))?;
        let tls = match timeout_at(deadline, self.0.tls.connect(name, tcp)).await {
            Err(_) => return Err(Error::Timeout(TimeoutPhase::Connect)),
            Ok(r) => r.map_err(|e| Error::Network(format!("tls error: {e}")))?,
        };
        if tls.get_ref().1.alpn_protocol() == Some(b"h2") {
            let (sender, conn) = http2::Builder::new(TokioExecutor::new())
                .handshake(TokioIo::new(tls))
                .await
                .map_err(|e| network(&e))?;
            tokio::spawn(conn);
            self.0.put_h2(key.clone(), sender.clone());
            return Ok(Conn::H2(sender));
        }
        h1_handshake(TokioIo::new(tls)).await
    }
}

enum Conn {
    H1(H1),
    H2(H2),
}

async fn h1_handshake<T>(io: T) -> Result<Conn>
where
    T: hyper::rt::Read + hyper::rt::Write + Unpin + Send + 'static,
{
    let (sender, conn) = http1::Builder::new()
        .read_buf_exact_size(Some(READ_BUF))
        .handshake(io)
        .await
        .map_err(|e| network(&e))?;
    tokio::spawn(async move {
        // 连接出错或对方关闭都在这里结束；请求那边会从 send_request 拿到错误
        let _ = conn.await;
    });
    Ok(Conn::H1(sender))
}

impl Inner {
    async fn take(&self, key: &Key) -> Option<Conn> {
        if let Some(h2) = self.h2_for(key) {
            let mut s = h2;
            if s.ready().await.is_ok() {
                return Some(Conn::H2(s));
            }
            self.pools.lock().expect("lock poisoned").h2.remove(key);
        }
        loop {
            let (mut s, since) = {
                let mut pools = self.pools.lock().expect("lock poisoned");
                let list = pools.h1.get_mut(key)?;
                let entry = list.pop();
                if list.is_empty() {
                    pools.h1.remove(key);
                }
                let entry = entry?;
                pools.idle -= 1;
                entry
            };
            if since.elapsed() >= self.idle_timeout || s.is_closed() {
                continue;
            }
            if s.ready().await.is_ok() {
                return Some(Conn::H1(s));
            }
        }
    }

    fn h2_for(&self, key: &Key) -> Option<H2> {
        let pools = self.pools.lock().expect("lock poisoned");
        let s = pools.h2.get(key)?;
        (!s.is_closed()).then(|| s.clone())
    }

    fn put_h2(&self, key: Key, sender: H2) {
        self.pools
            .lock()
            .expect("lock poisoned")
            .h2
            .insert(key, sender);
    }

    fn put_h1(&self, key: Key, sender: H1) {
        if self.max_idle == 0 || sender.is_closed() {
            return;
        }
        let mut pools = self.pools.lock().expect("lock poisoned");
        if pools.idle >= SWEEP_AT || pools.h1.len() >= 512 {
            let timeout = self.idle_timeout;
            let mut removed = 0;
            pools.h1.retain(|_, list| {
                let before = list.len();
                list.retain(|(s, since)| since.elapsed() < timeout && !s.is_closed());
                removed += before - list.len();
                !list.is_empty()
            });
            pools.idle -= removed;
            pools.h2.retain(|_, s| !s.is_closed());
        }
        let list = pools.h1.entry(key).or_default();
        if list.len() >= self.max_idle {
            return;
        }
        list.push((sender, Instant::now()));
        pools.idle += 1;
    }
}

fn network(e: &hyper::Error) -> Error {
    let mut msg = e.to_string();
    let mut src = std::error::Error::source(e);
    while let Some(s) = src {
        msg.push_str(": ");
        msg.push_str(&s.to_string());
        src = s.source();
    }
    Error::Network(msg)
}

/// 按 Content-Encoding 解压，最多输出 `max` 字节；返回解压结果和是否被截断。
/// 不认识的编码原样返回。解压成功后去掉 Content-Encoding 和 Content-Length（它们描述的是压缩前）。
pub(crate) fn decode_body(headers: &mut HeaderMap, raw: Vec<u8>, max: usize) -> (Vec<u8>, bool) {
    let encoding = headers
        .get(header::CONTENT_ENCODING)
        .and_then(|v| v.to_str().ok())
        .map(|v| v.trim().to_ascii_lowercase());
    let Some(encoding) = encoding.filter(|e| !e.is_empty() && e != "identity") else {
        return (raw, false);
    };
    let mut reader: Box<dyn Read + '_> = match encoding.as_str() {
        "gzip" | "x-gzip" => Box::new(flate2::read::MultiGzDecoder::new(&raw[..])),
        "deflate" => {
            // 规范是 zlib 封装，但不少服务器发的是裸 deflate：看头两个字节判断
            let zlib = raw.len() >= 2
                && raw[0] & 0x0f == 8
                && u16::from_be_bytes([raw[0], raw[1]]).is_multiple_of(31);
            if zlib {
                Box::new(flate2::read::ZlibDecoder::new(&raw[..]))
            } else {
                Box::new(flate2::read::DeflateDecoder::new(&raw[..]))
            }
        }
        "br" => Box::new(brotli_decompressor::Decompressor::new(&raw[..], 16 * 1024)),
        "zstd" => match zstd::stream::read::Decoder::with_buffer(&raw[..]) {
            Ok(d) => Box::new(d),
            Err(_) => return (raw, false),
        },
        _ => return (raw, false),
    };
    let mut out = Vec::with_capacity(raw.len().saturating_mul(4).min(max));
    let mut buf = [0u8; 16 * 1024];
    let mut truncated = false;
    loop {
        match reader.read(&mut buf) {
            Ok(0) => break,
            Ok(n) => {
                let room = max - out.len();
                if n > room {
                    out.extend_from_slice(&buf[..room]);
                    truncated = true;
                    break;
                }
                out.extend_from_slice(&buf[..n]);
            }
            // 数据不完整（比如压缩流本身被截断）：保留已经解出的部分
            Err(_) => {
                truncated = true;
                break;
            }
        }
    }
    drop(reader);
    headers.remove(header::CONTENT_ENCODING);
    headers.remove(header::CONTENT_LENGTH);
    (out, truncated)
}

#[cfg(test)]
mod tests {
    use std::io::Write;

    use super::*;

    fn decode(encoding: &str, raw: Vec<u8>, max: usize) -> (Vec<u8>, bool, HeaderMap) {
        let mut h = HeaderMap::new();
        h.insert(
            header::CONTENT_ENCODING,
            HeaderValue::from_str(encoding).unwrap(),
        );
        h.insert(header::CONTENT_LENGTH, HeaderValue::from_static("1"));
        let (body, cut) = decode_body(&mut h, raw, max);
        (body, cut, h)
    }

    #[test]
    fn decodes_every_supported_encoding() {
        let text = b"hello hello hello hello".repeat(50);
        let mut z = flate2::write::ZlibEncoder::new(Vec::new(), flate2::Compression::fast());
        z.write_all(&text).unwrap();
        let mut d = flate2::write::DeflateEncoder::new(Vec::new(), flate2::Compression::fast());
        d.write_all(&text).unwrap();
        let zs = zstd::encode_all(&text[..], 1).unwrap();
        for (enc, raw) in [
            ("deflate", z.finish().unwrap()),
            ("deflate", d.finish().unwrap()),
            ("zstd", zs),
        ] {
            let (body, cut, h) = decode(enc, raw, 1 << 20);
            assert_eq!(body, text, "{enc}");
            assert!(!cut);
            assert!(h.get(header::CONTENT_ENCODING).is_none());
            assert!(h.get(header::CONTENT_LENGTH).is_none());
        }
        // 未压缩元数据块的 brotli 流："hello"
        let (body, _, _) = decode(
            "br",
            vec![0x0b, 0x02, 0x80, b'h', b'e', b'l', b'l', b'o', 0x03],
            100,
        );
        assert_eq!(body, b"hello");
    }

    #[test]
    fn caps_decoded_size_and_keeps_unknown_encodings() {
        let mut g = flate2::write::GzEncoder::new(Vec::new(), flate2::Compression::fast());
        g.write_all(&[b'a'; 100_000]).unwrap();
        let (body, cut, _) = decode("gzip", g.finish().unwrap(), 1000);
        assert_eq!(body.len(), 1000);
        assert!(cut, "decompression bombs stop at the body limit");

        let (body, cut, h) = decode("compress", b"raw".to_vec(), 100);
        assert_eq!(body, b"raw");
        assert!(!cut);
        assert!(h.get(header::CONTENT_ENCODING).is_some());
    }
}
