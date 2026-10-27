//! 脚注标记不是公式：作者名后面上标的 `∗`、`†`、`‡`、`§`、`¶`、`‖` 或数字是脚注引用。
//!
//! 这些符号在 LaTeX 里出自数学字体（CMSY 的 `∗` 是 U+2217），单看字形和公式里的运算符一样；
//! 区别在排版：紧跟在普通文字的词后面、缩小并抬高。把它们当公式会得到 `$Vaswani^{\ast}$`，
//! 下游的人名抽取就把作者名当成了公式片段。

use super::tex::Item;
use crate::font::MathKind;
use crate::interp::Glyph;

/// 常见的脚注符号（`∗` 是数学字体里的星号，输出时写成普通的 `*`）。
const MARKERS: &str = "∗*†‡§¶‖";

/// 一个词是不是"普通文字 + 上标脚注标记"，或者单独一个上标脚注符号。
/// `after_text`：前一个词是普通文字（单独的符号只有跟在文字后面才算脚注）。
pub(crate) fn is_marker_word(
    word: &[Item<'_>],
    base_y: f64,
    base_size: f64,
    after_text: bool,
) -> bool {
    let mut letters = 0;
    let mut markers = 0;
    let mut digits_only = true;
    for item in word {
        let t = item.text.trim();
        if t.is_empty() {
            continue;
        }
        if raised(item.g, base_y, base_size) && t.chars().all(is_marker) {
            markers += 1;
            digits_only &= t.chars().all(|c| c.is_ascii_digit());
            continue;
        }
        // 标记后面又来了别的字形，那是公式里的上标，不是脚注
        if markers > 0 || item.g.math != MathKind::Text || !t.chars().all(is_word_char) {
            return false;
        }
        letters += 1;
    }
    if markers == 0 {
        return false;
    }
    // 单独的上标数字（`2` 之类）可能是前一个词的指数，只有符号才算脚注
    letters > 0 || (after_text && !digits_only)
}

/// 脚注词的普通文字：数学字体的 `∗` 写成 `*`，其余照原文。
pub(crate) fn plain(word: &[Item<'_>]) -> String {
    word.iter()
        .flat_map(|i| i.text.trim_start().chars())
        .map(|c| if c == '∗' { '*' } else { c })
        .collect()
}

fn is_marker(c: char) -> bool {
    MARKERS.contains(c) || c.is_ascii_digit()
}

fn is_word_char(c: char) -> bool {
    c.is_alphabetic() || matches!(c, '-' | '\'' | '’' | '.' | ',')
}

/// 和 `tex::level` 判上标的条件相同：比基线字号小，并且抬高。
fn raised(g: &Glyph, base_y: f64, base_size: f64) -> bool {
    g.size <= 0.85 * base_size && g.y - base_y > 0.12 * base_size
}
