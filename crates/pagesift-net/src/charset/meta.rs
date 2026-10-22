//! 扫描 HTML 文档中的 `<meta>` 标签与 HTTP Content-Type 中的 charset 参数。

use encoding_rs::{Encoding, UTF_8, WINDOWS_1252};

pub const PRESCAN_BYTES: usize = 4096;

pub fn charset_param(content_type: &str) -> Option<&str> {
    content_type.split(';').skip(1).find_map(|part| {
        let (k, v) = part.split_once('=')?;
        k.trim()
            .eq_ignore_ascii_case("charset")
            .then(|| v.trim().trim_matches(['"', '\'']))
    })
}

/// 按 HTML 规范扫描 `<meta charset>` 和 `<meta http-equiv="content-type" content="...">`。
pub fn prescan_meta(head: &[u8]) -> Option<&'static Encoding> {
    let mut i = 0;
    while i < head.len() {
        if head[i..].starts_with(b"<!--") {
            i = find(head, i + 4, b"-->").map_or(head.len(), |e| e + 3);
            continue;
        }
        if head[i] == b'<'
            && head.len() >= i + 6
            && head[i + 1..i + 5].eq_ignore_ascii_case(b"meta")
            && matches!(head[i + 5], b' ' | b'\t' | b'\n' | b'\r' | b'\x0c' | b'/')
        {
            let (attrs, end) = parse_attrs(head, i + 5);
            i = end;
            if let Some(enc) = meta_encoding(&attrs) {
                // 规范：meta 声明 UTF-16 时按 UTF-8 处理，x-user-defined 按 windows-1252。
                return Some(
                    if enc == encoding_rs::UTF_16LE || enc == encoding_rs::UTF_16BE {
                        UTF_8
                    } else if enc == encoding_rs::X_USER_DEFINED {
                        WINDOWS_1252
                    } else {
                        enc
                    },
                );
            }
            continue;
        }
        i += 1;
    }
    None
}

fn meta_encoding(attrs: &[(String, String)]) -> Option<&'static Encoding> {
    let get = |name: &str| {
        attrs
            .iter()
            .find(|(k, _)| k == name)
            .map(|(_, v)| v.as_str())
    };
    if let Some(cs) = get("charset") {
        return Encoding::for_label(cs.trim().as_bytes());
    }
    if get("http-equiv").is_some_and(|v| v.trim().eq_ignore_ascii_case("content-type")) {
        let content = get("content")?.to_ascii_lowercase();
        let rest = &content[content.find("charset")? + 7..];
        let rest = rest.trim_start().strip_prefix('=')?.trim_start();
        let label: String = rest
            .trim_start_matches(['"', '\''])
            .chars()
            .take_while(|c| !matches!(c, '"' | '\'' | ';' | ' ' | '\t'))
            .collect();
        return Encoding::for_label(label.as_bytes());
    }
    None
}

fn parse_attrs(s: &[u8], mut i: usize) -> (Vec<(String, String)>, usize) {
    let mut attrs = Vec::new();
    loop {
        while i < s.len() && (s[i].is_ascii_whitespace() || s[i] == b'/') {
            i += 1;
        }
        if i >= s.len() {
            return (attrs, i);
        }
        if s[i] == b'>' {
            return (attrs, i + 1);
        }
        let start = i;
        while i < s.len() && !matches!(s[i], b'=' | b'>' | b'/') && !s[i].is_ascii_whitespace() {
            i += 1;
        }
        let name = String::from_utf8_lossy(&s[start..i]).to_ascii_lowercase();
        while i < s.len() && s[i].is_ascii_whitespace() {
            i += 1;
        }
        let mut value = String::new();
        if i < s.len() && s[i] == b'=' {
            i += 1;
            while i < s.len() && s[i].is_ascii_whitespace() {
                i += 1;
            }
            if i < s.len() && matches!(s[i], b'"' | b'\'') {
                let q = s[i];
                let vs = i + 1;
                i = vs;
                while i < s.len() && s[i] != q {
                    i += 1;
                }
                value = String::from_utf8_lossy(&s[vs..i]).into_owned();
                i = (i + 1).min(s.len());
            } else {
                let vs = i;
                while i < s.len() && s[i] != b'>' && !s[i].is_ascii_whitespace() {
                    i += 1;
                }
                value = String::from_utf8_lossy(&s[vs..i]).into_owned();
            }
        }
        if name.is_empty() {
            i += 1;
        } else if !attrs.iter().any(|(k, _)| *k == name) {
            attrs.push((name, value));
        }
    }
}

fn find(hay: &[u8], from: usize, needle: &[u8]) -> Option<usize> {
    hay.get(from..)?
        .windows(needle.len())
        .position(|w| w == needle)
        .map(|p| p + from)
}
