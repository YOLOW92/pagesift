//! 集群连接的承载：TCP，加 TLS（rustls，默认）或显式声明的 h2c；证书从 PEM 文件读。

use std::path::Path;
use std::sync::Arc;

use anyhow::{Context, Result, bail};
use pagesift_cluster::{Dial, WorkerId};
use tokio::io::{AsyncRead, AsyncWrite};
use tokio_rustls::rustls::pki_types::pem::PemObject;
use tokio_rustls::rustls::pki_types::{CertificateDer, PrivateKeyDer};
use tokio_rustls::rustls::{ClientConfig, RootCertStore, ServerConfig};

/// 工人（或温备）连网关用的身份与传输设置。
#[derive(Clone)]
pub struct Link {
    pub id: WorkerId,
    pub token: String,
    /// `None` 为 h2c（只在部署者显式声明可信的内网）。
    pub tls: Option<ClientTls>,
}

#[derive(Clone)]
pub struct ClientTls {
    pub config: Arc<ClientConfig>,
    /// 网关证书上的名字；不设时取地址的主机部分。
    pub server_name: Option<String>,
}

impl Link {
    pub fn dial(&self, addr: &str) -> Dial {
        Dial {
            worker: self.id.clone(),
            token: self.token.clone(),
            authority: addr.to_owned(),
            tls: self.tls.is_some(),
            max_frame: pagesift_cluster::transport::default_max_frame(),
        }
    }
}

pub trait Io: AsyncRead + AsyncWrite + Unpin + Send {}
impl<T: AsyncRead + AsyncWrite + Unpin + Send> Io for T {}

/// `host:port` 的主机部分（IPv6 去掉方括号）。
fn host_of(addr: &str) -> &str {
    let host = addr.rsplit_once(':').map_or(addr, |(h, _)| h);
    host.trim_start_matches('[').trim_end_matches(']')
}

/// 连到网关，按设置做 TLS 握手。
pub async fn dial(addr: &str, link: &Link) -> Result<Box<dyn Io>> {
    let tcp = tokio::net::TcpStream::connect(addr)
        .await
        .with_context(|| format!("connecting to {addr}"))?;
    let _ = tcp.set_nodelay(true);
    Ok(match &link.tls {
        None => Box::new(tcp),
        Some(t) => {
            let name = t.server_name.as_deref().unwrap_or_else(|| host_of(addr));
            Box::new(pagesift_cluster::transport::tls::connect(t.config.clone(), name, tcp).await?)
        }
    })
}

fn certs(path: &Path) -> Result<Vec<CertificateDer<'static>>> {
    let pem = std::fs::read(path).with_context(|| format!("reading {}", path.display()))?;
    let certs = CertificateDer::pem_slice_iter(&pem)
        .collect::<Result<Vec<_>, _>>()
        .with_context(|| format!("parsing certificates in {}", path.display()))?;
    if certs.is_empty() {
        bail!("no certificate in {}", path.display());
    }
    Ok(certs)
}

fn key(path: &Path) -> Result<PrivateKeyDer<'static>> {
    let pem = std::fs::read(path).with_context(|| format!("reading {}", path.display()))?;
    PrivateKeyDer::from_pem_slice(&pem)
        .with_context(|| format!("parsing the private key in {}", path.display()))
}

fn roots(path: &Path) -> Result<RootCertStore> {
    let mut r = RootCertStore::empty();
    for c in certs(path)? {
        r.add(c)
            .with_context(|| format!("adding a CA certificate from {}", path.display()))?;
    }
    Ok(r)
}

/// 网关的 TLS：证书链与私钥；`client_ca` 给出时要求工人出示由它签发的证书（双向 TLS）。
pub fn server_tls(
    cert: &Path,
    key_path: &Path,
    client_ca: Option<&Path>,
) -> Result<Arc<ServerConfig>> {
    let ca = client_ca.map(roots).transpose()?;
    Ok(pagesift_cluster::transport::tls::server_config(
        certs(cert)?,
        key(key_path)?,
        ca,
    )?)
}

/// 工人的 TLS：`ca` 是校验网关证书的根（不给时用系统根证书）；`identity` 是双向 TLS 的客户端证书与私钥。
pub fn client_tls(
    ca: Option<&Path>,
    identity: Option<(&Path, &Path)>,
) -> Result<Arc<ClientConfig>> {
    let roots = match ca {
        Some(p) => roots(p)?,
        None => {
            let mut r = RootCertStore::empty();
            for c in rustls_native_certs::load_native_certs().certs {
                let _ = r.add(c);
            }
            if r.is_empty() {
                bail!("no system root certificates found; set [cluster.tls] ca");
            }
            r
        }
    };
    let identity = identity
        .map(|(c, k)| Ok::<_, anyhow::Error>((certs(c)?, key(k)?)))
        .transpose()?;
    Ok(pagesift_cluster::transport::tls::client_config(
        roots, identity,
    )?)
}

#[cfg(test)]
mod tests {
    #[test]
    fn host_part_of_an_address() {
        assert_eq!(super::host_of("gw.example:7700"), "gw.example");
        assert_eq!(super::host_of("[::1]:7700"), "::1");
        assert_eq!(super::host_of("gw"), "gw");
    }
}
