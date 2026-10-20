//! 字符引用解码（`&amp;`、`&#169;`、`&#x1F600;`），按 HTML 规范处理没有分号的历史写法和属性里的特例。

use crate::entities::ENTITIES;

/// 最长的实体名（不含 `&`，含 `;`）。
const MAX_NAME: usize = 32;
/// 没有分号也能识别的历史实体，名字最长 6 个字符。
const MAX_LEGACY: usize = 6;

/// 解码 `raw` 里的字符引用，追加到 `out`。`in_attr` 为真时按属性值的规则：
/// 没有分号的历史实体后面紧跟字母、数字或 `=` 时不解码（`?a=1&copy=2` 里的 `&copy` 保持原样）。
pub(crate) fn decode_into(out: &mut String, raw: &str, in_attr: bool) {
    let bytes = raw.as_bytes();
    let mut last = 0;
    let mut i = 0;
    while let Some(off) = memchr::memchr(b'&', &bytes[i..]) {
        let amp = i + off;
        out.push_str(&raw[last..amp]);
        match reference(raw, amp + 1, in_attr) {
            Some((a, b, end)) => {
                out.push(a);
                if b != '\0' {
                    out.push(b);
                }
                last = end;
                i = end;
            }
            None => {
                out.push('&');
                last = amp + 1;
                i = amp + 1;
            }
        }
    }
    out.push_str(&raw[last..]);
}

/// 解析 `&` 之后从 `start` 开始的引用，返回解出的字符和引用结束的位置。
fn reference(raw: &str, start: usize, in_attr: bool) -> Option<(char, char, usize)> {
    let bytes = raw.as_bytes();
    match bytes.get(start) {
        Some(b'#') => numeric(bytes, start + 1),
        Some(c) if c.is_ascii_alphanumeric() => named(raw, start, in_attr),
        _ => None,
    }
}

fn numeric(bytes: &[u8], start: usize) -> Option<(char, char, usize)> {
    let (hex, mut i) = match bytes.get(start) {
        Some(b'x' | b'X') => (true, start + 1),
        _ => (false, start),
    };
    let digits_start = i;
    let mut value: u32 = 0;
    while let Some(&c) = bytes.get(i) {
        let d = match c {
            b'0'..=b'9' => c - b'0',
            b'a'..=b'f' if hex => c - b'a' + 10,
            b'A'..=b'F' if hex => c - b'A' + 10,
            _ => break,
        };
        value = value
            .saturating_mul(if hex { 16 } else { 10 })
            .saturating_add(d as u32);
        i += 1;
    }
    if i == digits_start {
        return None;
    }
    if bytes.get(i) == Some(&b';') {
        i += 1;
    }
    Some((numeric_char(value), '\0', i))
}

/// 规范里的数字引用修正：0、超范围和代理区变成 U+FFFD，0x80–0x9F 按 Windows-1252 解释。
fn numeric_char(value: u32) -> char {
    const C1: [u32; 32] = [
        0x20AC, 0x81, 0x201A, 0x0192, 0x201E, 0x2026, 0x2020, 0x2021, 0x02C6, 0x2030, 0x0160,
        0x2039, 0x0152, 0x8D, 0x017D, 0x8F, 0x90, 0x2018, 0x2019, 0x201C, 0x201D, 0x2022, 0x2013,
        0x2014, 0x02DC, 0x2122, 0x0161, 0x203A, 0x0153, 0x9D, 0x017E, 0x0178,
    ];
    let v = match value {
        0x80..=0x9F => C1[(value - 0x80) as usize],
        v => v,
    };
    if v == 0 {
        return '\u{FFFD}';
    }
    char::from_u32(v).unwrap_or('\u{FFFD}')
}

fn named(raw: &str, start: usize, in_attr: bool) -> Option<(char, char, usize)> {
    let bytes = raw.as_bytes();
    let mut end = start;
    while end < bytes.len() && end - start < MAX_NAME && bytes[end].is_ascii_alphanumeric() {
        end += 1;
    }
    if bytes.get(end) == Some(&b';')
        && let Some((a, b)) = lookup(&raw[start..=end])
    {
        return Some((a, b, end + 1));
    }
    // 没有分号：只认历史实体，取最长的前缀（`&notit;` 是 `&not` 加 "it;"）
    for len in (1..=(end - start).min(MAX_LEGACY)).rev() {
        let Some((a, b)) = lookup(&raw[start..start + len]) else {
            continue;
        };
        let next = bytes.get(start + len).copied();
        if in_attr && next.is_some_and(|c| c.is_ascii_alphanumeric() || c == b'=') {
            return None;
        }
        return Some((a, b, start + len));
    }
    None
}

fn lookup(name: &str) -> Option<(char, char)> {
    ENTITIES
        .binary_search_by(|(n, _, _)| n.as_bytes().cmp(name.as_bytes()))
        .ok()
        .map(|i| (ENTITIES[i].1, ENTITIES[i].2))
}

#[cfg(test)]
mod tests {
    fn text(s: &str) -> String {
        let mut out = String::new();
        super::decode_into(&mut out, s, false);
        out
    }
    fn attr(s: &str) -> String {
        let mut out = String::new();
        super::decode_into(&mut out, s, true);
        out
    }

    #[test]
    fn named_and_numeric() {
        assert_eq!(text("a &amp; b &lt;c&gt;"), "a & b <c>");
        assert_eq!(text("&copy;&#169;&#xA9;&#XA9"), "©©©©");
        assert_eq!(text("&nbsp;x"), "\u{a0}x");
        assert_eq!(text("&acE;"), "\u{223e}\u{333}");
        assert_eq!(
            text("&#0;&#x110000;&#xD800;&#128;"),
            "\u{FFFD}\u{FFFD}\u{FFFD}€"
        );
        assert_eq!(text("&#99999999999999999999;"), "\u{FFFD}");
    }

    #[test]
    fn not_references() {
        assert_eq!(
            text("AT&T & co &#; &#x; &unknown; &"),
            "AT&T & co &#; &#x; &unknown; &"
        );
        assert_eq!(text("&notit; &amp"), "¬it; &");
    }

    #[test]
    fn attribute_rules() {
        assert_eq!(attr("?a=1&copy=2&lang=en"), "?a=1&copy=2&lang=en");
        assert_eq!(attr("&copy 2024 &amp;x"), "© 2024 &x");
        assert_eq!(attr("&ampx"), "&ampx");
    }
}
