//! 邮件头：折行（RFC 5322）、编码词（RFC 2047）、带参数的头和续行参数（RFC 2045、RFC 2231）。

use std::borrow::Cow;

/// 头和正文的分界（第一个空行）。返回头的文本和正文的起点。
pub(super) fn split_head(bytes: &[u8]) -> (&[u8], &[u8]) {
    let mut i = 0;
    while let Some(nl) = bytes[i..].iter().position(|&b| b == b'\n') {
        let line_end = i + nl;
        let line = &bytes[i..line_end];
        if line.is_empty() || line == b"\r" {
            return (&bytes[..i], &bytes[line_end + 1..]);
        }
        i = line_end + 1;
    }
    (bytes, &[])
}

/// 解析头，名字转成小写，值已展开折行、未解码。同名的头保留每一个。
pub(super) fn parse_headers(head: &[u8]) -> Vec<(String, String)> {
    let text = String::from_utf8_lossy(head);
    let mut out: Vec<(String, String)> = Vec::new();
    for line in text.split('\n') {
        let line = line.strip_suffix('\r').unwrap_or(line);
        if line.starts_with([' ', '\t']) {
            if let Some((_, v)) = out.last_mut() {
                v.push(' ');
                v.push_str(line.trim());
            }
        } else if let Some((name, value)) = line.split_once(':')
            && !name.contains(' ')
        {
            out.push((name.trim().to_ascii_lowercase(), value.trim().to_owned()));
        }
    }
    out
}

pub(super) fn get<'a>(headers: &'a [(String, String)], name: &str) -> Option<&'a str> {
    headers
        .iter()
        .find(|(n, _)| n == name)
        .map(|(_, v)| v.as_str())
}

/// 按字符集解码；不认识的字符集和无效序列都不报错（邮件里到处是写错的声明）。
pub(super) fn decode_charset(bytes: &[u8], charset: Option<&str>) -> String {
    if let Some(enc) = charset.and_then(|c| encoding_rs::Encoding::for_label(c.trim().as_bytes())) {
        return enc.decode(bytes).0.into_owned();
    }
    match std::str::from_utf8(bytes) {
        Ok(s) => s.to_owned(),
        Err(_) => encoding_rs::WINDOWS_1252.decode(bytes).0.into_owned(),
    }
}

/// RFC 2047：`=?字符集?B|Q?内容?=`。相邻的编码词之间的空白不算内容（§6.2）。
pub(super) fn decode_words(value: &str) -> String {
    if !value.contains("=?") {
        return value.to_owned();
    }
    let mut out = String::with_capacity(value.len());
    let mut rest = value;
    let mut last_encoded = false;
    while let Some(start) = rest.find("=?") {
        let between = &rest[..start];
        let Some((word, after)) = encoded_word(&rest[start..]) else {
            out.push_str(between);
            out.push_str("=?");
            rest = &rest[start + 2..];
            last_encoded = false;
            continue;
        };
        if !(last_encoded && between.trim().is_empty()) {
            out.push_str(between);
        }
        out.push_str(&word);
        rest = after;
        last_encoded = true;
    }
    out.push_str(rest);
    out
}

fn encoded_word(s: &str) -> Option<(String, &str)> {
    let body = s.strip_prefix("=?")?;
    let (charset, body) = body.split_once('?')?;
    let (encoding, body) = body.split_once('?')?;
    let (text, after) = body.split_once("?=")?;
    // RFC 2231 允许 `字符集*语言`
    let charset = charset.split('*').next().unwrap_or(charset);
    let bytes = match encoding {
        "B" | "b" => super::mime::base64(text.as_bytes()),
        "Q" | "q" => {
            let mut out = Vec::with_capacity(text.len());
            let t = text.as_bytes();
            let mut i = 0;
            while i < t.len() {
                match t[i] {
                    b'_' => out.push(b' '),
                    b'=' if i + 2 < t.len() => match (hex(t[i + 1]), hex(t[i + 2])) {
                        (Some(h), Some(l)) => {
                            out.push(h << 4 | l);
                            i += 2;
                        }
                        _ => out.push(b'='),
                    },
                    b => out.push(b),
                }
                i += 1;
            }
            out
        }
        _ => return None,
    };
    Some((decode_charset(&bytes, Some(charset)), after))
}

pub(super) fn hex(b: u8) -> Option<u8> {
    char::from(b).to_digit(16).map(|d| d as u8)
}

/// `值; 键=值; 键="值"`：返回小写的主值和参数。合并 RFC 2231 的 `键*0*=`、`键*=字符集''内容`。
pub(super) fn parse_params(value: &str) -> (String, Vec<(String, String)>) {
    let mut parts = split_semicolons(value).into_iter();
    let main = parts.next().unwrap_or("").trim().to_ascii_lowercase();
    let mut plain: Vec<(String, String)> = Vec::new();
    // (名字, 序号, 是否百分号编码, 值)
    let mut pieces: Vec<(String, u32, bool, String)> = Vec::new();
    for part in parts {
        let Some((k, v)) = part.split_once('=') else {
            continue;
        };
        let k = k.trim().to_ascii_lowercase();
        let v = unquote(v.trim());
        if let Some(star) = k.find('*') {
            let name = k[..star].to_owned();
            let tail = &k[star + 1..];
            let encoded = tail.is_empty() || tail.ends_with('*');
            let seq = tail.trim_end_matches('*').parse().unwrap_or(0);
            pieces.push((name, seq, encoded, v.into_owned()));
        } else {
            plain.push((k, v.into_owned()));
        }
    }
    pieces.sort_by(|a, b| (&a.0, a.1).cmp(&(&b.0, b.1)));
    let mut i = 0;
    while i < pieces.len() {
        let name = pieces[i].0.clone();
        let mut charset: Option<String> = None;
        let mut bytes = Vec::new();
        while i < pieces.len() && pieces[i].0 == name {
            let (_, seq, encoded, v) = &pieces[i];
            let mut text = v.as_str();
            if *encoded && *seq == 0 {
                let mut it = text.splitn(3, '\'');
                if let (Some(cs), Some(_lang), Some(t)) = (it.next(), it.next(), it.next()) {
                    charset = Some(cs.to_owned());
                    text = t;
                }
            }
            if *encoded {
                percent_into(text, &mut bytes);
            } else {
                bytes.extend_from_slice(text.as_bytes());
            }
            i += 1;
        }
        let decoded = decode_charset(&bytes, charset.as_deref().filter(|c| !c.is_empty()));
        // RFC 2231 的写法优先于同名的普通参数
        plain.retain(|(k, _)| *k != name);
        plain.push((name, decoded));
    }
    (main, plain)
}

pub(super) fn param<'a>(params: &'a [(String, String)], name: &str) -> Option<&'a str> {
    params
        .iter()
        .find(|(k, _)| k == name)
        .map(|(_, v)| v.as_str())
}

fn percent_into(text: &str, out: &mut Vec<u8>) {
    let t = text.as_bytes();
    let mut i = 0;
    while i < t.len() {
        if t[i] == b'%'
            && let (Some(h), Some(l)) = (
                t.get(i + 1).copied().and_then(hex),
                t.get(i + 2).copied().and_then(hex),
            )
        {
            out.push(h << 4 | l);
            i += 3;
        } else {
            out.push(t[i]);
            i += 1;
        }
    }
}

/// 按分号切开，引号里的分号不算。
fn split_semicolons(value: &str) -> Vec<&str> {
    let mut out = Vec::new();
    let (mut start, mut quoted, mut escaped) = (0, false, false);
    for (i, c) in value.char_indices() {
        if escaped {
            escaped = false;
        } else if quoted && c == '\\' {
            escaped = true;
        } else if c == '"' {
            quoted = !quoted;
        } else if c == ';' && !quoted {
            out.push(&value[start..i]);
            start = i + 1;
        }
    }
    out.push(&value[start..]);
    out
}

fn unquote(v: &str) -> Cow<'_, str> {
    let Some(inner) = v.strip_prefix('"').and_then(|s| s.strip_suffix('"')) else {
        return Cow::Borrowed(v);
    };
    if !inner.contains('\\') {
        return Cow::Borrowed(inner);
    }
    let mut out = String::with_capacity(inner.len());
    let mut it = inner.chars();
    while let Some(c) = it.next() {
        out.push(if c == '\\' { it.next().unwrap_or(c) } else { c });
    }
    Cow::Owned(out)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn splits_and_unfolds() {
        let (h, b) = split_head(b"Subject: a\r\n  b\r\nX: y\r\n\r\nbody\r\n");
        assert_eq!(b, b"body\r\n");
        let hs = parse_headers(h);
        assert_eq!(get(&hs, "subject"), Some("a b"));
        assert_eq!(get(&hs, "x"), Some("y"));
        assert_eq!(split_head(b"only: head").1, b"");
    }

    #[test]
    fn encoded_words() {
        assert_eq!(decode_words("=?UTF-8?B?5Lit5paH?="), "中文");
        assert_eq!(decode_words("=?iso-8859-1?Q?caf=E9_x?="), "café x");
        // 相邻编码词之间的空白不算，和普通文字之间的算
        assert_eq!(decode_words("=?utf-8?Q?a?= =?utf-8?Q?b?= c"), "ab c");
        assert_eq!(decode_words("x =?utf-8?Q?a?="), "x a");
        assert_eq!(decode_words("=?gb2312?B?1tDOxA==?="), "中文");
        // 坏的编码词原样留着，不丢内容
        assert_eq!(decode_words("=?broken"), "=?broken");
        assert_eq!(decode_words("=?x?Z?y?="), "=?x?Z?y?=");
    }

    #[test]
    fn params_with_quotes_and_rfc2231() {
        let (m, p) = parse_params("Text/Plain; charset=\"utf-8\"; name=\"a;b.txt\"");
        assert_eq!(m, "text/plain");
        assert_eq!(param(&p, "charset"), Some("utf-8"));
        assert_eq!(param(&p, "name"), Some("a;b.txt"));
        let (_, p) = parse_params("attachment; filename*=UTF-8''%E4%B8%AD.txt; filename=old.txt");
        assert_eq!(param(&p, "filename"), Some("中.txt"));
        let (_, p) = parse_params("x; filename*0*=utf-8''a%20; filename*1=b.txt");
        assert_eq!(param(&p, "filename"), Some("a b.txt"));
    }
}
