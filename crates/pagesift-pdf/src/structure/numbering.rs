//! 标题编号的样式（版面推断用）：写文档的人用编号表达层级，样式和深度就是层级的线索。
//!
//! 十进制编号自带深度（`3`、`3.1`、`3.1.2`）；日文公文、报告常用的 `第N章`、`【…】`、`＜…＞`、
//! `１．`、`（１）`、`①`、`ア．`，中文公文、年报的 `一、`、`（一）` 各算一个样式，层级按它们在文档里第一次出现的先后排。

/// 编号样式。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub(crate) enum Style {
    /// `3` / `3.1` / `3.1.2`，深度是段数。
    Decimal(u8),
    /// `A.` / `A.1`（附录），深度是段数。
    Letter(u8),
    Roman,
    /// `第N编/部`、`第N章`、`第N节`、`第N条` 和 Part / Chapter / Section：单位定上下级（0 编、1 章、2 节、3 条）
    Chapter(u8),
    /// `(1)` `（１）`
    Paren,
    /// `①`
    Circled,
    /// `ア．` `(ｱ)` 片假名编号
    Kana,
    /// 整行是 `【…】`
    Lenticular,
    /// 整行是 `＜…＞` `〈…〉`
    Angle,
    /// `■` `◆` `●` 之类的标记开头
    Mark,
    /// `一、` `十二、` 中文数字编号
    Han,
    /// `（一）` `(十二)` 括号里的中文数字
    HanParen,
}

impl Style {
    /// 自带深度的样式返回深度。
    pub(crate) fn depth(self) -> Option<u8> {
        match self {
            Style::Decimal(d) | Style::Letter(d) => Some(d),
            _ => None,
        }
    }
}

fn half(c: char) -> char {
    match c {
        '\u{FF01}'..='\u{FF5E}' => char::from_u32(c as u32 - 0xFEE0).unwrap_or(c),
        _ => c,
    }
}

/// 开头的编号样式和编号之后的正文。认不出编号返回 None。
pub(crate) fn numbering(text: &str) -> Option<(Style, &str)> {
    let t = text.trim();
    let first = t.chars().next()?;
    // 整行括起来的标题：【基本的見解】、＜概要＞（后面可能跟脚注号）
    for (open, close, style) in [
        ('【', '】', Style::Lenticular),
        ('＜', '＞', Style::Angle),
        ('〈', '〉', Style::Angle),
        ('《', '》', Style::Angle),
    ] {
        if first == open
            && let Some(end) = t.find(close)
        {
            let inner = &t[open.len_utf8()..end];
            let rest = t[end + close.len_utf8()..].trim();
            if !inner.is_empty()
                && rest
                    .chars()
                    .all(|c| half(c).is_ascii_digit() || c.is_whitespace())
            {
                return Some((style, inner));
            }
            return None;
        }
    }
    if "■□◆◇●▲▼◎★☆".contains(first) {
        let rest = t[first.len_utf8()..].trim_start();
        return (!rest.is_empty()).then_some((Style::Mark, rest));
    }
    if ('①'..='⑳').contains(&first) {
        return Some((Style::Circled, t[first.len_utf8()..].trim_start()));
    }
    if let Some(r) = han(t) {
        return r;
    }
    let rest = crate::structure::strip_numbering(t);
    if rest.len() == t.len() || rest.is_empty() {
        return None;
    }
    let head: String = t[..t.len() - rest.len()].chars().map(half).collect();
    let head = head.trim();
    let style = if head.starts_with('第') {
        let unit = head.chars().last().unwrap_or('章');
        Style::Chapter(match unit {
            '编' | '編' | '部' => 0,
            '节' | '節' => 2,
            '条' | '條' | '款' => 3,
            _ => 1,
        })
    } else if head.starts_with('(') || head.starts_with('[') {
        if head.starts_with('[') {
            // `[1]` 是参考文献编号，不是标题
            return None;
        }
        // 括号里只认一两位数字、单个字母或片假名；`(TS)`、`(OTC)` 是缩写
        let inner: Vec<char> = head.trim_matches(['(', ')', ' ']).chars().collect();
        let kana = |c: char| ('ｱ'..='ﾝ').contains(&c) || ('ア'..='ン').contains(&c);
        match inner.as_slice() {
            [c] if kana(*c) => Style::Kana,
            [c] if c.is_ascii_alphanumeric() => Style::Paren,
            [a, b] if a.is_ascii_digit() && b.is_ascii_digit() => Style::Paren,
            _ => return None,
        }
    } else if head.chars().next().is_some_and(|c| c.is_ascii_digit()) {
        Style::Decimal(segments(head))
    } else if head.chars().next().is_some_and(|c| c.is_ascii_uppercase()) {
        let letters = head.chars().take_while(|c| c.is_ascii_alphabetic()).count();
        if letters > 1 {
            Style::Roman
        } else {
            Style::Letter(segments(head))
        }
    } else {
        let unit =
            head.split_whitespace()
                .next()
                .and_then(|w| match w.to_ascii_lowercase().as_str() {
                    "part" => Some(0),
                    "chapter" | "appendix" => Some(1),
                    "section" => Some(2),
                    _ => None,
                })?;
        Style::Chapter(unit)
    };
    Some((style, rest))
}

/// 中文数字编号：`一、…`、`（一）…`（全角、半角括号都认）。编号后要有文字。
fn han(t: &str) -> Option<Option<(Style, &str)>> {
    let paren = t.starts_with(['(', '（']);
    let body = if paren {
        &t[t.chars().next()?.len_utf8()..]
    } else {
        t
    };
    let n = body.chars().take_while(|&c| han_digit(c)).count();
    if n == 0 {
        return None;
    }
    let digits_len: usize = body.chars().take(n).map(char::len_utf8).sum();
    let after = &body[digits_len..];
    let close = if paren {
        [')', '）'].iter().find(|&&c| after.starts_with(c))
    } else {
        ['、'].iter().find(|&&c| after.starts_with(c))
    };
    let &c = close?;
    let rest = after[c.len_utf8()..].trim_start();
    let style = if paren { Style::HanParen } else { Style::Han };
    Some((!rest.is_empty()).then_some((style, rest)))
}

/// 编号是不是从头数起的第一个（`1`、`一`、`(a)`、`①`、`ア`、`I.`、`第一章`）：下级编号每到一个新的上级条目下就重新从这里数起。
pub(crate) fn starts_at_one(text: &str) -> bool {
    let t: String = text.trim().chars().take(6).map(half).collect();
    let t = t.trim_start_matches(['(', '[', '第']);
    let mut it = t.chars();
    let (Some(a), b) = (it.next(), it.next()) else {
        return false;
    };
    let digit_follows = b.is_some_and(|c| c.is_ascii_digit() || han_digit(c));
    match a {
        '1' | '一' => !digit_follows,
        '①' | 'ア' | 'ｱ' => true,
        'A' | 'a' | 'I' => b.is_some_and(|c| matches!(c, '.' | ')' | ' ')),
        _ => false,
    }
}

fn han_digit(c: char) -> bool {
    "一二三四五六七八九十百".contains(c)
}

/// `3.1.2.` → 3
fn segments(head: &str) -> u8 {
    let core = head.trim_end_matches(['.', ')', ':', '、']);
    core.split('.')
        .filter(|s| !s.is_empty())
        .count()
        .clamp(1, 6) as u8
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn styles() {
        assert_eq!(numbering("2.1 DATA"), Some((Style::Decimal(2), "DATA")));
        assert_eq!(
            numbering("3 Model Architecture"),
            Some((Style::Decimal(1), "Model Architecture"))
        );
        assert_eq!(
            numbering("B.3.1. RL Data"),
            Some((Style::Letter(3), "RL Data"))
        );
        assert_eq!(
            numbering("【基本的見解】 1"),
            Some((Style::Lenticular, "基本的見解"))
        );
        assert_eq!(numbering("＜概要＞"), Some((Style::Angle, "概要")));
        assert_eq!(
            numbering("１．わが国の経済・物価の現状"),
            Some((Style::Decimal(1), "わが国の経済・物価の現状"))
        );
        assert_eq!(
            numbering("（１）経済の中心的な見通し"),
            Some((Style::Paren, "経済の中心的な見通し"))
        );
        assert_eq!(numbering("第2章 概要"), Some((Style::Chapter(1), "概要")));
        assert_eq!(
            numbering("第三节 公司简介"),
            Some((Style::Chapter(2), "公司简介"))
        );
        assert_eq!(numbering("[12] A. Author, Title"), None);
        assert_eq!(numbering("The model"), None);
        assert_eq!(numbering("(TS), located on Woodstock beach"), None);
        assert_eq!(
            numbering("(a) First item"),
            Some((Style::Paren, "First item"))
        );
        assert_eq!(numbering("IV. Results"), Some((Style::Roman, "Results")));
        assert_eq!(numbering("一、 释义"), Some((Style::Han, "释义")));
        assert_eq!(numbering("十二、公司治理"), Some((Style::Han, "公司治理")));
        assert_eq!(
            numbering("（二） 偿债能力"),
            Some((Style::HanParen, "偿债能力"))
        );
        assert!(
            starts_at_one("（一） 盈利能力")
                && starts_at_one("1. Intro")
                && starts_at_one("一、释义")
        );
        assert!(
            !starts_at_one("12. Data")
                && !starts_at_one("十一、其他")
                && !starts_at_one("（二） 偿债")
        );
    }
}
