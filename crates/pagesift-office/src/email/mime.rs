//! MIME 结构（RFC 2045、RFC 2046）：multipart 递归展开成正文和附件。
//! 坏邮件很常见，这里尽量多取内容：没有结束边界、编码写错都不报错。

use super::header::{self, decode_words, get, param, parse_headers, parse_params, split_head};
use super::{Attachment, Message};

/// multipart 嵌套上限，防恶意构造。
const MAX_DEPTH: usize = 32;
/// 单封邮件最多这么多个部分。
const MAX_PARTS: usize = 4_096;

/// 解析一封邮件。
pub(super) fn parse(bytes: &[u8]) -> Message {
    let (head, body) = split_head(bytes);
    let headers = parse_headers(head);
    let field = |name| get(&headers, name).map(decode_words).unwrap_or_default();
    let mut msg = Message {
        subject: field("subject"),
        from: field("from"),
        to: field("to"),
        cc: field("cc"),
        date: field("date"),
        ..Default::default()
    };
    let mut parts = 0;
    walk(&headers, body, 0, &mut msg, &mut parts, false);
    msg
}

fn walk(
    headers: &[(String, String)],
    body: &[u8],
    depth: usize,
    msg: &mut Message,
    parts: &mut usize,
    in_alternative: bool,
) {
    *parts += 1;
    if *parts > MAX_PARTS {
        msg.truncated = true;
        return;
    }
    let (ctype, params) = parse_params(get(headers, "content-type").unwrap_or("text/plain"));
    if ctype.starts_with("multipart/") {
        if depth >= MAX_DEPTH {
            msg.truncated = true;
            return;
        }
        let Some(boundary) = param(&params, "boundary") else {
            // 声明了 multipart 却没有边界：整段当正文，不丢内容
            msg.text.push_str(&String::from_utf8_lossy(body));
            return;
        };
        let alternative = ctype == "multipart/alternative";
        for part in split_parts(body, boundary.as_bytes()) {
            let (head, content) = split_head(part);
            let sub = parse_headers(head);
            walk(
                &sub,
                content,
                depth + 1,
                msg,
                parts,
                alternative || in_alternative,
            );
        }
        return;
    }
    let (disposition, dparams) =
        parse_params(get(headers, "content-disposition").unwrap_or("inline"));
    let filename = param(&dparams, "filename")
        .or_else(|| param(&params, "name"))
        .map(decode_words);
    let decoded = transfer_decode(
        body,
        get(headers, "content-transfer-encoding").unwrap_or("7bit"),
    );
    let is_attachment = disposition == "attachment" || filename.is_some();
    if !is_attachment && ctype == "text/html" {
        let text = header::decode_charset(&decoded, param(&params, "charset"));
        msg.html.push_str(&text);
    } else if !is_attachment && (ctype == "text/plain" || ctype.is_empty()) {
        let text = header::decode_charset(&decoded, param(&params, "charset"));
        if !msg.text.is_empty() {
            msg.text.push_str("\n\n");
        }
        msg.text.push_str(&text);
    } else if !is_attachment && in_alternative {
        // alternative 里别的表示法（text/calendar、amp）只是同一内容的另一种写法
    } else {
        let default_name = if ctype == "message/rfc822" {
            "message.eml"
        } else {
            "attachment"
        };
        msg.attachments.push(Attachment {
            name: filename.unwrap_or_else(|| default_name.to_owned()),
            content_type: ctype,
            data: decoded,
        });
    }
}

/// 按边界切开 multipart 正文（RFC 2046 §5.1.1）：边界在行首，分隔符前的换行属于分隔符。
/// 没有结束边界时，最后一段一直算到正文结束。
fn split_parts<'a>(body: &'a [u8], boundary: &[u8]) -> Vec<&'a [u8]> {
    let mut delim = b"--".to_vec();
    delim.extend_from_slice(boundary);
    let mut out = Vec::new();
    let mut start: Option<usize> = None;
    let mut line = 0usize;
    while line <= body.len() {
        let end = body[line..]
            .iter()
            .position(|&b| b == b'\n')
            .map_or(body.len(), |i| line + i);
        let text = body[line..end]
            .strip_suffix(b"\r")
            .unwrap_or(&body[line..end]);
        if let Some(tail) = text.strip_prefix(delim.as_slice()) {
            let closing = tail.starts_with(b"--");
            if closing || tail.iter().all(u8::is_ascii_whitespace) {
                if let Some(s) = start {
                    out.push(trim_newline(&body[s..line]));
                }
                if closing {
                    return out;
                }
                start = Some((end + 1).min(body.len()));
            }
        }
        line = end + 1;
    }
    if let Some(s) = start {
        out.push(&body[s..]);
    }
    out
}

fn trim_newline(b: &[u8]) -> &[u8] {
    let b = b.strip_suffix(b"\n").unwrap_or(b);
    b.strip_suffix(b"\r").unwrap_or(b)
}

fn transfer_decode(body: &[u8], encoding: &str) -> Vec<u8> {
    match encoding.trim().to_ascii_lowercase().as_str() {
        "base64" => base64(body),
        "quoted-printable" => quoted_printable(body),
        _ => body.to_vec(),
    }
}

/// 宽松的 Base64：跳过换行和杂字符，到 `=` 或输入结束为止。
pub(super) fn base64(input: &[u8]) -> Vec<u8> {
    let mut out = Vec::with_capacity(input.len() / 4 * 3);
    let (mut buf, mut bits) = (0u32, 0u32);
    for &b in input {
        let v = match b {
            b'A'..=b'Z' => b - b'A',
            b'a'..=b'z' => b - b'a' + 26,
            b'0'..=b'9' => b - b'0' + 52,
            b'+' | b'-' => 62,
            b'/' | b'_' => 63,
            b'=' => break,
            _ => continue,
        };
        buf = buf << 6 | u32::from(v);
        bits += 6;
        if bits >= 8 {
            bits -= 8;
            out.push((buf >> bits) as u8);
            buf &= (1 << bits) - 1;
        }
    }
    out
}

fn quoted_printable(input: &[u8]) -> Vec<u8> {
    let mut out = Vec::with_capacity(input.len());
    let mut i = 0;
    while i < input.len() {
        if input[i] != b'=' {
            out.push(input[i]);
            i += 1;
            continue;
        }
        // 软换行
        if input[i + 1..].starts_with(b"\r\n") {
            i += 3;
        } else if input[i + 1..].starts_with(b"\n") {
            i += 2;
        } else if let (Some(h), Some(l)) = (
            input.get(i + 1).copied().and_then(header::hex),
            input.get(i + 2).copied().and_then(header::hex),
        ) {
            out.push(h << 4 | l);
            i += 3;
        } else {
            out.push(b'=');
            i += 1;
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn multipart_with_alternative_and_attachment() {
        let m = parse(
            b"Subject: =?utf-8?B?5Lit5paH?=\r\nFrom: A <a@x>\r\nContent-Type: multipart/mixed; boundary=\"o\"\r\n\r\n\
              preamble\r\n--o\r\nContent-Type: multipart/alternative; boundary=i\r\n\r\n\
              --i\r\nContent-Type: text/plain; charset=utf-8\r\n\r\nplain\r\n\
              --i\r\nContent-Type: text/html\r\nContent-Transfer-Encoding: quoted-printable\r\n\r\n<p>caf=C3=A9=\r\n!</p>\r\n\
              --i\r\nContent-Type: text/calendar\r\n\r\nBEGIN\r\n--i--\r\n\
              --o\r\nContent-Type: text/plain\r\nContent-Disposition: attachment; filename=\"n.txt\"\r\nContent-Transfer-Encoding: base64\r\n\r\naGVs\r\nbG8=\r\n--o--\r\nepilogue",
        );
        assert_eq!(m.subject, "中文");
        assert_eq!(m.from, "A <a@x>");
        assert_eq!(m.text, "plain");
        assert_eq!(m.html, "<p>café!</p>");
        assert_eq!(m.attachments.len(), 1);
        assert_eq!(m.attachments[0].name, "n.txt");
        assert_eq!(m.attachments[0].data, b"hello");
    }

    #[test]
    fn broken_mail_keeps_what_it_can() {
        // 没有结束边界
        let m = parse(b"Content-Type: multipart/mixed; boundary=b\n\n--b\n\nbody text\n");
        assert_eq!(m.text.trim(), "body text");
        // 没有边界参数
        let m = parse(b"Content-Type: multipart/mixed\n\nraw");
        assert_eq!(m.text, "raw");
        // 没有头
        let m = parse(b"\nhello");
        assert_eq!(m.text, "hello");
        // 字符集写错
        let m = parse(b"Content-Type: text/plain; charset=nope\n\ncaf\xe9");
        assert_eq!(m.text, "café");
    }

    #[test]
    fn deep_nesting_stops() {
        let mut body = String::from("x");
        for i in 0..64 {
            body = format!(
                "Content-Type: multipart/mixed; boundary=b{i}\n\n--b{i}\n{body}\n--b{i}--\n"
            );
        }
        assert!(parse(body.as_bytes()).truncated);
    }

    #[test]
    fn transfer_encodings() {
        assert_eq!(base64(b"aGk=\n"), b"hi");
        assert_eq!(base64(b"a G\nk"), b"hi");
        assert_eq!(quoted_printable(b"a=3Db=\nc=\r\nd=ZZ="), b"a=bcd=ZZ=");
    }
}
