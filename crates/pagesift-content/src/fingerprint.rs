//! 内容指纹：原始 HTML、正文 Markdown 和规范化纯文本的 sha256。

use sha2::{Digest, Sha256};

use crate::text::normalize_text;

/// 三个 sha256，均为小写十六进制。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Fingerprints {
    /// 传入的原始 HTML。
    pub raw_html: String,
    /// 输出的 Markdown。
    pub markdown: String,
    /// 纯文本经 [`normalize_text`] 规范化后的结果，空白和 Unicode 写法差异不影响它。
    pub text: String,
}

pub(crate) fn compute(raw_html: &str, markdown: &str, plain_text: &str) -> Fingerprints {
    Fingerprints {
        raw_html: sha256_hex(raw_html.as_bytes()),
        markdown: sha256_hex(markdown.as_bytes()),
        text: sha256_hex(normalize_text(plain_text).as_bytes()),
    }
}

pub fn sha256_hex(data: &[u8]) -> String {
    const HEX: &[u8; 16] = b"0123456789abcdef";
    let digest = Sha256::digest(data);
    let mut out = String::with_capacity(64);
    for b in digest {
        out.push(HEX[(b >> 4) as usize] as char);
        out.push(HEX[(b & 0xf) as usize] as char);
    }
    out
}

#[cfg(test)]
mod tests {
    #[test]
    fn known_digest() {
        assert_eq!(
            super::sha256_hex(b"abc"),
            "ba7816bf8f01cfea414140de5dae2223b00361a396177a9cb410ff61f20015ad"
        );
    }
}
