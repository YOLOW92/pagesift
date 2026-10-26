//! TLS 证书错误的分类：把 rustls 的校验失败原因映射成稳定的类别，
//! 让上层（错误码、重试策略、排障输出）能区分永久性证书问题和可重试的网络抖动。

use tokio_rustls::rustls::{CertificateError, Error as RustlsError};

use crate::fetch::Error;

/// 证书校验失败的类别。`Other` 覆盖握手协议错误等非证书问题。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum TlsErrorKind {
    /// 证书已过期或尚未生效。
    Expired,
    /// 证书里的主机名和请求的域名不一致。
    HostnameMismatch,
    /// 证书链的根证书不受信任（自签名、缺中间证书等）。
    UntrustedRoot,
    /// 其他 TLS 握手/证书错误。
    Other,
}

impl TlsErrorKind {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Expired => "expired",
            Self::HostnameMismatch => "hostname mismatch",
            Self::UntrustedRoot => "untrusted root",
            Self::Other => "other TLS error",
        }
    }
}

impl std::fmt::Display for TlsErrorKind {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(self.as_str())
    }
}

pub(crate) fn classify(error: &RustlsError) -> TlsErrorKind {
    let RustlsError::InvalidCertificate(cert) = error else {
        return TlsErrorKind::Other;
    };
    match cert {
        CertificateError::Expired
        | CertificateError::ExpiredContext { .. }
        | CertificateError::NotValidYet
        | CertificateError::NotValidYetContext { .. } => TlsErrorKind::Expired,
        CertificateError::NotValidForName | CertificateError::NotValidForNameContext { .. } => {
            TlsErrorKind::HostnameMismatch
        }
        CertificateError::UnknownIssuer => TlsErrorKind::UntrustedRoot,
        _ => TlsErrorKind::Other,
    }
}

/// 某些代理/reqwest 路径把 rustls 错误包成普通文本，类型链里已找不到原始 `rustls::Error`；
/// 这时按保留的证书原因文本兜底分类。
pub(crate) fn classify_text(text: &str) -> Option<TlsErrorKind> {
    let lower = text.to_ascii_lowercase();
    let certificate = lower.contains("invalid peer certificate")
        || lower.contains("certificate expired")
        || lower.contains("certificate is not valid")
        || lower.contains("unknownissuer");
    if !certificate {
        return None;
    }
    if lower.contains("expired") || lower.contains("not valid yet") || lower.contains("notvalidyet")
    {
        Some(TlsErrorKind::Expired)
    } else if lower.contains("not valid for name")
        || lower.contains("notvalidforname")
        || lower.contains("does not include the expected name")
    {
        Some(TlsErrorKind::HostnameMismatch)
    } else if lower.contains("unknownissuer")
        || lower.contains("unable to get local issuer")
        || lower.contains("self-signed")
    {
        Some(TlsErrorKind::UntrustedRoot)
    } else {
        Some(TlsErrorKind::Other)
    }
}

/// tokio-rustls 的握手错误是 `io::Error`，证书原因藏在 `get_ref()` 里。
pub(crate) fn from_io(error: &std::io::Error) -> Error {
    let kind = error
        .get_ref()
        .and_then(|e| e.downcast_ref::<RustlsError>())
        .map(classify)
        .unwrap_or(TlsErrorKind::Other);
    Error::Tls {
        kind,
        message: error.to_string(),
    }
}

/// reqwest 的错误链里找 rustls 错误；找到时返回类别和完整的错误链文本。
pub(crate) fn from_sources(
    error: &(dyn std::error::Error + 'static),
) -> Option<(TlsErrorKind, String)> {
    let mut text = error.to_string();
    let mut source = error.source();
    let mut kind = None;
    while let Some(s) = source {
        text.push_str(": ");
        text.push_str(&s.to_string());
        if kind.is_none()
            && let Some(rustls) = s.downcast_ref::<RustlsError>()
        {
            kind = Some(classify(rustls));
        }
        source = s.source();
    }
    kind.map(|kind| (kind, text))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn classifies_certificate_failures() {
        for (error, kind) in [
            (
                RustlsError::InvalidCertificate(CertificateError::Expired),
                TlsErrorKind::Expired,
            ),
            (
                RustlsError::InvalidCertificate(CertificateError::NotValidYet),
                TlsErrorKind::Expired,
            ),
            (
                RustlsError::InvalidCertificate(CertificateError::NotValidForName),
                TlsErrorKind::HostnameMismatch,
            ),
            (
                RustlsError::InvalidCertificate(CertificateError::UnknownIssuer),
                TlsErrorKind::UntrustedRoot,
            ),
            (
                RustlsError::InvalidCertificate(CertificateError::BadEncoding),
                TlsErrorKind::Other,
            ),
            (
                RustlsError::PeerIncompatible(
                    tokio_rustls::rustls::PeerIncompatible::Tls12NotOffered,
                ),
                TlsErrorKind::Other,
            ),
        ] {
            assert_eq!(classify(&error), kind, "{error:?}");
        }
    }

    #[test]
    fn classifies_certificate_text_from_proxy_errors() {
        for (text, kind) in [
            (
                "error sending request: invalid peer certificate: certificate expired",
                TlsErrorKind::Expired,
            ),
            (
                "client error (Connect): invalid peer certificate: certificate not valid for name \"a\"; only valid for \"*.b\"",
                TlsErrorKind::HostnameMismatch,
            ),
            (
                "invalid peer certificate: UnknownIssuer",
                TlsErrorKind::UntrustedRoot,
            ),
            ("connection reset by peer", TlsErrorKind::Other),
        ] {
            if text.contains("reset") {
                assert_eq!(classify_text(text), None);
            } else {
                assert_eq!(classify_text(text), Some(kind), "{text}");
            }
        }
    }

    #[test]
    fn io_errors_keep_the_certificate_reason() {
        let inner = RustlsError::InvalidCertificate(CertificateError::Expired);
        let io = std::io::Error::new(std::io::ErrorKind::InvalidData, inner);
        match from_io(&io) {
            Error::Tls { kind, message } => {
                assert_eq!(kind, TlsErrorKind::Expired);
                assert!(
                    message.to_ascii_lowercase().contains("expired"),
                    "{message}"
                );
            }
            other => panic!("unexpected error: {other:?}"),
        }
    }
}
