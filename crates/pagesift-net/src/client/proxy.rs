//! Validated proxy tunnels for the fingerprint-shaped transport.
//!
//! Targets are resolved and SSRF-checked locally, then sent to the proxy as an
//! IP address. The original hostname remains the TLS SNI and HTTP authority.

use std::net::IpAddr;

use tokio::io::{AsyncRead, AsyncReadExt, AsyncWrite, AsyncWriteExt};
use tokio::net::TcpStream;
use tokio::time::{Instant, timeout_at};
use tokio_rustls::TlsConnector;
use tokio_rustls::rustls::pki_types::ServerName;
use url::Url;

use crate::fetch::{Error, Result, TimeoutPhase};

pub(super) trait AsyncIo: AsyncRead + AsyncWrite + Unpin + Send {}
impl<T: AsyncRead + AsyncWrite + Unpin + Send> AsyncIo for T {}
pub(super) type BoxStream = Box<dyn AsyncIo>;

#[derive(Clone)]
pub(crate) struct Proxy {
    pub(super) host: String,
    port: u16,
    scheme: String,
    username: String,
    password: String,
    pub(crate) exempt_host: bool,
}

impl Drop for Proxy {
    fn drop(&mut self) {
        zeroize::Zeroize::zeroize(&mut self.username);
        zeroize::Zeroize::zeroize(&mut self.password);
    }
}

impl Proxy {
    pub(crate) fn parse(raw: &str) -> Result<Self> {
        let url = Url::parse(raw).map_err(|_| Error::Config("invalid proxy URL".into()))?;
        if !matches!(url.scheme(), "http" | "https" | "socks5" | "socks5h")
            || url.query().is_some()
            || url.fragment().is_some()
            || !matches!(url.path(), "" | "/")
        {
            return Err(Error::Config("unsupported proxy URL".into()));
        }
        let decode = |v: &str| {
            percent_encoding::percent_decode_str(v)
                .decode_utf8()
                .map(|s| s.into_owned())
                .map_err(|_| Error::Config("invalid proxy credentials".into()))
        };
        let username = decode(url.username())?;
        let password = decode(url.password().unwrap_or(""))?;
        if username
            .bytes()
            .chain(password.bytes())
            .any(|c| c.is_ascii_control())
        {
            return Err(Error::Config("invalid proxy credentials".into()));
        }
        if username.contains(':') {
            return Err(Error::Config("proxy username cannot contain ':'".into()));
        }
        Ok(Self {
            host: url
                .host_str()
                .ok_or_else(|| Error::Config("proxy host is required".into()))?
                .trim_matches(['[', ']'])
                .into(),
            port: url.port().unwrap_or(match url.scheme() {
                "http" => 80,
                "https" => 443,
                _ => 1080,
            }),
            scheme: url.scheme().into(),
            username,
            password,
            exempt_host: false,
        })
    }
}

pub(super) async fn connect(
    proxy: &Proxy,
    proxy_ips: &[IpAddr],
    targets: &[IpAddr],
    port: u16,
    deadline: Instant,
) -> Result<BoxStream> {
    if proxy_ips.is_empty() {
        return Err(Error::Network("no validated proxy address".into()));
    }
    if targets.is_empty() {
        return Err(Error::Network("no validated target address".into()));
    }
    let mut last = None;
    for &proxy_ip in proxy_ips {
        for &target in targets {
            match timeout_at(deadline, connect_one(proxy, proxy_ip, target, port)).await {
                Ok(Ok(io)) => return Ok(io),
                Ok(Err(e @ Error::Tls { .. })) => return Err(e),
                Ok(Err(e)) => last = Some(e),
                Err(_) => return Err(Error::Timeout(TimeoutPhase::Connect)),
            }
        }
    }
    Err(last.unwrap_or_else(|| Error::Network("proxy tunnel failed".into())))
}

async fn connect_one(
    proxy: &Proxy,
    proxy_ip: IpAddr,
    target: IpAddr,
    port: u16,
) -> Result<BoxStream> {
    let tcp = TcpStream::connect(std::net::SocketAddr::new(proxy_ip, proxy.port))
        .await
        .map_err(|_| Error::Network("proxy connection failed".into()))?;
    let _ = tcp.set_nodelay(true);
    let mut io: BoxStream = if proxy.scheme == "https" {
        let mut tls = (*super::send::tls_config()?).clone();
        tls.alpn_protocols = vec![b"http/1.1".to_vec()];
        let name = ServerName::try_from(proxy.host.clone())
            .map_err(|_| Error::Config("invalid proxy host".into()))?;
        let stream = TlsConnector::from(std::sync::Arc::new(tls))
            .connect(name, tcp)
            .await
            .map_err(|e| crate::tls::from_io(&e))?;
        Box::new(stream)
    } else {
        Box::new(tcp)
    };
    if matches!(proxy.scheme.as_str(), "http" | "https") {
        http_connect(&mut io, proxy, target, port).await?;
    } else {
        socks_connect(&mut io, proxy, target, port).await?;
    }
    Ok(io)
}

fn io_error(_: std::io::Error) -> Error {
    Error::Network("proxy tunnel failed".into())
}

async fn http_connect(io: &mut BoxStream, proxy: &Proxy, ip: IpAddr, port: u16) -> Result<()> {
    let authority = std::net::SocketAddr::new(ip, port).to_string();
    let auth = if proxy.username.is_empty() && proxy.password.is_empty() {
        String::new()
    } else {
        let credentials = format!("{}:{}", proxy.username, proxy.password);
        let value = pagesift_base64::STANDARD
            .encode_to_string(credentials.as_bytes())
            .map_err(|_| Error::Network("proxy credentials too large".into()))?;
        format!("Proxy-Authorization: Basic {value}\r\n")
    };
    let request = format!("CONNECT {authority} HTTP/1.1\r\nHost: {authority}\r\n{auth}\r\n");
    io.write_all(request.as_bytes()).await.map_err(io_error)?;
    let mut head = Vec::new();
    while !head.ends_with(b"\r\n\r\n") {
        if head.len() >= 16384 {
            return Err(Error::Network("proxy response too large".into()));
        }
        head.push(io.read_u8().await.map_err(io_error)?);
    }
    let first = head.split(|&b| b == b'\n').next().unwrap_or_default();
    let status = std::str::from_utf8(first).ok().and_then(|s| {
        let mut fields = s.split_whitespace();
        let protocol = fields.next()?;
        if !matches!(protocol, "HTTP/1.0" | "HTTP/1.1") {
            return None;
        }
        fields.next()?.parse::<u16>().ok()
    });
    if !matches!(status, Some(200..=299)) {
        return Err(Error::Network("proxy CONNECT rejected".into()));
    }
    Ok(())
}

async fn socks_connect(io: &mut BoxStream, proxy: &Proxy, ip: IpAddr, port: u16) -> Result<()> {
    let authenticated = !proxy.username.is_empty() || !proxy.password.is_empty();
    io.write_all(if authenticated {
        &[5, 1, 2]
    } else {
        &[5, 1, 0]
    })
    .await
    .map_err(io_error)?;
    let mut choice = [0u8; 2];
    io.read_exact(&mut choice).await.map_err(io_error)?;
    if choice != [5, if authenticated { 2 } else { 0 }] {
        return Err(Error::Network("SOCKS authentication rejected".into()));
    }
    if authenticated {
        if proxy.username.is_empty() || proxy.username.len() > 255 || proxy.password.len() > 255 {
            return Err(Error::Config("SOCKS credentials too long".into()));
        }
        let mut auth = vec![1, proxy.username.len() as u8];
        auth.extend_from_slice(proxy.username.as_bytes());
        auth.push(proxy.password.len() as u8);
        auth.extend_from_slice(proxy.password.as_bytes());
        io.write_all(&auth).await.map_err(io_error)?;
        io.read_exact(&mut choice).await.map_err(io_error)?;
        if choice != [1, 0] {
            return Err(Error::Network("SOCKS authentication rejected".into()));
        }
    }
    let mut req = vec![5, 1, 0];
    match ip {
        IpAddr::V4(ip) => {
            req.push(1);
            req.extend_from_slice(&ip.octets());
        }
        IpAddr::V6(ip) => {
            req.push(4);
            req.extend_from_slice(&ip.octets());
        }
    }
    req.extend_from_slice(&port.to_be_bytes());
    io.write_all(&req).await.map_err(io_error)?;
    let mut head = [0u8; 4];
    io.read_exact(&mut head).await.map_err(io_error)?;
    if head[0] != 5 || head[1] != 0 || head[2] != 0 {
        return Err(Error::Network("SOCKS CONNECT rejected".into()));
    }
    let len = match head[3] {
        1 => 4,
        4 => 16,
        3 => usize::from(io.read_u8().await.map_err(io_error)?),
        _ => return Err(Error::Network("invalid SOCKS response".into())),
    };
    let mut rest = vec![0; len + 2];
    io.read_exact(&mut rest).await.map_err(io_error)?;
    Ok(())
}
