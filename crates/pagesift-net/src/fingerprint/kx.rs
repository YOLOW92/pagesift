//! X25519MLKEM768（draft-ietf-tls-ecdhe-mlkem）：X25519 用 ring，ML-KEM-768 用 [`super::mlkem`]。

use pagesift_rustls::crypto::ring::default_provider;
use pagesift_rustls::crypto::ring::kx_group::X25519;
use pagesift_rustls::crypto::{ActiveKeyExchange, SharedSecret, SupportedKxGroup};
use pagesift_rustls::{Error, NamedGroup, PeerMisbehaved, ProtocolVersion};
use zeroize::Zeroize;

use super::mlkem::{CT_BYTES, DK_BYTES, decapsulate, keygen};

/// [协议] draft-ietf-tls-ecdhe-mlkem §6（IANA TLS Supported Groups）。
const GROUP: u16 = 0x11ec;
/// [协议] X25519 公钥长度（RFC 7748）。
const X25519_BYTES: usize = 32;

#[derive(Debug)]
pub(super) struct X25519MlKem768;

struct Active {
    x25519: Box<dyn ActiveKeyExchange>,
    dk: Box<[u8; DK_BYTES]>,
    /// `ek || X25519`（ML-KEM 在前，draft §3.1）。
    public: Vec<u8>,
}

impl Drop for Active {
    fn drop(&mut self) {
        self.dk.zeroize();
    }
}

impl SupportedKxGroup for X25519MlKem768 {
    fn start(&self) -> Result<Box<dyn ActiveKeyExchange>, Error> {
        let x25519 = X25519.start()?;
        let mut seed = [0; 64]; // [协议] FIPS 203、RFC 7748 的长度
        default_provider()
            .secure_random
            .fill(&mut seed)
            .map_err(|_| Error::FailedToGetRandomBytes)?;
        let (d, z) = seed.split_at(32); // [协议] FIPS 203、RFC 7748 的长度
        // [协议] FIPS 203、RFC 7748 的长度
        let (d, z): (&[u8; 32], &[u8; 32]) = (
            d.try_into().map_err(|_| Error::FailedToGetRandomBytes)?,
            z.try_into().map_err(|_| Error::FailedToGetRandomBytes)?,
        );
        let (ek, dk) = keygen(d, z);
        seed.zeroize();
        let mut public = Vec::with_capacity(ek.len() + X25519_BYTES);
        public.extend_from_slice(&ek[..]);
        public.extend_from_slice(x25519.pub_key());
        Ok(Box::new(Active { x25519, dk, public }))
    }

    fn name(&self) -> NamedGroup {
        NamedGroup::from(GROUP)
    }

    /// 只用于 TLS 1.3。
    fn usable_for_version(&self, version: ProtocolVersion) -> bool {
        version == ProtocolVersion::TLSv1_3
    }
}

impl ActiveKeyExchange for Active {
    fn complete(mut self: Box<Self>, peer: &[u8]) -> Result<SharedSecret, Error> {
        if peer.len() != CT_BYTES + X25519_BYTES {
            return Err(PeerMisbehaved::InvalidKeyShare.into());
        }
        let (ct, x25519_peer) = peer.split_at(CT_BYTES);
        let ct: &[u8; CT_BYTES] = ct
            .try_into()
            .map_err(|_| Error::from(PeerMisbehaved::InvalidKeyShare))?;
        let mut mlkem = decapsulate(&self.dk, ct);
        // Active 实现了 Drop，不能把字段移出来，换一个占位
        let x25519 = std::mem::replace(&mut self.x25519, Box::new(Spent)).complete(x25519_peer)?;
        // [协议] 共享密钥：ML-KEM 在前（draft §3.2）
        let mut secret = Vec::with_capacity(mlkem.len() + x25519.secret_bytes().len());
        secret.extend_from_slice(&mlkem);
        secret.extend_from_slice(x25519.secret_bytes());
        mlkem.zeroize();
        Ok(SharedSecret::from(secret))
    }

    fn pub_key(&self) -> &[u8] {
        &self.public
    }

    fn group(&self) -> NamedGroup {
        NamedGroup::from(GROUP)
    }

    /// 同一个 X25519 钥匙也作为单独的份额发出去，服务器选 X25519 时用它（和 Chrome 相同）。
    fn hybrid_component(&self) -> Option<(NamedGroup, &[u8])> {
        Some((self.x25519.group(), self.x25519.pub_key()))
    }

    fn complete_hybrid_component(mut self: Box<Self>, peer: &[u8]) -> Result<SharedSecret, Error> {
        let taken = std::mem::replace(&mut self.x25519, Box::new(Spent));
        taken.complete(peer)
    }
}

/// `Active::x25519` 被取走之后的占位：`complete` 消费整个 `Active`，这个值不会再被用到。
struct Spent;

impl ActiveKeyExchange for Spent {
    fn complete(self: Box<Self>, _peer: &[u8]) -> Result<SharedSecret, Error> {
        Err(Error::General("key exchange already completed".into()))
    }

    fn pub_key(&self) -> &[u8] {
        &[]
    }

    fn group(&self) -> NamedGroup {
        NamedGroup::from(GROUP)
    }
}
