//! 内容指纹：原始 HTML、正文 Markdown 和规范化纯文本的 BLAKE3。
//! 选 BLAKE3 而不是 SHA-256：同样是密码学哈希，没有 SHA 硬件指令的 CPU 上快一个数量级
//! （评测集上 SHA-256 一页要 1.1 ms，和解析整页差不多）。

use crate::text::normalize_text;

/// 三个 BLAKE3 哈希，均为 64 位小写十六进制。
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
        raw_html: hash_hex(raw_html.as_bytes()),
        markdown: hash_hex(markdown.as_bytes()),
        text: hash_hex(normalize_text(plain_text).as_bytes()),
    }
}

pub fn hash_hex(data: &[u8]) -> String {
    blake3::hash(data).to_hex().to_string()
}

#[cfg(test)]
mod tests {
    #[test]
    fn known_digest() {
        assert_eq!(
            super::hash_hex(b"abc"),
            "6437b3ac38465133ffb63b75273a8db548c558465d79db03fd359c6cd5bd9d85"
        );
    }
}
