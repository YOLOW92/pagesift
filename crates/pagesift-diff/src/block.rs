//! Markdown 切块、规范化与哈希。

use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use unicode_normalization::{UnicodeNormalization, is_nfc_quick};

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum BlockKind {
    Heading,
    Paragraph,
    ListItem,
    TableRow,
    CodeBlock,
}

#[derive(Debug, Clone)]
pub struct Block {
    pub kind: BlockKind,
    /// 原文（去掉首尾空白）。
    pub text: String,
    /// 规范化后的文本，哈希基于它。
    pub norm: String,
}

impl Block {
    /// 种类加规范化文本的 SHA-256。
    pub fn hash(&self) -> [u8; 32] {
        let mut h = Sha256::new();
        h.update([self.kind as u8]);
        h.update(self.norm.as_bytes());
        h.finalize().into()
    }
}

fn is_zero_width(c: char) -> bool {
    matches!(
        c,
        '\u{200B}' | '\u{200C}' | '\u{200D}' | '\u{2060}' | '\u{FEFF}' | '\u{00AD}'
    )
}

fn nfc(s: String) -> String {
    if s.is_ascii()
        || matches!(
            is_nfc_quick(s.chars()),
            unicode_normalization::IsNormalized::Yes
        )
    {
        s
    } else {
        s.nfc().collect()
    }
}

/// 去零位宽字符、NFC、折叠空白；代码块只去行尾空白，保留缩进。
pub fn normalize(s: &str, code: bool) -> String {
    let cleaned: String = if s.chars().any(is_zero_width) {
        nfc(s.chars().filter(|&c| !is_zero_width(c)).collect())
    } else {
        nfc(s.to_string())
    };
    if code {
        let lines: Vec<&str> = cleaned.lines().map(str::trim_end).collect();
        lines.join("\n").trim_matches('\n').to_string()
    } else {
        let mut out = String::with_capacity(cleaned.len());
        for w in cleaned.split_whitespace() {
            if !out.is_empty() {
                out.push(' ');
            }
            out.push_str(w);
        }
        out
    }
}

fn make(kind: BlockKind, text: String) -> Option<Block> {
    let norm = normalize(&text, kind == BlockKind::CodeBlock);
    if norm.is_empty() {
        return None;
    }
    Some(Block {
        kind,
        text: text.trim().to_string(),
        norm,
    })
}

fn is_heading(t: &str) -> bool {
    let hashes = t.bytes().take_while(|&b| b == b'#').count();
    (1..=6).contains(&hashes) && t[hashes..].chars().next().is_none_or(char::is_whitespace)
}

fn is_list_item(t: &str) -> bool {
    let mut it = t.chars();
    match it.next() {
        Some('-' | '*' | '+') => it.next().is_some_and(char::is_whitespace),
        Some(c) if c.is_ascii_digit() => {
            let rest = t.trim_start_matches(|c: char| c.is_ascii_digit());
            let mut r = rest.chars();
            matches!(r.next(), Some('.' | ')')) && r.next().is_some_and(char::is_whitespace)
        }
        _ => false,
    }
}

fn is_hr(t: &str) -> bool {
    let mut it = t.chars().filter(|c| !c.is_whitespace());
    let Some(first) = it.next() else { return false };
    matches!(first, '-' | '*' | '_' | '=') && {
        let mut n = 1;
        it.all(|c| {
            n += 1;
            c == first
        }) && n >= 3
    }
}

fn is_table_separator(t: &str) -> bool {
    t.contains('-') && t.chars().all(|c| matches!(c, '|' | '-' | ':' | ' ' | '\t'))
}

fn fence_of(t: &str) -> Option<(char, usize)> {
    let c = t.chars().next()?;
    if c != '`' && c != '~' {
        return None;
    }
    let n = t.chars().take_while(|&x| x == c).count();
    (n >= 3).then_some((c, n))
}

pub fn split_blocks(md: &str) -> Vec<Block> {
    let mut blocks = Vec::new();
    let mut cur: Option<(BlockKind, String)> = None;
    let mut fence: Option<(char, usize, String)> = None;

    fn flush(cur: &mut Option<(BlockKind, String)>, blocks: &mut Vec<Block>) {
        if let Some((k, t)) = cur.take() {
            blocks.extend(make(k, t));
        }
    }
    fn append(cur: &mut Option<(BlockKind, String)>, line: &str) {
        if let Some((_, t)) = cur {
            t.push('\n');
            t.push_str(line);
        }
    }

    for line in md.lines() {
        let t = line.trim();
        if let Some((c, n, buf)) = &mut fence {
            buf.push('\n');
            buf.push_str(line);
            if fence_of(t).is_some_and(|(c2, n2)| c2 == *c && n2 >= *n)
                && t.chars().all(|x| x == *c)
            {
                let text = std::mem::take(buf);
                fence = None;
                blocks.extend(make(BlockKind::CodeBlock, text));
            }
            continue;
        }
        if t.is_empty() {
            flush(&mut cur, &mut blocks);
        } else if let Some((c, n)) = fence_of(t) {
            flush(&mut cur, &mut blocks);
            fence = Some((c, n, line.to_string()));
        } else if is_heading(t) {
            flush(&mut cur, &mut blocks);
            blocks.extend(make(BlockKind::Heading, t.to_string()));
        } else if t.starts_with('|') {
            flush(&mut cur, &mut blocks);
            if !is_table_separator(t) {
                blocks.extend(make(BlockKind::TableRow, t.to_string()));
            }
        } else if is_hr(t) {
            flush(&mut cur, &mut blocks);
        } else if is_list_item(t) {
            flush(&mut cur, &mut blocks);
            cur = Some((BlockKind::ListItem, t.to_string()));
        } else if cur.is_some() {
            append(&mut cur, t);
        } else {
            cur = Some((BlockKind::Paragraph, t.to_string()));
        }
    }
    if let Some((_, _, buf)) = fence {
        blocks.extend(make(BlockKind::CodeBlock, buf));
    }
    flush(&mut cur, &mut blocks);
    blocks
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn splits_kinds() {
        let md = "# T\n\npara one\nline two\n\n- a\n- b\n  more\n\n| h | v |\n|---|---|\n| x | 1 |\n\n```rs\nlet a = 1;\n\nlet b = 2;\n```\n";
        let kinds: Vec<_> = split_blocks(md).iter().map(|b| b.kind).collect();
        use BlockKind::*;
        assert_eq!(
            kinds,
            [
                Heading, Paragraph, ListItem, ListItem, TableRow, TableRow, CodeBlock
            ]
        );
    }

    #[test]
    fn normalizes() {
        let a = split_blocks("caf\u{e9}  \u{200B}au\u{a0}lait");
        let b = split_blocks("cafe\u{301} au lait");
        assert_eq!(a[0].hash(), b[0].hash());
    }
}
