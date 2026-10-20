//! 本地假 HTTP 服务器：按顺序返回预设响应（用完后重复最后一个），并记录收到的请求。
#![allow(dead_code)]

use std::sync::{Arc, Mutex};
use std::time::Duration;

use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::TcpListener;

#[derive(Clone)]
pub struct Resp {
    pub status: u16,
    pub headers: Vec<(String, String)>,
    pub body: String,
    pub delay: Duration,
}

impl Resp {
    pub fn json(status: u16, body: impl Into<String>) -> Self {
        Self {
            status,
            headers: vec![("Content-Type".into(), "application/json".into())],
            body: body.into(),
            delay: Duration::ZERO,
        }
    }
    pub fn header(mut self, k: &str, v: &str) -> Self {
        self.headers.push((k.into(), v.into()));
        self
    }
    pub fn delay(mut self, d: Duration) -> Self {
        self.delay = d;
        self
    }
}

#[derive(Debug, Clone)]
pub struct Recorded {
    pub request_line: String,
    pub headers: Vec<(String, String)>,
    pub body: String,
}

impl Recorded {
    pub fn header(&self, name: &str) -> Option<&str> {
        self.headers
            .iter()
            .find(|(k, _)| k.eq_ignore_ascii_case(name))
            .map(|(_, v)| v.as_str())
    }
    pub fn path(&self) -> &str {
        self.request_line.split(' ').nth(1).unwrap_or("")
    }
}

pub struct FakeServer {
    pub base: String,
    pub requests: Arc<Mutex<Vec<Recorded>>>,
}

impl FakeServer {
    pub fn requests(&self) -> Vec<Recorded> {
        self.requests.lock().unwrap().clone()
    }
}

pub async fn spawn(responses: Vec<Resp>) -> FakeServer {
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let base = format!("http://{}", listener.local_addr().unwrap());
    let requests = Arc::new(Mutex::new(Vec::new()));
    let log = requests.clone();
    tokio::spawn(async move {
        let mut n = 0usize;
        loop {
            let Ok((mut sock, _)) = listener.accept().await else {
                return;
            };
            let resp = responses[n.min(responses.len() - 1)].clone();
            n += 1;
            let log = log.clone();
            tokio::spawn(async move {
                let Some(rec) = read_request(&mut sock).await else {
                    return;
                };
                log.lock().unwrap().push(rec);
                tokio::time::sleep(resp.delay).await;
                let mut out = format!("HTTP/1.1 {} X\r\n", resp.status);
                for (k, v) in &resp.headers {
                    out.push_str(&format!("{k}: {v}\r\n"));
                }
                out.push_str(&format!(
                    "Content-Length: {}\r\nConnection: close\r\n\r\n{}",
                    resp.body.len(),
                    resp.body
                ));
                let _ = sock.write_all(out.as_bytes()).await;
                let _ = sock.shutdown().await;
            });
        }
    });
    FakeServer { base, requests }
}

async fn read_request(sock: &mut tokio::net::TcpStream) -> Option<Recorded> {
    let mut buf = Vec::new();
    let mut tmp = [0u8; 4096];
    let head_end = loop {
        if let Some(p) = buf.windows(4).position(|w| w == b"\r\n\r\n") {
            break p;
        }
        let n = sock.read(&mut tmp).await.ok()?;
        if n == 0 {
            return None;
        }
        buf.extend_from_slice(&tmp[..n]);
    };
    let head = String::from_utf8_lossy(&buf[..head_end]).into_owned();
    let mut lines = head.split("\r\n");
    let request_line = lines.next()?.to_owned();
    let headers: Vec<(String, String)> = lines
        .filter_map(|l| l.split_once(':'))
        .map(|(k, v)| (k.trim().to_owned(), v.trim().to_owned()))
        .collect();
    let len = headers
        .iter()
        .find(|(k, _)| k.eq_ignore_ascii_case("content-length"))
        .and_then(|(_, v)| v.parse::<usize>().ok())
        .unwrap_or(0);
    let mut body = buf[head_end + 4..].to_vec();
    while body.len() < len {
        let n = sock.read(&mut tmp).await.ok()?;
        if n == 0 {
            break;
        }
        body.extend_from_slice(&tmp[..n]);
    }
    Some(Recorded {
        request_line,
        headers,
        body: String::from_utf8_lossy(&body).into_owned(),
    })
}
