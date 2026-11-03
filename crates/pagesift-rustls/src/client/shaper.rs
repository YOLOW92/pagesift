//! pagesift addition (not in upstream rustls): lets the caller write the ClientHello bytes.
//!
//! rustls still chooses the random, session id and key shares and keeps the secrets. The shaper
//! only decides how they are laid out. The bytes it returns go on the wire and into the
//! transcript, so the server's signature and Finished are checked against what was really sent.

use alloc::vec::Vec;
use core::fmt::Debug;

use crate::error::Error;

/// What rustls has decided for this ClientHello.
#[derive(Debug)]
pub struct HelloInput<'a> {
    /// `client_random`; the same for both hellos when the server sends a HelloRetryRequest.
    pub random: &'a [u8; 32],
    /// `legacy_session_id`.
    pub session_id: &'a [u8],
    /// The DNS name to put in SNI, or `None` for an IP address.
    pub server_name: Option<&'a str>,
    /// `(group, public key)`, in the order rustls would have sent them.
    pub key_shares: &'a [(u16, &'a [u8])],
    /// The HelloRetryRequest cookie to echo.
    pub cookie: Option<&'a [u8]>,
    /// This is the second ClientHello, after a HelloRetryRequest.
    pub retry: bool,
}

/// The shaper's output.
#[derive(Debug)]
pub struct ShapedHello {
    /// The ClientHello body, without the four-byte handshake header.
    pub body: Vec<u8>,
    /// Extension types the body offers beyond the ones rustls knows. The server may answer these.
    pub offered_extensions: Vec<u16>,
}

/// Writes the ClientHello.
pub trait HelloShaper: Debug + Send + Sync {
    /// Lays out one ClientHello.
    fn shape(&self, input: &HelloInput<'_>) -> Result<ShapedHello, Error>;

    /// The extension type for application settings (ALPS), if the body offers it.
    fn application_settings_extension(&self) -> Option<u16>;

    /// The client's settings for a negotiated ALPN protocol, sent when the server negotiated ALPS.
    fn application_settings(&self, protocol: &[u8]) -> Vec<u8>;
}
