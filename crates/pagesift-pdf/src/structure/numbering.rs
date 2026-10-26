//! 标题编号的样式（版面推断用）：写文档的人用编号表达层级，样式和深度就是层级的线索。
//!
//! 十进制编号自带深度（`3`、`3.1`、`3.1.2`）；日文公文、报告常用的 `第N章`、`【…】`、`＜…＞`、
//! `１．`、`（１）`、`①`、`ア．` 各算一个样式，层级按它们在文档里第一次出现的先后排。

/// 编号样式。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub(crate) enum Style {
    /// `3` / `3.1` / `3.1.2`，深度是段数。
    Decimal(u8),
    /// `A.` / `A.1`（附录），深度是段数。
    Letter(u8),
    Roman,
    Chapter,
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
    for (open, close, style) in [('【', '】', Style::Lenticular), ('＜', '＞', Style::Angle), ('〈', '〉', Style::Angle), ('《', '》', Style::Angle)] {
        if first == open
            && let Some(end) = t.find(close)
        {
            let inner = &t[open.len_utf8()..end];
            let rest = t[end + close.len_utf8()..].trim();
            if !inner.is_empty() && rest.chars().all(|c| half(c).is_ascii_digit() || c.is_whitespace()) {
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
    let rest = crate::structure::strip_numbering(t);
    if rest.len() == t.len() || rest.is_empty() {
        return None;
    }
    let head: String = t[..t.len() - rest.len()].chars().map(half).collect();
    let head = head.trim();
    let style = if head.starts_with('第') {
        Style::Chapter
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
    } else if head.split_whitespace().next().is_some_and(|w| ["Chapter", "CHAPTER", "Section", "SECTION", "Part", "PART", "Appendix", "APPENDIX"].contains(&w)) {
        Style::Chapter
    } else {
        return None;
    };
    Some((style, rest))
}

/// `3.1.2.` → 3
fn segments(head: &str) -> u8 {
    let core = head.trim_end_matches(['.', ')', ':', '、']);
    core.split('.').filter(|s| !s.is_empty()).count().clamp(1, 6) as u8
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn styles() {
        assert_eq!(numbering("2.1 DATA"), Some((Style::Decimal(2), "DATA")));
        assert_eq!(numbering("3 Model Architecture"), Some((Style::Decimal(1), "Model Architecture")));
        assert_eq!(numbering("B.3.1. RL Data"), Some((Style::Letter(3), "RL Data")));
        assert_eq!(numbering("【基本的見解】 1"), Some((Style::Lenticular, "基本的見解")));
        assert_eq!(numbering("＜概要＞"), Some((Style::Angle, "概要")));
        assert_eq!(numbering("１．わが国の経済・物価の現状"), Some((Style::Decimal(1), "わが国の経済・物価の現状")));
        assert_eq!(numbering("（１）経済の中心的な見通し"), Some((Style::Paren, "経済の中心的な見通し")));
        assert_eq!(numbering("第2章 概要"), Some((Style::Chapter, "概要")));
        assert_eq!(numbering("[12] A. Author, Title"), None);
        assert_eq!(numbering("The model"), None);
        assert_eq!(numbering("(TS), located on Woodstock beach"), None);
        assert_eq!(numbering("(a) First item"), Some((Style::Paren, "First item")));
        assert_eq!(numbering("IV. Results"), Some((Style::Roman, "Results")));
    }
}
