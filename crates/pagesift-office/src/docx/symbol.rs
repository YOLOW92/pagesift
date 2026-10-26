//! `w:sym` 和 Symbol 字体文字的字符转换。

/// `w:sym`：符号字体的字符码（常写成 F0xx 私用区）转成 Unicode。Symbol 字体按 Adobe Symbol 编码；
/// 其他字体（Wingdings 之类是图形符号）只认普通 Unicode 字符。
pub(super) fn symbol_char(font: &str, code: u32) -> Option<char> {
    let low = if (0xF000..=0xF0FF).contains(&code) {
        code - 0xF000
    } else {
        code
    };
    if font.eq_ignore_ascii_case("symbol") && low < 0x100 {
        const GREEK: &str = "ΑΒΧΔΕΦΓΗΙϑΚΛΜΝΟΠΘΡΣΤΥςΩΞΨΖ";
        const GREEK_LOWER: &str = "αβχδεφγηιϕκλμνοπθρστυϖωξψζ";
        let c = match low {
            0x41..=0x5A => GREEK.chars().nth((low - 0x41) as usize)?,
            0x61..=0x7A => GREEK_LOWER.chars().nth((low - 0x61) as usize)?,
            0x22 => '∀',
            0x24 => '∃',
            0x27 => '∋',
            0x2A => '∗',
            0x2D => '−',
            0x40 => '≅',
            0x5E => '⊥',
            0xA2 => '′',
            0xA3 => '≤',
            0xA5 => '∞',
            0xAB => '↔',
            0xAC => '←',
            0xAD => '↑',
            0xAE => '→',
            0xAF => '↓',
            0xB0 => '°',
            0xB1 => '±',
            0xB2 => '″',
            0xB3 => '≥',
            0xB4 => '×',
            0xB5 => '∝',
            0xB6 => '∂',
            0xB7 => '•',
            0xB8 => '÷',
            0xB9 => '≠',
            0xBA => '≡',
            0xBB => '≈',
            0xBC => '…',
            0xC4 => '⊗',
            0xC5 => '⊕',
            0xC6 => '∅',
            0xC7 => '∩',
            0xC8 => '∪',
            0xC9 => '⊃',
            0xCA => '⊇',
            0xCC => '⊂',
            0xCD => '⊆',
            0xCE => '∈',
            0xCF => '∉',
            0xD0 => '∠',
            0xD1 => '∇',
            0xD5 => '∏',
            0xD6 => '√',
            0xD7 => '⋅',
            0xD8 => '¬',
            0xD9 => '∧',
            0xDA => '∨',
            0xDB => '⇔',
            0xDC => '⇐',
            0xDE => '⇒',
            0xE5 => '∑',
            0xF2 => '∫',
            0x20..=0x7E => char::from_u32(low)?,
            _ => return None,
        };
        return Some(c);
    }
    char::from_u32(code).filter(|c| !('\u{E000}'..='\u{F8FF}').contains(c))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn symbol_font_characters() {
        assert_eq!(symbol_char("Symbol", 0xF0B4), Some('×'));
        assert_eq!(symbol_char("Symbol", 0xF061), Some('α'));
        assert_eq!(symbol_char("Wingdings", 0xF0FC), None);
        assert_eq!(symbol_char("Arial", 0x2192), Some('→'));
    }
}
