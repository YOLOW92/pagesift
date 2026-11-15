//! 本地测试服务器：按路径返回原始 HTTP 响应，记录收到的请求头。

#![allow(dead_code)]

use std::collections::HashMap;
use std::io;
use std::net::{IpAddr, SocketAddr};
use std::sync::{Arc, Mutex};

use pagesift_net::{BoxFuture, DnsLookup};
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::TcpListener;

#[derive(Debug, Clone)]
pub struct Seen {
    pub path: String,
    pub headers: Vec<(String, String)>,
}

impl Seen {
    pub fn header(&self, name: &str) -> Option<&str> {
        self.headers
            .iter()
            .find(|(k, _)| k.eq_ignore_ascii_case(name))
            .map(|(_, v)| v.as_str())
    }
}

pub type Handler = Arc<dyn Fn(&Seen) -> Option<Vec<u8>> + Send + Sync>;

pub struct Server {
    pub addr: SocketAddr,
    pub seen: Arc<Mutex<Vec<Seen>>>,
}

impl Server {
    pub fn url(&self, path: &str) -> String {
        format!("http://{}{}", self.addr, path)
    }

    pub fn seen(&self) -> Vec<Seen> {
        self.seen.lock().unwrap().clone()
    }
}

/// handler 返回 None 表示不回应（用来测超时）。
pub async fn serve(handler: impl Fn(&Seen) -> Option<Vec<u8>> + Send + Sync + 'static) -> Server {
    let handler: Handler = Arc::new(handler);
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    let seen = Arc::new(Mutex::new(Vec::new()));
    let log = seen.clone();
    tokio::spawn(async move {
        loop {
            let Ok((mut sock, _)) = listener.accept().await else {
                return;
            };
            let handler = handler.clone();
            let log = log.clone();
            tokio::spawn(async move {
                let mut buf = Vec::new();
                let mut tmp = [0u8; 4096];
                while !buf.windows(4).any(|w| w == b"\r\n\r\n") {
                    match sock.read(&mut tmp).await {
                        Ok(0) | Err(_) => return,
                        Ok(n) => buf.extend_from_slice(&tmp[..n]),
                    }
                }
                let text = String::from_utf8_lossy(&buf).into_owned();
                let mut lines = text.split("\r\n");
                let path = lines
                    .next()
                    .and_then(|l| l.split(' ').nth(1))
                    .unwrap_or("/")
                    .to_string();
                let headers = lines
                    .filter_map(|l| l.split_once(':'))
                    .map(|(k, v)| (k.trim().to_ascii_lowercase(), v.trim().to_string()))
                    .collect();
                let req = Seen { path, headers };
                log.lock().unwrap().push(req.clone());
                match handler(&req) {
                    Some(resp) => {
                        let _ = sock.write_all(&resp).await;
                        let _ = sock.shutdown().await;
                    }
                    None => tokio::time::sleep(std::time::Duration::from_secs(30)).await,
                }
            });
        }
    });
    Server { addr, seen }
}

pub fn response(status: u16, headers: &[(&str, &str)], body: &[u8]) -> Vec<u8> {
    let mut out = format!(
        "HTTP/1.1 {status} X\r\nContent-Length: {}\r\nConnection: close\r\n",
        body.len()
    );
    for (k, v) in headers {
        out.push_str(&format!("{k}: {v}\r\n"));
    }
    out.push_str("\r\n");
    let mut out = out.into_bytes();
    out.extend_from_slice(body);
    out
}

pub fn ok(body: &str) -> Vec<u8> {
    response(200, &[("Content-Type", "text/html")], body.as_bytes())
}

pub fn redirect(status: u16, location: &str) -> Vec<u8> {
    response(status, &[("Location", location)], b"")
}

/// 假 DNS：固定域名到地址的映射。
pub struct FakeDns(pub HashMap<String, Vec<IpAddr>>);

impl FakeDns {
    pub fn new(entries: &[(&str, &[&str])]) -> Arc<Self> {
        Arc::new(Self(
            entries
                .iter()
                .map(|(h, ips)| {
                    (
                        h.to_string(),
                        ips.iter().map(|i| i.parse().unwrap()).collect(),
                    )
                })
                .collect(),
        ))
    }
}

impl DnsLookup for FakeDns {
    fn lookup<'a>(&'a self, host: &'a str) -> BoxFuture<'a, io::Result<Vec<IpAddr>>> {
        Box::pin(async move {
            self.0
                .get(host)
                .cloned()
                .ok_or_else(|| io::Error::new(io::ErrorKind::NotFound, "no such host"))
        })
    }
}
