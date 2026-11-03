//! TLS 与可选的双向 TLS（rustls + ring，ALPN `h2`）。
//!
//! 证书与私钥由部署者提供（PEM 解析在 CLI 一层做）；双向 TLS 时网关用部署者给的 CA 校验工人证书。
//! 令牌认证在 TLS 之上照常进行，两者是叠加关系。

use std::sync::Arc;

use tokio::io::{AsyncRead, AsyncWrite};
use tokio_rustls::rustls::pki_types::{CertificateDer, PrivateKeyDer, ServerName};
use tokio_rustls::rustls::server::WebPkiClientVerifier;
use tokio_rustls::rustls::{self, ClientConfig, RootCertStore, ServerConfig};
use tokio_rustls::{TlsAcceptor, TlsConnector, client, server};

use crate::types::Error;

/// [协议] ALPN 协议名 "h2"（RFC 9113 §3.2，RFC 7301）
const ALPN_H2: &[u8] = b"h2";

fn provider() -> Arc<rustls::crypto::CryptoProvider> {
    Arc::new(rustls::crypto::ring::default_provider())
}

fn tls_err(e: impl std::fmt::Display) -> Error {
    Error::Transport(format!("tls: {e}"))
}

/// 网关的 TLS 配置。`client_ca` 给出时要求并校验工人证书（双向 TLS）。
pub fn server_config(
    chain: Vec<CertificateDer<'static>>,
    key: PrivateKeyDer<'static>,
    client_ca: Option<RootCertStore>,
) -> Result<Arc<ServerConfig>, Error> {
    let b = ServerConfig::builder_with_provider(provider())
        .with_safe_default_protocol_versions()
        .map_err(tls_err)?;
    let b = match client_ca {
        Some(roots) => {
            let v = WebPkiClientVerifier::builder_with_provider(Arc::new(roots), provider())
                .build()
                .map_err(tls_err)?;
            b.with_client_cert_verifier(v)
        }
        None => b.with_no_client_auth(),
    };
    let mut c = b.with_single_cert(chain, key).map_err(tls_err)?;
    c.alpn_protocols = vec![ALPN_H2.to_vec()];
    Ok(Arc::new(c))
}

/// 工人的 TLS 配置。`identity` 给出时出示客户端证书（双向 TLS）。
pub fn client_config(
    roots: RootCertStore,
    identity: Option<(Vec<CertificateDer<'static>>, PrivateKeyDer<'static>)>,
) -> Result<Arc<ClientConfig>, Error> {
    let b = ClientConfig::builder_with_provider(provider())
        .with_safe_default_protocol_versions()
        .map_err(tls_err)?
        .with_root_certificates(roots);
    let mut c = match identity {
        Some((chain, key)) => b.with_client_auth_cert(chain, key).map_err(tls_err)?,
        None => b.with_no_client_auth(),
    };
    c.alpn_protocols = vec![ALPN_H2.to_vec()];
    Ok(Arc::new(c))
}

/// 网关一侧：完成 TLS 握手，并确认协商到了 `h2`。
pub async fn accept<IO>(cfg: Arc<ServerConfig>, io: IO) -> Result<server::TlsStream<IO>, Error>
where
    IO: AsyncRead + AsyncWrite + Unpin,
{
    let s = TlsAcceptor::from(cfg).accept(io).await.map_err(tls_err)?;
    if s.get_ref().1.alpn_protocol() != Some(ALPN_H2) {
        return Err(tls_err("peer did not negotiate h2"));
    }
    Ok(s)
}

/// 工人一侧：连接网关并校验其证书（`server_name` 是证书上的 DNS 名或 IP）。
pub async fn connect<IO>(
    cfg: Arc<ClientConfig>,
    server_name: &str,
    io: IO,
) -> Result<client::TlsStream<IO>, Error>
where
    IO: AsyncRead + AsyncWrite + Unpin,
{
    let name = ServerName::try_from(server_name.to_owned()).map_err(tls_err)?;
    let s = TlsConnector::from(cfg)
        .connect(name, io)
        .await
        .map_err(tls_err)?;
    if s.get_ref().1.alpn_protocol() != Some(ALPN_H2) {
        return Err(tls_err("gateway did not negotiate h2"));
    }
    Ok(s)
}
