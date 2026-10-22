//! Resolve an existing remote CDP endpoint independently of local process support.

use crate::error::{Error, Result};
use std::time::Duration;
use tokio::io::{AsyncReadExt, AsyncWriteExt};

/// 远程端点：`ws://` 直接用；`http://host:port` 读 `/json/version` 拿 WebSocket 地址。
pub(crate) async fn resolve_endpoint(endpoint: &str) -> Result<String> {
    let url = url::Url::parse(endpoint).map_err(|e| Error::Connect(format!("{endpoint}: {e}")))?;
    match url.scheme() {
        "ws" => return Ok(endpoint.to_owned()),
        "http" => {}
        s => {
            return Err(Error::Connect(format!(
                "unsupported CDP endpoint scheme `{s}` (use ws:// or http://)"
            )));
        }
    }
    let host = url
        .host_str()
        .ok_or_else(|| Error::Connect("endpoint has no host".into()))?;
    let port = url.port_or_known_default().unwrap_or(80);
    let fetch = async {
        let mut stream = tokio::net::TcpStream::connect((host, port)).await?;
        let req = format!(
            "GET /json/version HTTP/1.1\r\nHost: {host}:{port}\r\nConnection: close\r\n\r\n"
        );
        stream.write_all(req.as_bytes()).await?;
        // DevTools 的 HTTP 服务不一定按 Connection: close 断开，按 Content-Length 读。
        let mut buf = Vec::new();
        let mut chunk = [0u8; 8192];
        loop {
            let n = stream.read(&mut chunk).await?;
            buf.extend_from_slice(&chunk[..n]);
            if n == 0 || buf.len() > 1 << 20 || http_body_complete(&buf) {
                break;
            }
        }
        Ok::<_, std::io::Error>(buf)
    };
    let buf = tokio::time::timeout(Duration::from_secs(10), fetch)
        .await
        .map_err(|_| Error::Connect("timed out reading /json/version".into()))?
        .map_err(|e| Error::Connect(e.to_string()))?;
    let text = String::from_utf8_lossy(&buf);
    let body = text
        .split_once("\r\n\r\n")
        .map(|(_, b)| b)
        .unwrap_or_default();
    let v: serde_json::Value = serde_json::from_str(body.trim())
        .map_err(|e| Error::Connect(format!("bad /json/version: {e}")))?;
    let ws = v
        .get("webSocketDebuggerUrl")
        .and_then(|v| v.as_str())
        .ok_or_else(|| Error::Connect("no webSocketDebuggerUrl in /json/version".into()))?;
    // 浏览器可能报出 localhost 之类的地址，换成调用方给的主机和端口。
    let mut ws = url::Url::parse(ws).map_err(|e| Error::Connect(e.to_string()))?;
    let _ = ws.set_host(Some(host));
    let _ = ws.set_port(Some(port));
    Ok(ws.to_string())
}

fn http_body_complete(buf: &[u8]) -> bool {
    let Some(end) = buf.windows(4).position(|w| w == b"\r\n\r\n") else {
        return false;
    };
    let head = String::from_utf8_lossy(&buf[..end]).to_ascii_lowercase();
    let len = head
        .lines()
        .find_map(|l| l.strip_prefix("content-length:"))
        .and_then(|v| v.trim().parse::<usize>().ok());
    len.is_some_and(|len| buf.len() >= end + 4 + len)
}
