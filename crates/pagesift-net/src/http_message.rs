//! 解析存档里的 HTTP/1.x 响应报文（WARC `response` 记录的块）：状态行、头部、分块传输、
//! Content-Encoding。解压和网络抓取用同一个 `decode_body`（层数上限、解压后大小上限、损坏流的处理一致）。

use reqwest::header::{HeaderMap, HeaderName, HeaderValue};

/// 解析出的响应。
#[derive(Debug)]
pub struct HttpMessage {
    pub status: u16,
    pub headers: HeaderMap,
    /// 去掉分块传输、解开 Content-Encoding 之后的正文。
    pub body: Vec<u8>,
    /// 正文触到 `max` 被截断，或报文本身不完整。
    pub truncated: bool,
    pub warnings: Vec<String>,
}

/// 解析 `raw`（状态行开头的完整 HTTP/1.x 响应），正文最多保留 `max` 字节。
pub fn parse_response(raw: &[u8], max: usize) -> Result<HttpMessage, String> {
    let head_end = find(raw, b"\r\n\r\n")
        .map(|i| (i, i + 4)) // [协议] CRLF CRLF 的长度（RFC 9112 §2.1）
        .or_else(|| find(raw, b"\n\n").map(|i| (i, i + 2)))
        .ok_or("HTTP message without a header terminator")?;
    let head = std::str::from_utf8(&raw[..head_end.0]).map_err(|_| "HTTP headers are not UTF-8")?;
    let mut lines = head.lines();
    let status_line = lines.next().ok_or("empty HTTP message")?;
    let mut parts = status_line.split_whitespace();
    let proto = parts.next().unwrap_or_default();
    if !proto.starts_with("HTTP/") {
        return Err(format!("not an HTTP response: {status_line:?}"));
    }
    let status: u16 = parts
        .next()
        .and_then(|s| s.parse().ok())
        .ok_or_else(|| format!("bad status line: {status_line:?}"))?;
    let mut headers = HeaderMap::new();
    for line in lines {
        let Some((k, v)) = line.split_once(':') else {
            continue;
        };
        if let (Ok(k), Ok(v)) = (
            HeaderName::from_bytes(k.trim().as_bytes()),
            HeaderValue::from_str(v.trim()),
        ) {
            headers.append(k, v);
        }
    }
    let mut body = raw[head_end.1..].to_vec();
    let mut truncated = false;
    let mut warnings = Vec::new();
    let chunked = headers
        .get_all(reqwest::header::TRANSFER_ENCODING)
        .iter()
        .filter_map(|v| v.to_str().ok())
        .any(|v| v.to_ascii_lowercase().contains("chunked"));
    if chunked {
        match dechunk(&body) {
            Ok(b) => body = b,
            Err(partial) => {
                body = partial;
                truncated = true;
                warnings.push("archive_body_incomplete: chunked body ends early".into());
            }
        }
        headers.remove(reqwest::header::TRANSFER_ENCODING);
    }
    let decoded = crate::client::decode_body(&mut headers, body, max, truncated)
        .map_err(|e| e.to_string())?;
    warnings.extend(decoded.warnings);
    Ok(HttpMessage {
        status,
        headers,
        body: decoded.body,
        truncated: truncated || decoded.truncated,
        warnings,
    })
}

fn find(hay: &[u8], needle: &[u8]) -> Option<usize> {
    hay.windows(needle.len()).position(|w| w == needle)
}

/// 去掉分块传输编码；格式坏了时返回已解出的部分（`Err`）。
fn dechunk(mut data: &[u8]) -> Result<Vec<u8>, Vec<u8>> {
    let mut out = Vec::with_capacity(data.len());
    loop {
        let Some(eol) = find(data, b"\n") else {
            return Err(out);
        };
        let size_line = std::str::from_utf8(&data[..eol]).map_err(|_| out.clone())?;
        let size_hex = size_line.trim().split(';').next().unwrap_or_default();
        // [协议] chunk-size 是十六进制（RFC 9112 §7.1）
        let Ok(size) = usize::from_str_radix(size_hex.trim(), 16) else {
            return Err(out);
        };
        data = &data[eol + 1..];
        if size == 0 {
            return Ok(out);
        }
        if data.len() < size {
            out.extend_from_slice(data);
            return Err(out);
        }
        out.extend_from_slice(&data[..size]);
        data = &data[size..];
        data = data
            .strip_prefix(b"\r\n")
            .or_else(|| data.strip_prefix(b"\n"))
            .unwrap_or(data);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Write;

    #[test]
    fn chunked_gzip_response() {
        let mut gz = flate2::write::GzEncoder::new(Vec::new(), flate2::Compression::fast());
        gz.write_all(b"<p>hello archive</p>").unwrap();
        let z = gz.finish().unwrap();
        let mut raw = b"HTTP/1.1 200 OK\r\nContent-Type: text/html\r\nContent-Encoding: gzip\r\nTransfer-Encoding: chunked\r\n\r\n".to_vec();
        let (a, b) = z.split_at(5);
        raw.extend(format!("{:x}\r\n", a.len()).as_bytes());
        raw.extend(a);
        raw.extend(format!("\r\n{:x}\r\n", b.len()).as_bytes());
        raw.extend(b);
        raw.extend(b"\r\n0\r\n\r\n");
        let m = parse_response(&raw, 1 << 20).unwrap();
        assert_eq!(m.status, 200);
        assert_eq!(m.body, b"<p>hello archive</p>");
        assert!(m.headers.get("content-encoding").is_none());
        assert_eq!(m.headers.get("content-type").unwrap(), "text/html");
        assert!(parse_response(b"garbage", 10).is_err());
    }
}
