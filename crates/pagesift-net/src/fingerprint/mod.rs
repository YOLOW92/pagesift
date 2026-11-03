//! 可选的浏览器指纹模拟（默认关）：开启后 HTTP 抓取的 ClientHello 和 HTTP/2 连接特征与档案里的浏览器一致。
//!
//! - ClientHello 由 [`hello`] 写，握手和证书校验是 `pagesift-rustls`（rustls 加一个钩子）；
//! - HTTP/2 的 SETTINGS、WINDOW_UPDATE 用 hyper 的 Builder 设，伪头部顺序和优先级在 [`shape`]；
//! - 默认路径不经过这里，继续用原版 rustls。
//!
//! 服务器选了只声明没实现的算法（见 [`profile`]）时明确失败，这个主机退回普通连接并报告。
//! 证书错误不退回。不管哪种模式，robots 和限速都在上一层照常生效。

use std::sync::Arc;

use pagesift_rustls::client::Resumption;
use pagesift_rustls::compress::{CertDecompressor, DecompressionFailed};
use pagesift_rustls::crypto::CryptoProvider;
use pagesift_rustls::crypto::ring::{self, cipher_suite, kx_group};
use pagesift_rustls::{
    CertificateCompressionAlgorithm, ClientConfig, Error as TlsError, PeerMisbehaved,
    RootCertStore, SupportedCipherSuite,
};

mod hello;
mod keccak;
mod kx;
mod mlkem;
mod profile;
mod shape;
mod stream;

pub use profile::FingerprintProfile;
pub(crate) use shape::ShapedIo;
pub(crate) use stream::TlsStream;

use crate::fetch::{Error, Result};
use crate::tls::{TlsErrorKind, classify_text};

/// 档案里真正实现的套件，顺序同线上。其余（CBC、静态 RSA）只声明。
// [协议] 档案实现的套件数、32 字节种子
const IMPLEMENTED_SUITES: [SupportedCipherSuite; 9] = [
    cipher_suite::TLS13_AES_128_GCM_SHA256,
    cipher_suite::TLS13_AES_256_GCM_SHA384,
    cipher_suite::TLS13_CHACHA20_POLY1305_SHA256,
    cipher_suite::TLS_ECDHE_ECDSA_WITH_AES_128_GCM_SHA256,
    cipher_suite::TLS_ECDHE_RSA_WITH_AES_128_GCM_SHA256,
    cipher_suite::TLS_ECDHE_ECDSA_WITH_AES_256_GCM_SHA384,
    cipher_suite::TLS_ECDHE_RSA_WITH_AES_256_GCM_SHA384,
    cipher_suite::TLS_ECDHE_ECDSA_WITH_CHACHA20_POLY1305_SHA256,
    cipher_suite::TLS_ECDHE_RSA_WITH_CHACHA20_POLY1305_SHA256,
];

/// [协议] RFC 8879 §7：证书压缩解压；Chrome 只声明 brotli。
#[derive(Debug)]
struct BrotliDecompressor;

impl CertDecompressor for BrotliDecompressor {
    fn decompress(
        &self,
        input: &[u8],
        output: &mut [u8],
    ) -> std::result::Result<(), DecompressionFailed> {
        use std::io::Read;
        // [协议] brotli-decompressor 的内部缓冲，不影响结果
        const BUFFER: usize = 4096;
        let mut reader = brotli_decompressor::Decompressor::new(input, BUFFER);
        reader.read_exact(output).map_err(|_| DecompressionFailed)?;
        // 声明的长度必须正好：还能多读出来就是坏数据（trait 约定）
        let mut extra = [0];
        match reader.read(&mut extra) {
            Ok(0) => Ok(()),
            _ => Err(DecompressionFailed),
        }
    }

    fn algorithm(&self) -> CertificateCompressionAlgorithm {
        CertificateCompressionAlgorithm::Brotli
    }
}

static BROTLI: BrotliDecompressor = BrotliDecompressor;
static HYBRID: kx::X25519MlKem768 = kx::X25519MlKem768;

/// 进程内只有一个档案就够（配置决定），和默认路径一样只读一遍系统根证书。
fn native_roots() -> Result<RootCertStore> {
    let mut roots = RootCertStore::empty();
    for cert in rustls_native_certs::load_native_certs().certs {
        // 个别系统证书格式不对时跳过，不影响其他证书
        let _ = roots.add(cert);
    }
    if roots.is_empty() {
        return Err(Error::Config("no system root certificates found".into()));
    }
    Ok(roots)
}

/// 档案对应的 TLS 配置。`roots` 留给测试换成自己的 CA；证书校验就是 rustls 的 WebPKI 校验，没有开关。
pub(crate) fn tls_config(
    profile: FingerprintProfile,
    roots: Option<RootCertStore>,
) -> Result<Arc<ClientConfig>> {
    let roots = match roots {
        Some(roots) => roots,
        None => native_roots()?,
    };
    let base = ring::default_provider();
    let secure_random = base.secure_random;
    let provider = CryptoProvider {
        cipher_suites: IMPLEMENTED_SUITES.to_vec(),
        kx_groups: vec![
            &HYBRID,
            kx_group::X25519,
            kx_group::SECP256R1,
            kx_group::SECP384R1,
        ],
        ..base
    };
    let mut secret = [0; 32]; // [协议] 档案实现的套件数、32 字节种子
    secure_random
        .fill(&mut secret)
        .map_err(|_| Error::Config("no system randomness".into()))?;
    let mut config = ClientConfig::builder_with_provider(Arc::new(provider))
        .with_safe_default_protocol_versions()
        .map_err(|e| Error::Config(e.to_string()))?
        .with_root_certificates(roots)
        .with_no_client_auth();
    // ALPN 由档案的字节声明；这里的列表让 rustls 认可服务器的选择
    config.alpn_protocols = vec![b"h2".to_vec(), b"http/1.1".to_vec()];
    config.resumption = Resumption::disabled();
    config.cert_decompressors = vec![&BROTLI];
    config.hello_shaper = Some(Arc::new(hello::ChromeShaper::new(profile, secret)));
    Ok(Arc::new(config))
}

/// 握手失败是不是因为服务器选了只声明没实现的东西（这时才退回普通连接）。
pub(crate) fn is_unimplemented_choice(error: &std::io::Error) -> bool {
    error
        .get_ref()
        .and_then(|e| e.downcast_ref::<TlsError>())
        .is_some_and(|e| {
            matches!(
                e,
                TlsError::PeerMisbehaved(PeerMisbehaved::SelectedUnofferedCipherSuite)
            )
        })
}

/// 和 [`crate::tls::from_io`] 同样的分类，错误类型是 fork 里的。
pub(crate) fn classify_io(error: &std::io::Error) -> Error {
    use pagesift_rustls::CertificateError as Cert;
    let kind = match error.get_ref().and_then(|e| e.downcast_ref::<TlsError>()) {
        Some(TlsError::InvalidCertificate(cert)) => match cert {
            Cert::Expired
            | Cert::ExpiredContext { .. }
            | Cert::NotValidYet
            | Cert::NotValidYetContext { .. } => TlsErrorKind::Expired,
            Cert::NotValidForName | Cert::NotValidForNameContext { .. } => {
                TlsErrorKind::HostnameMismatch
            }
            Cert::UnknownIssuer => TlsErrorKind::UntrustedRoot,
            _ => TlsErrorKind::Other,
        },
        _ => classify_text(&error.to_string()).unwrap_or(TlsErrorKind::Other),
    };
    Error::Tls {
        kind,
        message: error.to_string(),
    }
}

/// 按档案填请求头：顺序就是线上的顺序。调用方的头（凭证、条件请求、Cookie）替换同名项，其余排在档案之后。
pub(crate) fn fill_request_headers(
    profile: FingerprintProfile,
    out: &mut hyper::HeaderMap,
    http1_authority: Option<&str>,
    caller: &hyper::HeaderMap,
) -> Result<()> {
    use hyper::header::{self, HeaderName, HeaderValue};
    let data = profile.data();
    if let Some(authority) = http1_authority {
        let host = HeaderValue::from_str(authority).map_err(|_| Error::Header("host".into()))?;
        out.insert(header::HOST, host);
        out.insert(header::CONNECTION, HeaderValue::from_static("keep-alive"));
    }
    for (name, value) in data.request_headers {
        out.insert(
            HeaderName::from_static(name),
            HeaderValue::from_static(value),
        );
    }
    for (name, value) in caller {
        out.insert(name, value.clone());
    }
    if http1_authority.is_none() {
        for (name, value) in data.http2_only_headers {
            let name = HeaderName::from_static(name);
            if !out.contains_key(&name) {
                out.insert(name, HeaderValue::from_static(value));
            }
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests;
