//! Dual cryptographic checksum calculator for WARC records (SHA-256 and BLAKE3).
//! Computes ISO 28500 standard `sha256:<hex>` digests alongside high-speed `blake3:<hex>` digests.

use sha2::{Digest, Sha256};

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct WarcDigests {
    pub sha256_hex: String,
    pub blake3_hex: String,
}

impl WarcDigests {
    /// Format as ISO 28500 standard digest string: `sha256:<hex>`.
    pub fn to_warc_header(&self) -> String {
        format!("sha256:{}", self.sha256_hex)
    }

    /// Format as BLAKE3 digest string: `blake3:<hex>`.
    pub fn to_blake3_header(&self) -> String {
        format!("blake3:{}", self.blake3_hex)
    }
}

pub struct DigestCalculator {
    sha256: Sha256,
    blake3: blake3::Hasher,
}

impl Default for DigestCalculator {
    fn default() -> Self {
        Self::new()
    }
}

impl DigestCalculator {
    pub fn new() -> Self {
        Self {
            sha256: Sha256::new(),
            blake3: blake3::Hasher::new(),
        }
    }

    pub fn update(&mut self, data: &[u8]) {
        self.sha256.update(data);
        self.blake3.update(data);
    }

    pub fn finalize(self) -> WarcDigests {
        let sha_res = self.sha256.finalize();
        let blake_res = self.blake3.finalize();

        let mut sha256_hex = String::with_capacity(64);
        for b in sha_res {
            use std::fmt::Write;
            let _ = write!(sha256_hex, "{:02x}", b);
        }

        WarcDigests {
            sha256_hex,
            blake3_hex: blake_res.to_hex().to_string(),
        }
    }

    /// Convenience one-shot digest calculation.
    pub fn digest_bytes(data: &[u8]) -> WarcDigests {
        let mut calc = Self::new();
        calc.update(data);
        calc.finalize()
    }
}
