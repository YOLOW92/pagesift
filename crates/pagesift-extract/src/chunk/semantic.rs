//! 语义与标点边界探测断句器。
//! 基于多层级标点、空白换行与中日韩全角标点，无需载入神经网络向量模型。

use std::ops::Range;

pub const LATIN_END: &[char] = &['.', '!', '?', '…'];
pub const CJK_END: &[char] = &['。', '！', '？', '｡'];
pub const CLOSERS: &[char] = &[
    '"', '\'', ')', ']', '”', '’', '」', '』', '）', '》', '】', '〉',
];
pub const ABBREVIATIONS: &[&str] = &[
    "co", "dr", "e.g", "etc", "fig", "i.e", "inc", "jr", "ltd", "mr", "mrs", "ms", "no", "prof",
    "sr", "st", "vs",
];

/// 句子边界切分：识别拉丁标点跟空白、中日韩句末标点，并考虑缩写（如 Mr.、Dr.、e.g.）。
pub fn split_sentences(text: &str) -> Vec<Range<usize>> {
    let mut cuts = Vec::new();
    let mut it = text.char_indices().peekable();
    while let Some((i, c)) = it.next() {
        if c == '\n' {
            cuts.push(i + 1);
            continue;
        }
        let cjk = CJK_END.contains(&c);
        if !cjk && !LATIN_END.contains(&c) {
            continue;
        }
        let mut end = i + c.len_utf8();
        while let Some(&(j, n)) = it.peek() {
            if CLOSERS.contains(&n) || LATIN_END.contains(&n) || CJK_END.contains(&n) {
                end = j + n.len_utf8();
                it.next();
            } else {
                break;
            }
        }
        let at_space = text[end..].chars().next().is_none_or(char::is_whitespace);
        if cjk || (at_space && !(c == '.' && is_abbreviation(&text[..i]))) {
            cuts.push(end);
        }
    }
    cuts.push(text.len());
    let mut start = 0;
    cuts.into_iter()
        .filter_map(|end| {
            let r = start..end;
            start = end;
            (!r.is_empty()).then_some(r)
        })
        .collect()
}

/// 检查点号前是否是常见英文缩写或单字母大写简写（如 U.S.）。
pub fn is_abbreviation(before: &str) -> bool {
    let word = before.rsplit(char::is_whitespace).next().unwrap_or("");
    let word = word.trim_start_matches(['(', '"', '\'']);
    let mut chars = word.chars();
    if matches!((chars.next(), chars.next()), (Some(c), None) if c.is_alphabetic() && c.is_uppercase())
    {
        return true;
    }
    ABBREVIATIONS.iter().any(|a| word.eq_ignore_ascii_case(a))
}
