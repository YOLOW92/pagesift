//! 标题文字的比对：归一化、去章节编号、判断段落和书签标题对不对得上，以及把粘着正文的标题拆出来。

use crate::layout::{Para, join_text};

/// 文字对上的程度，越小越好。
#[derive(Debug, Clone, Copy, PartialEq, PartialOrd)]
pub(super) enum Fit {
    Exact,
    /// 段落以标题开头，后面还有正文：标题占段落归一化文字的前 n 个字符。
    Prefix(usize),
    /// 段落是标题的大半截（标题折行成了两段，或者书签多写了几个字）。
    Partial,
}

/// 一段文字的两种归一化形式：整句，和去掉开头章节编号之后。
pub(super) struct Titled {
    pub(super) full: String,
    bare: String,
    /// 开头是“单个大写字母 + 空格”时，去掉这个字母后的归一化文字。
    raw_letter_rest: Option<String>,
}

impl Titled {
    pub(super) fn new(s: &str) -> Self {
        let t = s.trim_start();
        let mut it = t.chars();
        let raw_letter_rest = match (it.next(), it.next()) {
            (Some(c), Some(' ')) if c.is_ascii_uppercase() => Some(norm(&t[2..])),
            _ => None,
        };
        Titled {
            full: norm(s),
            bare: norm(strip_numbering(s)),
            raw_letter_rest,
        }
    }
}

pub(super) fn fit(title: &Titled, text: &Titled) -> Option<Fit> {
    let (t, x) = (&title.bare, &text.bare);
    if text.full.is_empty() {
        return None;
    }
    if text.full == title.full || (t.chars().count() >= 2 && x == t) {
        return Some(Fit::Exact);
    }
    // 附录的 “A Appendix”：单个大写字母当编号，只在和书签标题完全对上时才这么认
    if let Some(rest) = text.raw_letter_rest.as_deref()
        && t.chars().count() >= 2
        && rest == t
    {
        return Some(Fit::Exact);
    }
    if text.full.starts_with(title.full.as_str()) {
        return Some(Fit::Prefix(title.full.chars().count()));
    }
    if t.chars().count() >= 2 && x.starts_with(t.as_str()) {
        let skipped = text.full.chars().count() - x.chars().count();
        return Some(Fit::Prefix(skipped + t.chars().count()));
    }
    let (tn, xn) = (t.chars().count(), x.chars().count());
    if xn >= 2 && t.starts_with(x.as_str()) && xn * 3 >= tn * 2 {
        return Some(Fit::Partial);
    }
    None
}

fn half(c: char) -> char {
    match c {
        '\u{FF01}'..='\u{FF5E}' => char::from_u32(c as u32 - 0xFEE0).unwrap_or(c),
        _ => c,
    }
}

/// 归一化：全角 ASCII 转半角，只留字母、数字和汉字等文字，小写。
fn norm(s: &str) -> String {
    s.chars()
        .map(half)
        .filter(|c| c.is_alphanumeric())
        .flat_map(char::to_lowercase)
        .collect()
}

/// 去掉开头的章节编号：`3.2.1`、`A.1.`、`IV.`、`(1)`、`（１）`、`１．`、`第3章`、`Chapter 2`……
/// 纯数字编号后面可以直接跟空白或 CJK 文字；字母编号（附录 A、罗马数字）必须带标点，免得把 “A New …” 的 A 当编号。
pub(crate) fn strip_numbering(s: &str) -> &str {
    let t = s.trim_start();
    let chars: Vec<(usize, char)> = t.char_indices().map(|(i, c)| (i, half(c))).collect();
    let at = |k: usize| chars.get(k).map(|&(_, c)| c);
    let rest = |k: usize| t[chars.get(k).map_or(t.len(), |&(i, _)| i)..].trim_start();
    // 第N章 / 第N節 / 第N部
    if at(0) == Some('第') {
        let mut j = 1;
        while at(j).is_some_and(|c| c.is_ascii_digit() || "一二三四五六七八九十百".contains(c)) {
            j += 1;
        }
        if j > 1 && at(j).is_some_and(|c| "章節节部編编条".contains(c)) {
            return rest(j + 1);
        }
        return t;
    }
    for word in ["Chapter ", "CHAPTER ", "Section ", "SECTION ", "Part ", "PART ", "Appendix ", "APPENDIX "] {
        if let Some(r) = t.strip_prefix(word) {
            let tail = r.trim_start_matches(|c: char| c.is_ascii_alphanumeric() || c == '.' || c == ':');
            if tail.len() < r.len() {
                return tail.trim_start();
            }
        }
    }
    // (1) （１） [1]
    if matches!(at(0), Some('(' | '[')) {
        let mut j = 1;
        while j <= 4 && at(j).is_some_and(|c| c.is_ascii_alphanumeric()) {
            j += 1;
        }
        if j > 1 && matches!(at(j), Some(')' | ']')) {
            return rest(j + 1);
        }
        return t;
    }
    // 1 / 1.2.3 / A / A.1 / IV
    let mut k = 0;
    if at(0).is_some_and(|c| c.is_ascii_digit()) {
        while at(k).is_some_and(|c| c.is_ascii_digit()) {
            k += 1;
        }
        // 章节编号每段最多两位：`1882年`、`2024 Annual Report` 的年份不是编号
        if k > 2 {
            return t;
        }
    } else if at(0).is_some_and(|c| c.is_ascii_uppercase()) {
        let mut j = 0;
        while at(j).is_some_and(|c| "IVXLC".contains(c)) {
            j += 1;
        }
        k = j.max(1);
    } else {
        return t;
    }
    // `.数字` 接着算编号的一段，但三位以上的数字是正文（`１．2025年３月期`）
    let mut long_tail = false;
    while at(k) == Some('.') && at(k + 1).is_some_and(|c| c.is_ascii_digit()) {
        let mut j = k + 1;
        while at(j).is_some_and(|c| c.is_ascii_digit()) {
            j += 1;
        }
        if j - (k + 1) > 2 {
            long_tail = true;
            break;
        }
        k = j;
    }
    // 带数字的编号（`3`、`A.1`）后面直接跟空白就算；纯字母的（`A`、`IV`）必须带标点
    let numeric = chars[..k].iter().any(|&(_, c)| c.is_ascii_digit());
    let punct = matches!(at(k), Some('.' | ')' | ':' | '、'));
    let end = if punct { k + 1 } else { k };
    match at(end) {
        // 整行只有编号（`C.`、`3.1`）
        None if numeric || punct => "",
        Some(c) if c.is_whitespace() && (numeric || punct) => rest(end),
        // 编号后紧跟文字：带标点（`1.Foo`、`１．わが国`、`A.Foo`），或纯数字接 CJK
        Some(c) if punct && (c.is_alphabetic() || long_tail) => rest(end),
        Some(c) if numeric && !punct && c.is_alphabetic() && !c.is_ascii() => rest(end),
        _ => t,
    }
}

/// 在原文里数到第 n 个归一化字符之后切开：前半段留在原段落，后半段成新段落。
pub(super) fn split_after(paras: &mut Vec<Para>, i: usize, n: usize) {
    let text = &paras[i].text;
    let mut count = 0;
    let mut cut = None;
    for (pos, c) in text.char_indices() {
        if count >= n {
            cut = Some(pos);
            break;
        }
        let c = half(c);
        if c.is_alphanumeric() {
            count += c.to_lowercase().count();
        }
    }
    let Some(cut) = cut else { return };
    // 标题后面紧跟的右括号、句点之类留在标题里
    let tail_start = text[cut..]
        .char_indices()
        .find(|(_, c)| c.is_alphanumeric() || *c == '$')
        .map_or(text.len(), |(k, _)| cut + k);
    let head = text[..tail_start].trim_end().to_string();
    let tail = text[tail_start..].trim_start().to_string();
    if head.is_empty() || tail.is_empty() {
        return;
    }
    let mut rest = paras[i].clone();
    rest.text = String::new();
    join_text(&mut rest.text, &tail);
    rest.bold = false;
    rest.block_start = false;
    paras[i].text = head;
    paras[i].lines = 1;
    paras.insert(i + 1, rest);
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn numbering() {
        assert_eq!(strip_numbering("3.2.1 Scaled Dot-Product"), "Scaled Dot-Product");
        assert_eq!(strip_numbering("B.3.1. RL Data"), "RL Data");
        assert_eq!(strip_numbering("IV. Results"), "Results");
        assert_eq!(strip_numbering("１．わが国の経済"), "わが国の経済");
        assert_eq!(strip_numbering("（２）物価の中心的な見通し"), "物価の中心的な見通し");
        assert_eq!(strip_numbering("第3章 概要"), "概要");
        assert_eq!(strip_numbering("Introduction"), "Introduction");
        assert_eq!(strip_numbering("A New Approach"), "A New Approach");
        assert_eq!(strip_numbering("I think"), "I think");
        assert_eq!(strip_numbering("2 Background"), "Background");
        assert_eq!(strip_numbering("１．2025年３月期の連結業績"), "2025年３月期の連結業績");
        assert_eq!(strip_numbering("1882年（明治15年）"), "1882年（明治15年）");
        assert_eq!(strip_numbering("C."), "");
        assert_eq!(strip_numbering("CHAPTER 2"), "");
        assert_eq!(strip_numbering("A"), "A");
    }

    #[test]
    fn fits() {
        let f = |a: &str, b: &str| fit(&Titled::new(a), &Titled::new(b));
        assert_eq!(f("3.1 Encoder and Decoder Stacks", "3.1 Encoder and Decoder Stacks"), Some(Fit::Exact));
        assert_eq!(f("Encoder and Decoder Stacks", "3.1 Encoder and Decoder Stacks"), Some(Fit::Exact));
        assert_eq!(f("RL Data", "B.3.1. RL Data"), Some(Fit::Exact));
        assert_eq!(f("Background", "A. Background"), Some(Fit::Exact));
        assert_eq!(f("Contributions", "A.1 Contributions"), Some(Fit::Exact));
        assert_eq!(f("Appendix", "A Appendix"), Some(Fit::Exact));
        assert_eq!(f("＜概要＞", "＜概要＞先行きのわが国経済"), Some(Fit::Prefix(2)));
        assert!(f("Introduction", "In this paper we").is_none());
    }
}
