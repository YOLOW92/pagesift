//! 请求头、响应体读取与底层传输（reqwest / 自有 HTTP 客户端）的适配。

use super::*;

/// 把 Location 头的原始字节转成文本。`HeaderValue::to_str` 只接受可见 ASCII，
/// 而不少服务器直接发 UTF-8 路径（`/目标 页面?q=中文`）。合法 UTF-8 原样交给 `Url::join`，
/// 它按 WHATWG URL 规则把非 ASCII 和空格百分号编码；不是 UTF-8 时按 latin-1 读取、
/// 把每个高位字节直接写成 `%XX`，保留服务器原本的字节（与 Firefox 一致）。
/// SSRF 与重定向次数检查照常作用于解析结果。
pub(super) fn location_text(raw: &[u8]) -> String {
    if let Ok(text) = std::str::from_utf8(raw) {
        return text.trim().to_owned();
    }
    let mut out = String::with_capacity(raw.len() * 3);
    for &b in raw {
        if b.is_ascii() {
            out.push(char::from(b));
        } else {
            out.push_str(&format!("%{b:02X}"));
        }
    }
    out.trim().to_owned()
}

pub(super) fn merge_cookie(out: &mut HeaderMap, v: &HeaderValue) {
    let mut merged = match out.get(header::COOKIE) {
        Some(old) => {
            let mut b = old.as_bytes().to_vec();
            b.extend_from_slice(b"; ");
            b.extend_from_slice(v.as_bytes());
            HeaderValue::from_bytes(&b).unwrap_or_else(|_| v.clone())
        }
        None => v.clone(),
    };
    merged.set_sensitive(true);
    out.insert(header::COOKIE, merged);
}

pub(super) const FORBIDDEN_HEADERS: [&str; 12] = [
    "host",
    "content-length",
    "transfer-encoding",
    "connection",
    "keep-alive",
    "upgrade",
    "te",
    "trailer",
    "proxy-authorization",
    "proxy-connection",
    "accept-encoding",
    "expect",
];

pub(super) fn parse_header(name: &str, value: &str) -> Result<(HeaderName, HeaderValue)> {
    let n = HeaderName::from_bytes(name.trim().as_bytes())
        .map_err(|_| Error::Header(name.to_string()))?;
    if FORBIDDEN_HEADERS.contains(&n.as_str()) {
        return Err(Error::Header(n.to_string()));
    }
    if n == header::USER_AGENT {
        validate_user_agent(value)?;
    }
    let mut v = HeaderValue::from_str(value).map_err(|_| Error::Header(n.to_string()))?;
    v.set_sensitive(true);
    Ok((n, v))
}

pub(super) fn credential_headers(c: &HostCredentials) -> Result<HeaderMap> {
    let mut out = HeaderMap::new();
    for (k, v) in &c.headers {
        let (n, v) = parse_header(k, v.expose())?;
        out.insert(n, v);
    }
    if !c.cookies.is_empty() {
        let joined = c
            .cookies
            .iter()
            .map(|(k, v)| format!("{k}={}", v.expose()))
            .collect::<Vec<_>>()
            .join("; ");
        let (_, v) = parse_header("cookie", &joined)?;
        merge_cookie(&mut out, &v);
    }
    Ok(out)
}

pub(super) fn request_headers(req: &FetchRequest) -> Result<HeaderMap> {
    let mut out = HeaderMap::new();
    for (k, v) in &req.headers {
        let (n, v) = parse_header(k, v)?;
        if n == header::COOKIE {
            merge_cookie(&mut out, &v);
        } else {
            out.insert(n, v);
        }
    }
    if let Some(etag) = &req.if_none_match {
        let v = HeaderValue::from_str(etag).map_err(|_| Error::Header("if-none-match".into()))?;
        out.insert(header::IF_NONE_MATCH, v);
    }
    if let Some(t) = req.if_modified_since {
        let v = HeaderValue::from_str(&httpdate::fmt_http_date(t))
            .map_err(|_| Error::Header("if-modified-since".into()))?;
        out.insert(header::IF_MODIFIED_SINCE, v);
    }
    Ok(out)
}

pub(super) async fn read_body(
    mut resp: Resp,
    max: usize,
    deadline: Instant,
    body_idle_timeout: Duration,
) -> Result<(Vec<u8>, bool)> {
    // 有 Content-Length 就一次分配到位，免得边读边扩容（扩容会多占一倍并反复拷贝）
    let hint = resp
        .headers()
        .get(header::CONTENT_LENGTH)
        .and_then(|v| v.to_str().ok())
        .and_then(|v| v.parse::<u64>().ok())
        .map_or(0, |n| n.min(max as u64) as usize);
    let mut body = Vec::with_capacity(hint);
    loop {
        let idle_deadline = (Instant::now() + body_idle_timeout).min(deadline);
        let chunk = match timeout_at(idle_deadline, resp.chunk()).await {
            Err(_) if idle_deadline == deadline => return Err(Error::Timeout(TimeoutPhase::Total)),
            Err(_) => return Err(Error::Timeout(TimeoutPhase::BodyIdle)),
            Ok(c) => c?,
        };
        let Some(chunk) = chunk else {
            return Ok((body, false));
        };
        let room = max - body.len();
        if chunk.len() > room {
            body.extend_from_slice(&chunk[..room]);
            resp.abandon();
            return Ok((body, true));
        }
        body.extend_from_slice(&chunk);
    }
}

#[allow(clippy::large_enum_variant)] // 只构造一次，放在 Fetcher 里长期复用，不值得再分配一层 Box
pub(super) enum Transport {
    Direct(client::Client),
    Proxy {
        direct: client::Client,
        decoded: reqwest::Client,
        identity: reqwest::Client,
    },
}

#[allow(clippy::large_enum_variant)] // 每个响应只活一次请求，Box 只会多一次分配
pub(super) enum Resp {
    Direct(client::Response),
    Proxy(reqwest::Response),
}

impl Transport {
    pub(super) async fn get(
        &self,
        url: &Url,
        headers: HeaderMap,
        raw: bool,
        direct: bool,
    ) -> Result<Resp> {
        match self {
            Transport::Direct(c) => c.get(url, headers).await.map(Resp::Direct),
            Transport::Proxy {
                direct: direct_client,
                decoded,
                identity,
            } => {
                if direct {
                    return direct_client.get(url, headers).await.map(Resp::Direct);
                }
                (if raw { identity } else { decoded })
                    .get(url.clone())
                    .headers(headers)
                    .send()
                    .await
                    .map(Resp::Proxy)
                    .map_err(map_reqwest)
            }
        }
    }
}

impl Resp {
    pub(super) fn status(&self) -> u16 {
        match self {
            Resp::Direct(r) => r.status,
            Resp::Proxy(r) => r.status().as_u16(),
        }
    }

    pub(super) fn headers(&self) -> &HeaderMap {
        match self {
            Resp::Direct(r) => &r.headers,
            Resp::Proxy(r) => r.headers(),
        }
    }

    pub(super) fn ttfb(&self) -> Option<Duration> {
        match self {
            Resp::Direct(response) => Some(response.ttfb),
            Resp::Proxy(_) => None,
        }
    }

    pub(super) async fn chunk(&mut self) -> Result<Option<bytes::Bytes>> {
        match self {
            Resp::Direct(r) => r.chunk().await,
            Resp::Proxy(r) => r.chunk().await.map_err(map_reqwest),
        }
    }

    pub(super) fn abandon(&mut self) {
        if let Resp::Direct(response) = self {
            response.abandon();
        }
    }

    pub(super) async fn move_to_tail(&mut self, stages: &StageController) {
        if let Resp::Direct(response) = self {
            response.move_to_tail(stages).await;
        }
    }
}

pub(super) fn map_reqwest(e: reqwest::Error) -> Error {
    let mut src: Option<&(dyn std::error::Error + 'static)> = Some(&e);
    while let Some(s) = src {
        if let Some(ResolveError::Blocked(b)) = s.downcast_ref::<ResolveError>() {
            return Error::Blocked(b.clone());
        }
        src = s.source();
    }
    if e.is_timeout() {
        return Error::Timeout(if e.is_connect() {
            TimeoutPhase::Connect
        } else {
            TimeoutPhase::FirstByte
        });
    }
    let e = e.without_url();
    if let Some((kind, message)) = crate::tls::from_sources(&e) {
        return Error::Tls { kind, message };
    }
    let mut msg = e.to_string();
    let mut src = std::error::Error::source(&e);
    while let Some(s) = src {
        msg.push_str(": ");
        msg.push_str(&s.to_string());
        src = s.source();
    }
    if let Some(kind) = crate::tls::classify_text(&msg) {
        return Error::Tls { kind, message: msg };
    }
    Error::Network(msg)
}
