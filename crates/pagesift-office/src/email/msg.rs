//! Outlook `.msg`（MS-OXMSG）：OLE 复合文件里每个属性一个流，名字是 `__substg1.0_<属性号><类型>`。
//! 只取主题、收发件人、时间、正文（纯文本和 HTML）和附件。压缩 RTF 正文只在前两者都没有时报警告。

use super::{Attachment, Message};
use crate::Error;
use crate::ole::{Entry, Ole};

const TYPE_UNICODE: &str = "001F";
const TYPE_ANSI: &str = "001E";

pub(super) fn parse(bytes: &[u8]) -> Result<Message, Error> {
    let ole = Ole::open(bytes)?;
    let root = ole.children(None);
    if !root.iter().any(|e| e.name.starts_with("__substg1.0_")) {
        return Err(Error::Malformed("not an Outlook message".into()));
    }
    let mut msg = Message {
        subject: text(&ole, &root, "0037").unwrap_or_default(),
        from: sender(&ole, &root),
        to: text(&ole, &root, "0E04").unwrap_or_default(),
        cc: text(&ole, &root, "0E03").unwrap_or_default(),
        date: root
            .iter()
            .find(|e| e.name == "__properties_version1.0")
            .and_then(|e| ole.read(e).ok())
            .and_then(|p| filetime_prop(&p))
            .unwrap_or_default(),
        text: text(&ole, &root, "1000").unwrap_or_default(),
        html: html_body(&ole, &root),
        ..Default::default()
    };
    let (to, cc) = recipients(&ole, &root);
    if !to.is_empty() {
        msg.to = to;
    }
    if !cc.is_empty() {
        msg.cc = cc;
    }
    if msg.text.is_empty() && msg.html.is_empty() {
        let body = root
            .iter()
            .find(|e| e.is_stream && e.name.starts_with("__substg1.0_1009"))
            .and_then(|e| ole.read(e).ok());
        if let Some(body) = body {
            match super::lzfu::decompress(&body) {
                Some(rtf) => match crate::rtf::convert(&rtf, &crate::Options::default()) {
                    Ok(d) => msg.text = d.markdown,
                    Err(_) => msg
                        .notes
                        .push("msg_rtf_body: the RTF body could not be read".into()),
                },
                None => msg
                    .notes
                    .push("msg_rtf_body: the compressed RTF body is broken".into()),
            }
        }
    }
    for e in &root {
        if !e.name.starts_with("__attach_version1.0_") || e.is_stream {
            continue;
        }
        let parts = ole.children(Some(e));
        let name = text(&ole, &parts, "3707")
            .or_else(|| text(&ole, &parts, "3704"))
            .unwrap_or_else(|| "attachment".to_owned());
        let content_type = text(&ole, &parts, "370E").unwrap_or_default();
        // 内嵌的邮件对象（3701 是存储而不是流）：转成一封 .eml，走附件的转换路径
        if let Some(inner) = parts
            .iter()
            .find(|p| !p.is_stream && p.name.starts_with("__substg1.0_3701"))
        {
            let inner = ole.children(Some(inner));
            let subject = text(&ole, &inner, "0037").unwrap_or_default();
            let eml = embedded_eml(&ole, &inner, &subject);
            let file = if subject.trim().is_empty() {
                name.clone()
            } else {
                subject.trim().to_owned()
            };
            msg.attachments.push(Attachment {
                name: format!("{file}.eml"),
                content_type: "message/rfc822".to_owned(),
                data: eml,
            });
            continue;
        }
        let data = parts
            .iter()
            .find(|p| p.is_stream && p.name == "__substg1.0_37010102")
            .and_then(|p| ole.read(p).ok())
            .unwrap_or_default();
        msg.attachments.push(Attachment {
            name,
            content_type,
            data,
        });
    }
    Ok(msg)
}

/// 收件人存储：`__recip_version1.0_#序号`。显示名加地址，按类型分到 To 和 Cc（Bcc 不写）。
fn recipients(ole: &Ole<'_>, root: &[&Entry]) -> (String, String) {
    const PR_RECIPIENT_TYPE: u32 = 0x0C15_0003;
    let (mut to, mut cc) = (Vec::new(), Vec::new());
    for e in root {
        if e.is_stream || !e.name.starts_with("__recip_version1.0_") {
            continue;
        }
        let parts = ole.children(Some(e));
        let name = text(ole, &parts, "3001").unwrap_or_default();
        let addr = text(ole, &parts, "39FE")
            .or_else(|| text(ole, &parts, "3003"))
            .filter(|a| a.contains('@'))
            .unwrap_or_default();
        let shown = match (name.is_empty(), addr.is_empty()) {
            (false, false) if name != addr => format!("{name} <{addr}>"),
            (true, false) | (false, false) => addr,
            (false, true) => name,
            (true, true) => continue,
        };
        // 属性流：8 字节头之后每项 16 字节，标签 4 字节、标志 4 字节、值 8 字节
        let kind = parts
            .iter()
            .find(|p| p.is_stream && p.name == "__properties_version1.0")
            .and_then(|p| ole.read(p).ok())
            .and_then(|d| {
                d.get(8..)?
                    .as_chunks::<16>()
                    .0
                    .iter()
                    .find(|c| u32::from_le_bytes([c[0], c[1], c[2], c[3]]) == PR_RECIPIENT_TYPE)
                    .map(|c| u32::from_le_bytes([c[8], c[9], c[10], c[11]]))
            })
            .unwrap_or(1);
        match kind {
            2 => cc.push(shown),
            3 => {}
            _ => to.push(shown),
        }
    }
    (to.join(", "), cc.join(", "))
}

/// 把内嵌邮件的属性写成一封最简单的 RFC 822 邮件。头部的换行换成空格，防止注入多余的头。
fn embedded_eml(ole: &Ole<'_>, parts: &[&Entry], subject: &str) -> Vec<u8> {
    let one_line = |s: &str| s.replace(['\r', '\n'], " ");
    let mut out = String::new();
    out.push_str(&format!("Subject: {}\r\n", one_line(subject)));
    let from = sender(ole, parts);
    if !from.is_empty() {
        out.push_str(&format!("From: {}\r\n", one_line(&from)));
    }
    let (to, cc) = recipients(ole, parts);
    let to = if to.is_empty() {
        text(ole, parts, "0E04").unwrap_or_default()
    } else {
        to
    };
    if !to.is_empty() {
        out.push_str(&format!("To: {}\r\n", one_line(&to)));
    }
    if !cc.is_empty() {
        out.push_str(&format!("Cc: {}\r\n", one_line(&cc)));
    }
    out.push_str("MIME-Version: 1.0\r\n");
    let body = text(ole, parts, "1000").unwrap_or_default();
    if body.is_empty() {
        out.push_str("Content-Type: text/html; charset=utf-8\r\n\r\n");
        out.push_str(&html_body(ole, parts));
    } else {
        out.push_str("Content-Type: text/plain; charset=utf-8\r\n\r\n");
        out.push_str(&body);
    }
    out.into_bytes()
}

fn sender(ole: &Ole<'_>, root: &[&Entry]) -> String {
    let name = text(ole, root, "0C1A").unwrap_or_default();
    let addr = text(ole, root, "5D01")
        .or_else(|| text(ole, root, "0C1F"))
        .filter(|a| a.contains('@'));
    match (name.is_empty(), addr) {
        (false, Some(a)) => format!("{name} <{a}>"),
        (true, Some(a)) => a,
        _ => name,
    }
}

/// 取一个文字属性，先找 Unicode 版，再找 ANSI 版。
fn text(ole: &Ole<'_>, parts: &[&Entry], id: &str) -> Option<String> {
    let find = |ty: &str| {
        let want = format!("__substg1.0_{id}{ty}");
        parts.iter().find(|e| e.is_stream && e.name == want)
    };
    if let Some(e) = find(TYPE_UNICODE) {
        return Some(utf16(&ole.read(e).ok()?));
    }
    let e = find(TYPE_ANSI)?;
    Some(super::header::decode_charset(&ole.read(e).ok()?, None))
}

fn utf16(bytes: &[u8]) -> String {
    let units: Vec<u16> = bytes
        .as_chunks::<2>()
        .0
        .iter()
        .map(|c| u16::from_le_bytes([c[0], c[1]]))
        .collect();
    String::from_utf16_lossy(&units)
        .trim_end_matches('\0')
        .to_owned()
}

/// HTML 正文（PidTagHtml，属性号 1013）：通常是二进制流，字节里自己声明字符集。
fn html_body(ole: &Ole<'_>, root: &[&Entry]) -> String {
    if let Some(e) = root
        .iter()
        .find(|e| e.is_stream && e.name == "__substg1.0_10130102")
        && let Ok(bytes) = ole.read(e)
    {
        return super::header::decode_charset(&bytes, None);
    }
    text(ole, root, "1013").unwrap_or_default()
}

/// `__properties_version1.0`：顶层有 32 字节头，之后每个属性 16 字节（标签、标志、8 字节值）。
/// 取发送时间（0039），没有就取投递时间（0E06）。
fn filetime_prop(p: &[u8]) -> Option<String> {
    const HEADER: usize = 32;
    let mut found = [None, None];
    for rec in p.get(HEADER..)?.as_chunks::<16>().0 {
        let ty = u16::from_le_bytes([rec[0], rec[1]]);
        let id = u16::from_le_bytes([rec[2], rec[3]]);
        // 0x0040 是 PtypTime
        if ty == 0x0040 {
            let v = u64::from_le_bytes(rec[8..16].try_into().ok()?);
            match id {
                0x0039 => found[0] = Some(v),
                0x0E06 => found[1] = Some(v),
                _ => {}
            }
        }
    }
    found[0].or(found[1]).and_then(format_filetime)
}

/// FILETIME（1601 年起的 100 纳秒）转 ISO 8601（UTC）。
fn format_filetime(ft: u64) -> Option<String> {
    const EPOCH_DIFF: u64 = 11_644_473_600;
    let secs = (ft / 10_000_000).checked_sub(EPOCH_DIFF)?;
    let days = (secs / 86_400) as i64;
    let rem = secs % 86_400;
    // Howard Hinnant 的 civil_from_days
    let z = days + 719_468;
    let era = z.div_euclid(146_097);
    let doe = z.rem_euclid(146_097);
    let yoe = (doe - doe / 1460 + doe / 36_524 - doe / 146_096) / 365;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let d = doy - (153 * mp + 2) / 5 + 1;
    let m = if mp < 10 { mp + 3 } else { mp - 9 };
    let y = yoe + era * 400 + i64::from(m <= 2);
    Some(format!(
        "{y:04}-{m:02}-{d:02}T{:02}:{:02}:{:02}Z",
        rem / 3600,
        rem % 3600 / 60,
        rem % 60
    ))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::ole::tests::{Node, build};

    fn le16(s: &str) -> Vec<u8> {
        s.encode_utf16().flat_map(u16::to_le_bytes).collect()
    }

    #[test]
    fn reads_properties_and_attachments() {
        let (subject, body, from, attach) =
            (le16("Hi 中"), le16("Body"), le16("Ann"), le16("n.txt"));
        let mut props = vec![0u8; 32];
        props.extend_from_slice(&[0x40, 0, 0x39, 0, 0, 0, 0, 0]);
        // 2024-01-02T03:04:05Z
        let ft: u64 = (1_704_164_645 + 11_644_473_600) * 10_000_000;
        props.extend_from_slice(&ft.to_le_bytes());
        let bytes = build(&[
            Node {
                name: "__substg1.0_0037001F",
                data: &subject,
                storage: false,
                parent: 0,
            },
            Node {
                name: "__substg1.0_1000001F",
                data: &body,
                storage: false,
                parent: 0,
            },
            Node {
                name: "__substg1.0_0C1A001F",
                data: &from,
                storage: false,
                parent: 0,
            },
            Node {
                name: "__substg1.0_0C1F001E",
                data: b"a@x.io",
                storage: false,
                parent: 0,
            },
            Node {
                name: "__properties_version1.0",
                data: &props,
                storage: false,
                parent: 0,
            },
            Node {
                name: "__attach_version1.0_#00000000",
                data: b"",
                storage: true,
                parent: 0,
            },
            Node {
                name: "__substg1.0_3707001F",
                data: &attach,
                storage: false,
                parent: 6,
            },
            Node {
                name: "__substg1.0_37010102",
                data: b"file body",
                storage: false,
                parent: 6,
            },
        ]);
        let m = parse(&bytes).unwrap();
        assert_eq!(m.subject, "Hi 中");
        assert_eq!(m.text, "Body");
        assert_eq!(m.from, "Ann <a@x.io>");
        assert_eq!(m.date, "2024-01-02T03:04:05Z");
        assert_eq!(m.attachments.len(), 1);
        assert_eq!(m.attachments[0].name, "n.txt");
        assert_eq!(m.attachments[0].data, b"file body");
    }

    #[test]
    fn other_compound_files_are_rejected() {
        let bytes = build(&[Node {
            name: "WordDocument",
            data: b"x",
            storage: false,
            parent: 0,
        }]);
        assert!(matches!(parse(&bytes), Err(Error::Malformed(_))));
    }

    #[test]
    fn filetime_edges() {
        assert_eq!(format_filetime(0), None);
        assert_eq!(
            format_filetime(11_644_473_600 * 10_000_000).as_deref(),
            Some("1970-01-01T00:00:00Z")
        );
    }
}
