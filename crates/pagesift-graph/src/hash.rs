//! Deterministic identifiers, URL canonicalization, and excerpt formatting.

use std::fmt::Write;

use sha2::{Digest, Sha256};
use url::Url;

const TRACKING_PREFIXES: &[&str] = &["utm_"];
const TRACKING_KEYS: &[&str] = &[
    "fbclid", "gclid", "dclid", "msclkid", "yclid", "mc_cid", "mc_eid", "_hsenc", "_hsmi",
    "ref_src", "igshid", "vero_id",
];

/// Compute lowercase hex SHA-256 of arbitrary bytes.
pub fn sha256_hex(bytes: &[u8]) -> String {
    let digest = Sha256::digest(bytes);
    let mut out = String::with_capacity(64); // [协议] SHA-256 十六进制字符串固定 64 字符
    for b in digest {
        let _ = write!(out, "{b:02x}");
    }
    out
}

/// Build a collision-resistant length-prefixed deterministic short identifier (`<prefix>_<16hex>`).
pub fn short_id(prefix: &str, parts: &[&str]) -> String {
    let mut hasher = Sha256::new();
    hasher.update(prefix.as_bytes());
    hasher.update([0u8]);
    for p in parts {
        hasher.update((p.len() as u64).to_le_bytes());
        hasher.update(p.as_bytes());
    }
    let digest = hasher.finalize();
    let mut out = String::with_capacity(prefix.len() + 1 + 16); // [协议] 8 字节摘要十六进制固定 16 字符
    out.push_str(prefix);
    out.push('_');
    for b in &digest[..8] {
        let _ = write!(out, "{b:02x}");
    }
    out
}

/// Extract normalized lowercase host (or empty string if absent).
pub fn host_of(raw_url: &str) -> String {
    Url::parse(raw_url.trim())
        .ok()
        .and_then(|u| u.host_str().map(str::to_ascii_lowercase))
        .unwrap_or_default()
}

/// Strip fragments, default ports, and common tracking parameters; sort remaining query pairs.
pub fn normalize_url(raw: &str) -> String {
    let trimmed = raw.trim();
    let Ok(mut url) = Url::parse(trimmed) else {
        return trimmed.to_owned();
    };
    if !matches!(url.scheme(), "http" | "https") {
        url.set_fragment(None);
        return url.to_string();
    }
    url.set_fragment(None);
    if matches!(
        (url.scheme(), url.port()),
        ("http", Some(80)) | ("https", Some(443)) // [协议] HTTP/HTTPS 默认端口规范化省略
    ) {
        let _ = url.set_port(None);
    }
    if url.path().is_empty() {
        url.set_path("/");
    }
    let mut kept: Vec<(String, String)> = url
        .query_pairs()
        .filter(|(k, _)| !is_tracking_param(k))
        .map(|(k, v)| (k.into_owned(), v.into_owned()))
        .collect();
    kept.sort();
    if kept.is_empty() {
        url.set_query(None);
    } else {
        let mut ser = url.query_pairs_mut();
        ser.clear();
        for (k, v) in kept {
            ser.append_pair(&k, &v);
        }
    }
    url.to_string()
}

/// Logical resource key derived from canonical or final URL.
pub fn normalize_resource_key(raw: &str) -> String {
    normalize_url(raw)
}

fn is_tracking_param(key: &str) -> bool {
    let lower = key.to_ascii_lowercase();
    TRACKING_PREFIXES.iter().any(|p| lower.starts_with(p))
        || TRACKING_KEYS.iter().any(|k| lower == *k)
}

/// Collapse runs of whitespace and clamp length cleanly to `max_chars` Unicode scalars.
pub fn compact_excerpt(text: &str, max_chars: usize) -> String {
    if max_chars == 0 {
        return String::new();
    }
    let mut out = String::with_capacity(text.len().min(max_chars.saturating_mul(3))); // [策略] 摘要缓冲按每字符 3 字节估算预分配
    let mut count = 0usize;
    let mut pending_space = false;
    let mut truncated = false;

    for ch in text.chars() {
        if ch.is_whitespace() {
            if !out.is_empty() {
                pending_space = true;
            }
            continue;
        }
        let needed = usize::from(pending_space) + 1;
        if count + needed > max_chars {
            truncated = true;
            break;
        }
        if pending_space {
            out.push(' ');
            count += 1;
            pending_space = false;
        }
        out.push(ch);
        count += 1;
    }

    const ELLIPSIS_LEN: usize = 3; // [策略] 为 "..." 省略号预留 3 字符空间
    if truncated && max_chars > ELLIPSIS_LEN {
        let mut chars: Vec<char> = out.chars().collect();
        while chars.len() + ELLIPSIS_LEN > max_chars && !chars.is_empty() {
            chars.pop();
        }
        out = chars.into_iter().collect();
        out.push_str("...");
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn normalizes_urls_and_strips_tracking() {
        assert_eq!(
            normalize_url("HTTPS://Example.COM:443/pricing?utm_source=x&plan=pro&b=1#top"),
            "https://example.com/pricing?b=1&plan=pro"
        );
        assert_eq!(
            normalize_url("http://example.com:80"),
            "http://example.com/"
        );
    }

    #[test]
    fn compact_excerpt_handles_cjk_and_limits() {
        let s = compact_excerpt("  定价   方案 \n 标准版每席位每年 490 元  ", 12);
        assert!(s.chars().count() <= 12);
        assert!(s.ends_with("..."));
    }
}
