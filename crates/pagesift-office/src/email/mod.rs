//! 邮件：`.eml`（RFC 5322 + MIME）、`.mbox`、Outlook `.msg`。
//! 输出：主题作标题，收发件人和时间一小段，正文，附件清单，再逐个转换附件。
//! 有 HTML 正文时走调用方给的 HTML 提取器（和网页一致），否则用纯文本。附件的转换交给
//! [`crate::Options::member`]；嵌套和文件总数与压缩包共用一份额度。

mod header;
mod lzfu;
mod mime;
mod msg;

use crate::md;
use crate::{Converted, Error, Options, archive, budget, check_deadline};

#[derive(Debug, Default)]
pub(crate) struct Message {
    subject: String,
    from: String,
    to: String,
    cc: String,
    date: String,
    text: String,
    html: String,
    attachments: Vec<Attachment>,
    truncated: bool,
    notes: Vec<String>,
}

#[derive(Debug)]
struct Attachment {
    name: String,
    content_type: String,
    data: Vec<u8>,
}

pub(crate) fn convert_eml(bytes: &[u8], opts: &Options<'_>) -> Result<Converted, Error> {
    let _nest = archive::Nest::enter(opts).ok_or(Error::TooLarge("attachment nesting"))?;
    let msg = mime::parse(bytes);
    finish(vec![msg], opts)
}

pub(crate) fn convert_msg(bytes: &[u8], opts: &Options<'_>) -> Result<Converted, Error> {
    let _nest = archive::Nest::enter(opts).ok_or(Error::TooLarge("attachment nesting"))?;
    let msg = msg::parse(bytes)?;
    finish(vec![msg], opts)
}

pub(crate) fn convert_mbox(bytes: &[u8], opts: &Options<'_>) -> Result<Converted, Error> {
    let _nest = archive::Nest::enter(opts).ok_or(Error::TooLarge("attachment nesting"))?;
    let mut messages = Vec::new();
    let mut truncated = false;
    for raw in split_mbox(bytes) {
        check_deadline(opts)?;
        if !archive::take_member() || budget::exhausted() {
            truncated = true;
            break;
        }
        messages.push(mime::parse(&unescape_from(raw)));
    }
    let mut doc = finish(messages, opts)?;
    if truncated {
        doc.warnings
            .push("mbox_truncated: not every message was converted".into());
    }
    Ok(doc)
}

/// mbox：每封以行首的 `From ` 开始，前面是空行或文件开头。
fn split_mbox(bytes: &[u8]) -> Vec<&[u8]> {
    let mut starts = Vec::new();
    let mut line = 0usize;
    let mut prev_blank = true;
    while line < bytes.len() {
        let end = bytes[line..]
            .iter()
            .position(|&b| b == b'\n')
            .map_or(bytes.len(), |i| line + i);
        let text = &bytes[line..end];
        if prev_blank && text.starts_with(b"From ") {
            starts.push((line, (end + 1).min(bytes.len())));
        }
        prev_blank = text.is_empty() || text == b"\r";
        line = end + 1;
    }
    starts
        .iter()
        .enumerate()
        .map(|(i, &(_, body))| {
            let end = starts.get(i + 1).map_or(bytes.len(), |n| n.0);
            &bytes[body..end]
        })
        .collect()
}

/// mboxrd 把正文里行首的 `From ` 写成 `>From `：还原。
fn unescape_from(raw: &[u8]) -> Vec<u8> {
    let mut out = Vec::with_capacity(raw.len());
    for line in raw.split_inclusive(|&b| b == b'\n') {
        let quotes = line.iter().take_while(|&&b| b == b'>').count();
        if quotes > 0 && line[quotes..].starts_with(b"From ") {
            out.extend_from_slice(&line[1..]);
        } else {
            out.extend_from_slice(line);
        }
    }
    out
}

fn finish(messages: Vec<Message>, opts: &Options<'_>) -> Result<Converted, Error> {
    let title = messages
        .first()
        .map(|m| m.subject.trim().to_owned())
        .filter(|s| !s.is_empty());
    let metadata = messages.first().map_or_else(Vec::new, |m| {
        [("creator", &m.from), ("to", &m.to), ("date", &m.date)]
            .into_iter()
            .filter(|(_, v)| !v.is_empty())
            .map(|(k, v)| (k.to_owned(), v.clone()))
            .collect()
    });
    let single = messages.len() == 1;
    let parts = messages.len();
    let mut out = String::new();
    let mut warnings = Vec::new();
    for m in messages {
        check_deadline(opts)?;
        render(
            &m,
            if single { 1 } else { 2 },
            opts,
            &mut out,
            &mut warnings,
        );
    }
    Ok(Converted {
        markdown: out,
        title,
        metadata,
        warnings,
        parts,
    })
}

fn render(
    m: &Message,
    level: usize,
    opts: &Options<'_>,
    out: &mut String,
    warnings: &mut Vec<String>,
) {
    let start = out.len();
    let hashes = "#".repeat(level);
    if !out.is_empty() {
        out.push('\n');
    }
    let subject = m.subject.trim();
    if !subject.is_empty() {
        out.push_str(&format!("{hashes} {}\n\n", md::escape(subject)));
    }
    let meta: Vec<String> = [
        ("From", &m.from),
        ("To", &m.to),
        ("Cc", &m.cc),
        ("Date", &m.date),
    ]
    .into_iter()
    .filter(|(_, v)| !v.trim().is_empty())
    .map(|(k, v)| format!("**{k}:** {}", md::escape(v.trim())))
    .collect();
    if !meta.is_empty() {
        out.push_str(&meta.join("  \n"));
        out.push_str("\n\n");
    }
    let body = match (opts.html, m.html.trim().is_empty()) {
        (Some(html), false) => html(&m.html, ""),
        _ => m.text.replace("\r\n", "\n"),
    };
    let body = body.trim();
    if !body.is_empty() {
        out.push_str(body);
        out.push('\n');
    }
    budget::spend(out.len() - start);
    warnings.extend(m.notes.iter().cloned());
    if m.truncated {
        warnings
            .push("email_truncated: the message nests too deep; some parts were left out".into());
    }
    attachments(m, level, opts, out, warnings);
}

fn attachments(
    m: &Message,
    level: usize,
    opts: &Options<'_>,
    out: &mut String,
    warnings: &mut Vec<String>,
) {
    if m.attachments.is_empty() {
        return;
    }
    let sub = "#".repeat((level + 1).min(6));
    out.push_str(&format!("\n{sub} Attachments\n\n"));
    for a in &m.attachments {
        let kind = if a.content_type.is_empty() {
            String::new()
        } else {
            format!(", {}", md::escape(&a.content_type))
        };
        out.push_str(&format!(
            "- {} ({} bytes{kind})\n",
            md::escape(&a.name),
            a.data.len()
        ));
    }
    let Some(convert) = opts.member else {
        return;
    };
    for a in &m.attachments {
        if budget::exhausted() || !archive::take_member() {
            warnings.push("attachments_truncated: not every attachment was converted".into());
            return;
        }
        match convert(&a.data, &a.name, budget::left()) {
            Ok(Some(doc)) => {
                warnings.extend(doc.warnings.into_iter().map(|w| format!("{}: {w}", a.name)));
                let mut body = doc.markdown;
                if body.trim().is_empty() {
                    continue;
                }
                budget::clip(&mut body);
                out.push_str(&format!("\n{sub} {}\n\n", md::escape(&a.name)));
                out.push_str(body.trim_end());
                out.push('\n');
            }
            Ok(None) => {}
            Err(e) => warnings.push(format!("{}: {e}", a.name)),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{Format, convert};

    fn html(x: &str, _: &str) -> String {
        x.replace("<p>", "").replace("</p>", "")
    }

    fn member(bytes: &[u8], name: &str, room: usize) -> Result<Option<Converted>, Error> {
        let mut opts = Options {
            html: Some(&html),
            member: Some(&member),
            ..Options::default()
        };
        opts.limits.max_output_bytes = room;
        match Format::detect(bytes, Some(name), None) {
            Some(Format::Text) => {
                crate::convert_text(std::str::from_utf8(bytes).unwrap(), Format::Text, &opts)
                    .map(Some)
            }
            Some(f @ Format::Eml) => convert(bytes, f, &opts).map(Some),
            _ => Ok(None),
        }
    }

    fn opts() -> Options<'static> {
        Options {
            html: Some(&html),
            member: Some(&member),
            ..Options::default()
        }
    }

    #[test]
    fn eml_with_html_body_and_attachment() {
        let d = convert(
            b"Subject: Hi\r\nFrom: =?utf-8?B?5byg5LiJ?= <z@x>\r\nTo: a@x\r\nDate: Mon, 1 Jan 2024 00:00:00 +0000\r\n\
              Content-Type: multipart/mixed; boundary=b\r\n\r\n\
              --b\r\nContent-Type: text/html\r\n\r\n<p>Body</p>\r\n\
              --b\r\nContent-Disposition: attachment; filename=n.txt\r\nContent-Type: text/plain\r\n\r\nnote\r\n\
              --b\r\nContent-Disposition: attachment; filename=p.bin\r\nContent-Type: application/octet-stream\r\n\r\n\0\r\n--b--\r\n",
            Format::Eml,
            &opts(),
        )
        .unwrap();
        assert_eq!(d.title.as_deref(), Some("Hi"));
        assert!(
            d.metadata
                .contains(&("creator".into(), "张三 <z@x>".into()))
        );
        assert_eq!(
            d.markdown,
            "# Hi\n\n**From:** 张三 \\<z@x\\>  \n**To:** a@x  \n**Date:** Mon, 1 Jan 2024 00:00:00 +0000\n\nBody\n\n\
             ## Attachments\n\n- n.txt (4 bytes, text/plain)\n- p.bin (1 bytes, application/octet-stream)\n\n## n.txt\n\nnote\n"
        );
    }

    #[test]
    fn attached_mail_recurses_until_the_shared_depth_limit() {
        let mut mail = b"Subject: bottom\n\ndeep text\n".to_vec();
        for i in 0..5 {
            let mut outer = format!(
                "Subject: s{i}\nContent-Type: multipart/mixed; boundary=b{i}\n\n--b{i}\nContent-Type: message/rfc822\nContent-Disposition: attachment; filename=m.eml\n\n"
            )
            .into_bytes();
            outer.extend_from_slice(&mail);
            outer.extend_from_slice(format!("\n--b{i}--\n").as_bytes());
            mail = outer;
        }
        let d = convert(&mail, Format::Eml, &opts()).unwrap();
        assert!(!d.markdown.contains("deep text"));
        assert!(
            d.warnings.iter().any(|w| w.contains("nesting")),
            "{:?}",
            d.warnings
        );
    }

    #[test]
    fn mbox_splits_messages_and_restores_from_lines() {
        let d = convert(
            b"From a@x Mon Jan 1\nSubject: One\n\nbody 1\n>From here\n\nFrom b@x Mon Jan 2\nSubject: Two\n\nbody 2\n",
            Format::Mbox,
            &opts(),
        )
        .unwrap();
        assert_eq!(d.parts, 2);
        assert_eq!(d.title.as_deref(), Some("One"));
        assert_eq!(
            d.markdown,
            "## One\n\nbody 1\nFrom here\n\n## Two\n\nbody 2\n"
        );
        // 没有分隔行的文件：一封也没有，不报错
        assert_eq!(
            convert(b"no separators", Format::Mbox, &opts())
                .unwrap()
                .parts,
            0
        );
    }
}
