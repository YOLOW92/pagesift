//! 字幕（SRT、WebVTT，W3C WebVTT）转文字稿：去掉序号、时间轴、样式块和行内标签，
//! 说话人写成 `名字: `，滚动字幕里紧挨着重复的行只留一次。

use crate::md::{self, Block};
use crate::{Converted, Error, Options, budget, check_deadline};

/// 滚动字幕一行通常出现两次，和最近这么多行比较就够了。
const DEDUP_WINDOW: usize = 2;

pub(crate) fn convert(text: &str, opts: &Options<'_>) -> Result<Converted, Error> {
    let normalized = text
        .strip_prefix('\u{feff}')
        .unwrap_or(text)
        .replace("\r\n", "\n")
        .replace('\r', "\n");
    let mut lines: Vec<String> = Vec::new();
    let mut cues = 0;
    let mut speaker = String::new();
    let mut in_cue = false;
    for (i, block) in normalized.split("\n\n").enumerate() {
        if i % 256 == 0 {
            check_deadline(opts)?;
        }
        let block_lines: Vec<&str> = block.lines().collect();
        let Some(first) = block_lines.first() else {
            continue;
        };
        let timing = block_lines.iter().position(|l| l.contains("-->"));
        let body = match timing {
            Some(t) => {
                cues += 1;
                in_cue = true;
                &block_lines[t + 1..]
            }
            None if first.starts_with("WEBVTT")
                || first.starts_with("NOTE")
                || first.starts_with("STYLE")
                || first.starts_with("REGION") =>
            {
                in_cue = false;
                continue;
            }
            // 同一条字幕中间夹了空行时，后面的文字还属于它
            None if in_cue => &block_lines[..],
            None => continue,
        };
        for raw in body {
            let (who, clean) = clean_line(raw);
            if let Some(who) = who
                && who != speaker
            {
                speaker = who;
                push(&mut lines, format!("{speaker}: {clean}"));
                continue;
            }
            if !clean.is_empty() && !is_cue_number(&clean) {
                push(&mut lines, clean);
            }
        }
        if budget::exhausted() {
            break;
        }
    }
    let escaped: Vec<String> = lines.iter().map(|l| md::escape(l)).collect();
    let mut markdown = if escaped.is_empty() {
        String::new()
    } else {
        md::render(&[Block::Para(escaped.join("\n"))])
    };
    budget::clip(&mut markdown);
    Ok(Converted {
        markdown,
        parts: cues,
        ..Default::default()
    })
}

fn push(lines: &mut Vec<String>, line: String) {
    let start = lines.len().saturating_sub(DEDUP_WINDOW);
    if !lines[start..].contains(&line) {
        budget::spend(line.len());
        lines.push(line);
    }
}

fn is_cue_number(s: &str) -> bool {
    s.bytes().all(|b| b.is_ascii_digit())
}

/// 一行字幕：去掉 `<...>` 标签和 `{\an8}` 样式，解码基本实体。`<v 名字>` 单独返回。
fn clean_line(raw: &str) -> (Option<String>, String) {
    let mut who = None;
    let mut out = String::with_capacity(raw.len());
    let mut rest = raw;
    while let Some(i) = rest.find(['<', '{']) {
        out.push_str(&rest[..i]);
        let close = if rest.as_bytes()[i] == b'<' { '>' } else { '}' };
        let Some(end) = rest[i..].find(close) else {
            out.push_str(&rest[i..]);
            rest = "";
            break;
        };
        let tag = &rest[i + 1..i + end];
        if let Some(name) = tag.strip_prefix("v ").or_else(|| tag.strip_prefix("v."))
            && who.is_none()
        {
            // `<v.loud Fred>`：点号后面是类名，真正的名字在空格后面
            let name = if tag.starts_with("v.") {
                name.split_once(' ').map_or("", |(_, b)| b)
            } else {
                name
            };
            let name = name.trim();
            if !name.is_empty() {
                who = Some(name.to_owned());
            }
        }
        rest = &rest[i + end + 1..];
    }
    out.push_str(rest);
    let out = out
        .replace("&amp;", "&")
        .replace("&lt;", "<")
        .replace("&gt;", ">")
        .replace("&nbsp;", " ")
        .replace("&lrm;", "")
        .replace("&rlm;", "");
    (who, out.trim().to_owned())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn run(s: &str) -> String {
        convert(s, &Options::default()).unwrap().markdown
    }

    #[test]
    fn srt_keeps_only_the_words() {
        let srt = "1\r\n00:00:01,000 --> 00:00:02,000\r\nHello <i>there</i>\r\n\r\n\
                   2\r\n00:00:02,000 --> 00:00:03,000\r\n{\\an8}world &amp; all\r\n";
        assert_eq!(run(srt), "Hello there\nworld & all\n");
    }

    #[test]
    fn webvtt_skips_headers_notes_and_styles() {
        let vtt = "WEBVTT - x\nKind: captions\n\nNOTE a comment\n\nSTYLE\n::cue {}\n\n\
                   id-1\n00:01.000 --> 00:02.000\nOne\n\n00:02.000 --> 00:03.000\nTwo\n";
        assert_eq!(run(vtt), "One\nTwo\n");
    }

    #[test]
    fn rolling_captions_are_not_repeated_and_speakers_are_named() {
        let vtt = "WEBVTT\n\n00:01.000 --> 00:02.000\nsay it\n\n\
                   00:02.000 --> 00:03.000\nsay it\nagain\n\n\
                   00:03.000 --> 00:04.000\n<v Ann>Hi\n\n\
                   00:04.000 --> 00:05.000\n<v Ann>Still me\n";
        assert_eq!(run(vtt), "say it\nagain\nAnn: Hi\nStill me\n");
    }

    #[test]
    fn empty_and_broken_input_do_not_fail() {
        assert_eq!(run(""), "");
        assert_eq!(run("WEBVTT\n"), "");
        assert_eq!(
            run("00:01 --> 00:02\n<unclosed"),
            "<unclosed\n".replace('<', "\\<")
        );
    }
}
