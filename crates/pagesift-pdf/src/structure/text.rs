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
    /// 开头的章节编号占了几个归一化字符（`full` 比 `bare` 多出来的那截）。
    skipped: usize,
}

impl Titled {
    pub(super) fn new(s: &str) -> Self {
        Self::with_limit(s, usize::MAX)
    }

    /// 只归一化到 `limit` 个字符：和不超过 `limit - 1` 个字符的标题比对时，结果与看全文相同
    /// （全等、以标题开头、标题以它开头这几种比法都只看到标题长度再多一个字符）。
    pub(super) fn with_limit(s: &str, limit: usize) -> Self {
        let t = s.trim_start();
        let mut it = t.chars();
        let raw_letter_rest = match (it.next(), it.next()) {
            (Some(c), Some(' ')) if c.is_ascii_uppercase() => Some(norm_n(&t[2..], limit)),
            _ => None,
        };
        // 去编号的结果总是原文的后缀，编号那截就是前面剩下的部分
        let bare = strip_numbering(s);
        Titled {
            full: norm_n(s, limit),
            bare: norm_n(bare, limit),
            raw_letter_rest,
            skipped: norm_len(&s[..s.len() - bare.len()]),
        }
    }
}

/// 段落和书签比对前的廉价预筛：只看段落的第一个有效字符。真要比时才建 `Titled`（懒加载）。
pub(super) struct Lazy {
    /// 归一化后的第一个字符。
    first: Option<char>,
    /// 开头可能是章节编号（数字、括号、`第`、大写字母）：去编号会改变首字符，预筛不能用。
    maybe_numbered: bool,
    titled: std::cell::OnceCell<Titled>,
}

impl Lazy {
    pub(super) fn new(s: &str) -> Self {
        let first = s.chars().map(half).find(|c| c.is_alphanumeric()).and_then(|c| c.to_lowercase().next());
        let maybe_numbered = s
            .trim_start()
            .chars()
            .next()
            .map(half)
            .is_some_and(|c| c.is_ascii_digit() || c.is_ascii_uppercase() || matches!(c, '(' | '[' | '第'));
        Lazy { first, maybe_numbered, titled: std::cell::OnceCell::new() }
    }

    /// 和 `fit(title, &Titled::with_limit(s, limit))` 结果相同；首字符就对不上的直接返回 None。
    pub(super) fn fit(&self, title: &Titled, s: &str, limit: usize) -> Option<Fit> {
        let first = |x: &str| x.chars().next();
        if !self.maybe_numbered
            && self.first != first(&title.full)
            && (title.bare.is_empty() || self.first != first(&title.bare))
        {
            return None;
        }
        fit(title, self.titled.get_or_init(|| Titled::with_limit(s, limit)))
    }
}

pub(super) fn fit(title: &Titled, text: &Titled) -> Option<Fit> {
    let (t, x) = (&title.bare, &text.bare);
    if text.full.is_empty() {
        return None;
    }
    // 下面每一种对上的方式都要求某一对首字符相同；先比首字符，绝大多数行在这里就排除了
    let first = |s: &str| s.chars().next();
    if !title.full.is_empty()
        && first(&text.full) != first(&title.full)
        && (t.is_empty() || (first(x) != first(t) && text.raw_letter_rest.as_deref().and_then(first) != first(t)))
    {
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
        return Some(Fit::Prefix(text.skipped + t.chars().count()));
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

/// 归一化：全角 ASCII 转半角，只留字母、数字和汉字等文字，小写；最多取 `limit` 个字符。
fn norm_n(s: &str, limit: usize) -> String {
    s.chars()
        .map(half)
        .filter(|c| c.is_alphanumeric())
        .flat_map(char::to_lowercase)
        .take(limit)
        .collect()
}

fn norm_len(s: &str) -> usize {
    s.chars().map(half).filter(|c| c.is_alphanumeric()).flat_map(char::to_lowercase).count()
}

/// 去掉开头的章节编号：`3.2.1`、`A.1.`、`IV.`、`(1)`、`（１）`、`１．`、`第3章`、`Chapter 2`……
/// 纯数字编号后面可以直接跟空白或 CJK 文字；字母编号（附录 A、罗马数字）必须带标点，免得把 “A New …” 的 A 当编号。
pub(crate) fn strip_numbering(s: &str) -> &str {
    let t = s.trim_start();
    // 编号只在开头：先只看前 64 个字符；判断时碰到了这个边界才看全文（结果与一开始就看全文相同）
    const HEAD: usize = 64;
    let head: Vec<(usize, char)> = t.char_indices().take(HEAD).map(|(i, c)| (i, half(c))).collect();
    if head.len() < HEAD {
        return strip_numbering_in(t, &head).0;
    }
    match strip_numbering_in(t, &head) {
        (r, false) => r,
        (_, true) => {
            let all: Vec<(usize, char)> = t.char_indices().map(|(i, c)| (i, half(c))).collect();
            strip_numbering_in(t, &all).0
        }
    }
}

/// 返回去掉编号后的文字，以及判断时有没有用到 `chars` 的最后一个位置之后（截断时要看全文重算）。
fn strip_numbering_in<'a>(t: &'a str, chars: &[(usize, char)]) -> (&'a str, bool) {
    let edge = std::cell::Cell::new(false);
    let at = |k: usize| {
        if k + 1 >= chars.len() {
            edge.set(true);
        }
        chars.get(k).map(|&(_, c)| c)
    };
    let rest = |k: usize| {
        if k >= chars.len() {
            edge.set(true);
        }
        t[chars.get(k).map_or(t.len(), |&(i, _)| i)..].trim_start()
    };
    let r = strip_numbering_body(t, chars, &at, &rest);
    (r, edge.get())
}

fn strip_numbering_body<'a>(
    t: &'a str,
    chars: &[(usize, char)],
    at: &dyn Fn(usize) -> Option<char>,
    rest: &dyn Fn(usize) -> &'a str,
) -> &'a str {
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

    /// 截断版（只看开头 64 个字符、只归一化到标题长度、首字符预筛）和看全文的结果相同。
    #[test]
    fn shortcuts_match_full_computation() {
        let long_num = format!("{} 标题", (1..40).map(|i| (i % 10).to_string()).collect::<Vec<_>>().join("."));
        let body = "Introduction to the method and a very long paragraph of body text that goes on ".repeat(5);
        let texts = [
            "3.2 Results",
            "（１）概要",
            "第3章 方法",
            "A Appendix",
            "IV. Discussion",
            "1882年の出来事",
            long_num.as_str(),
            "1.2.3.4.5.6.7.8.9.1.2.3.4.5.6.7.8.9.1.2.3.4.5.6.7.8.9.1.2.3.4.5.6.7.8.9",
            body.as_str(),
            "intro text without number",
            "",
        ];
        for t in texts {
            let all: Vec<(usize, char)> = t.trim_start().char_indices().map(|(i, c)| (i, half(c))).collect();
            assert_eq!(strip_numbering(t), strip_numbering_in(t.trim_start(), &all).0, "{t}");
        }
        for title in ["Results", "3.2 Results", "Introduction", "概要", "Appendix", "Discussion", "方法"] {
            let tt = Titled::new(title);
            let limit = tt.full.chars().count() + 1;
            for t in texts {
                let lazy = Lazy::new(t);
                assert_eq!(lazy.fit(&tt, t, limit), fit(&tt, &Titled::new(t)), "{title} / {t}");
            }
        }
    }

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
