//! 简单字体的编码：标准编码表、Differences 用到的字形名，以及标准 14 字体的字宽。

use std::collections::HashMap;
use std::sync::OnceLock;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum BaseEncoding {
    Standard,
    WinAnsi,
    MacRoman,
}

/// 基础编码下单字节码对应的字符。
pub(crate) fn base_char(enc: BaseEncoding, code: u8) -> Option<char> {
    match enc {
        BaseEncoding::WinAnsi => match code {
            0x20..=0x7e => Some(code as char),
            0xa0 => Some(' '),
            0xad => Some('-'),
            0x80..=0xff => single_byte(encoding_rs::WINDOWS_1252, code),
            _ => None,
        },
        BaseEncoding::MacRoman => match code {
            0x20..=0x7e => Some(code as char),
            0x80..=0xff => single_byte(encoding_rs::MACINTOSH, code),
            _ => None,
        },
        BaseEncoding::Standard => match code {
            0x27 => Some('\u{2019}'),
            0x60 => Some('\u{2018}'),
            0x20..=0x7e => Some(code as char),
            0xa1..=0xff => {
                let c = STANDARD_HIGH[usize::from(code - 0xa1)];
                (c != '\0').then_some(c)
            }
            _ => None,
        },
    }
}

fn single_byte(enc: &'static encoding_rs::Encoding, code: u8) -> Option<char> {
    let buf = [code];
    let (s, bad) = enc.decode_without_bom_handling(&buf);
    if bad {
        return None;
    }
    s.chars().next().filter(|c| !c.is_control())
}

/// StandardEncoding 的 0xA1..=0xFF，'\0' 表示未定义。
const STANDARD_HIGH: [char; 95] = [
    '¡', '¢', '£', '⁄', '¥', 'ƒ', '§', '¤', '\'', '“', '«', '‹', '›', 'ﬁ', 'ﬂ', // A1-AF
    '\0', '–', '†', '‡', '·', '\0', '¶', '•', '‚', '„', '”', '»', '…', '‰', '\0',
    '¿', // B0-BF
    '\0', '`', '´', 'ˆ', '˜', '¯', '˘', '˙', '¨', '\0', '˚', '¸', '\0', '˝', '˛',
    'ˇ', // C0-CF
    '—', '\0', '\0', '\0', '\0', '\0', '\0', '\0', '\0', '\0', '\0', '\0', '\0', '\0', '\0',
    '\0', // D0-DF
    '\0', 'Æ', '\0', 'ª', '\0', '\0', '\0', '\0', 'Ł', 'Ø', 'Œ', 'º', '\0', '\0', '\0',
    '\0', // E0-EF
    '\0', 'æ', '\0', '\0', '\0', 'ı', '\0', '\0', 'ł', 'ø', 'œ', 'ß', '\0', '\0', '\0',
    '\0', // F0-FF
];

/// ASCII 0x20..=0x7E 的字形名（按码位顺序）。
const ASCII_NAMES: [&str; 95] = [
    "space",
    "exclam",
    "quotedbl",
    "numbersign",
    "dollar",
    "percent",
    "ampersand",
    "quotesingle",
    "parenleft",
    "parenright",
    "asterisk",
    "plus",
    "comma",
    "hyphen",
    "period",
    "slash",
    "zero",
    "one",
    "two",
    "three",
    "four",
    "five",
    "six",
    "seven",
    "eight",
    "nine",
    "colon",
    "semicolon",
    "less",
    "equal",
    "greater",
    "question",
    "at",
    "A",
    "B",
    "C",
    "D",
    "E",
    "F",
    "G",
    "H",
    "I",
    "J",
    "K",
    "L",
    "M",
    "N",
    "O",
    "P",
    "Q",
    "R",
    "S",
    "T",
    "U",
    "V",
    "W",
    "X",
    "Y",
    "Z",
    "bracketleft",
    "backslash",
    "bracketright",
    "asciicircum",
    "underscore",
    "grave",
    "a",
    "b",
    "c",
    "d",
    "e",
    "f",
    "g",
    "h",
    "i",
    "j",
    "k",
    "l",
    "m",
    "n",
    "o",
    "p",
    "q",
    "r",
    "s",
    "t",
    "u",
    "v",
    "w",
    "x",
    "y",
    "z",
    "braceleft",
    "bar",
    "braceright",
    "asciitilde",
];

/// Latin-1 0xA0..=0xFF 的字形名。
const LATIN1_NAMES: [&str; 96] = [
    "nbspace",
    "exclamdown",
    "cent",
    "sterling",
    "currency",
    "yen",
    "brokenbar",
    "section",
    "dieresis",
    "copyright",
    "ordfeminine",
    "guillemotleft",
    "logicalnot",
    "sfthyphen",
    "registered",
    "macron",
    "degree",
    "plusminus",
    "twosuperior",
    "threesuperior",
    "acute",
    "mu",
    "paragraph",
    "periodcentered",
    "cedilla",
    "onesuperior",
    "ordmasculine",
    "guillemotright",
    "onequarter",
    "onehalf",
    "threequarters",
    "questiondown",
    "Agrave",
    "Aacute",
    "Acircumflex",
    "Atilde",
    "Adieresis",
    "Aring",
    "AE",
    "Ccedilla",
    "Egrave",
    "Eacute",
    "Ecircumflex",
    "Edieresis",
    "Igrave",
    "Iacute",
    "Icircumflex",
    "Idieresis",
    "Eth",
    "Ntilde",
    "Ograve",
    "Oacute",
    "Ocircumflex",
    "Otilde",
    "Odieresis",
    "multiply",
    "Oslash",
    "Ugrave",
    "Uacute",
    "Ucircumflex",
    "Udieresis",
    "Yacute",
    "Thorn",
    "germandbls",
    "agrave",
    "aacute",
    "acircumflex",
    "atilde",
    "adieresis",
    "aring",
    "ae",
    "ccedilla",
    "egrave",
    "eacute",
    "ecircumflex",
    "edieresis",
    "igrave",
    "iacute",
    "icircumflex",
    "idieresis",
    "eth",
    "ntilde",
    "ograve",
    "oacute",
    "ocircumflex",
    "otilde",
    "odieresis",
    "divide",
    "oslash",
    "ugrave",
    "uacute",
    "ucircumflex",
    "udieresis",
    "yacute",
    "thorn",
    "ydieresis",
];

const GREEK_LOWER: [&str; 25] = [
    "alpha", "beta", "gamma", "delta", "epsilon", "zeta", "eta", "theta", "iota", "kappa",
    "lambda", "mu", "nu", "xi", "omicron", "pi", "rho", "sigma1", "sigma", "tau", "upsilon", "phi",
    "chi", "psi", "omega",
];

const GREEK_UPPER: [&str; 25] = [
    "Alpha", "Beta", "Gamma", "Delta", "Epsilon", "Zeta", "Eta", "Theta", "Iota", "Kappa",
    "Lambda", "Mu", "Nu", "Xi", "Omicron", "Pi", "Rho", "", "Sigma", "Tau", "Upsilon", "Phi",
    "Chi", "Psi", "Omega",
];

const OTHER_NAMES: &[(&str, &str)] = &[
    ("quoteleft", "\u{2018}"),
    ("quoteright", "\u{2019}"),
    ("quotedblleft", "\u{201C}"),
    ("quotedblright", "\u{201D}"),
    ("quotesinglbase", "\u{201A}"),
    ("quotedblbase", "\u{201E}"),
    ("guilsinglleft", "\u{2039}"),
    ("guilsinglright", "\u{203A}"),
    ("endash", "\u{2013}"),
    ("emdash", "\u{2014}"),
    ("bullet", "\u{2022}"),
    ("ellipsis", "\u{2026}"),
    ("dagger", "\u{2020}"),
    ("daggerdbl", "\u{2021}"),
    ("perthousand", "\u{2030}"),
    ("trademark", "\u{2122}"),
    ("fraction", "\u{2044}"),
    ("florin", "\u{0192}"),
    ("circumflex", "\u{02C6}"),
    ("tilde", "\u{02DC}"),
    ("breve", "\u{02D8}"),
    ("dotaccent", "\u{02D9}"),
    ("ring", "\u{02DA}"),
    ("ogonek", "\u{02DB}"),
    ("caron", "\u{02C7}"),
    ("hungarumlaut", "\u{02DD}"),
    ("dotlessi", "\u{0131}"),
    ("Lslash", "\u{0141}"),
    ("lslash", "\u{0142}"),
    ("OE", "\u{0152}"),
    ("oe", "\u{0153}"),
    ("Scaron", "\u{0160}"),
    ("scaron", "\u{0161}"),
    ("Zcaron", "\u{017D}"),
    ("zcaron", "\u{017E}"),
    ("Ydieresis", "\u{0178}"),
    ("Euro", "\u{20AC}"),
    ("minus", "\u{2212}"),
    ("fi", "fi"),
    ("fl", "fl"),
    ("ff", "ff"),
    ("ffi", "ffi"),
    ("ffl", "ffl"),
    ("nonbreakingspace", " "),
    ("softhyphen", "\u{00AD}"),
    ("hyphensoft", "\u{00AD}"),
    ("infinity", "\u{221E}"),
    ("lessequal", "\u{2264}"),
    ("greaterequal", "\u{2265}"),
    ("notequal", "\u{2260}"),
    ("approxequal", "\u{2248}"),
    ("summation", "\u{2211}"),
    ("product", "\u{220F}"),
    ("radical", "\u{221A}"),
    ("integral", "\u{222B}"),
    ("partialdiff", "\u{2202}"),
    ("lozenge", "\u{25CA}"),
    ("arrowleft", "\u{2190}"),
    ("arrowup", "\u{2191}"),
    ("arrowright", "\u{2192}"),
    ("arrowdown", "\u{2193}"),
    ("degree", "\u{00B0}"),
    ("periodcentered", "\u{00B7}"),
    ("middot", "\u{00B7}"),
];

fn glyph_table() -> &'static HashMap<&'static str, String> {
    static TABLE: OnceLock<HashMap<&'static str, String>> = OnceLock::new();
    TABLE.get_or_init(|| {
        let mut m = HashMap::new();
        for (i, n) in ASCII_NAMES.iter().enumerate() {
            m.insert(*n, char::from(0x20 + i as u8).to_string());
        }
        for (i, n) in LATIN1_NAMES.iter().enumerate() {
            m.insert(*n, char::from(0xa0 + i as u8).to_string());
        }
        for (i, n) in GREEK_LOWER.iter().enumerate() {
            if let Some(c) = char::from_u32(0x3b1 + i as u32) {
                m.insert(*n, c.to_string());
            }
        }
        for (i, n) in GREEK_UPPER.iter().enumerate() {
            if let Some(c) = char::from_u32(0x391 + i as u32)
                && !n.is_empty()
            {
                m.insert(*n, c.to_string());
            }
        }
        // mu 在 AGL 里指微符号
        m.insert("mu", "\u{00B5}".to_string());
        for (n, s) in OTHER_NAMES {
            m.insert(*n, (*s).to_string());
        }
        m
    })
}

/// 字形名转 Unicode：查表，再按 AGL 规则处理 uniXXXX、uXXXX、连字和后缀。
pub(crate) fn glyph_to_unicode(name: &[u8]) -> Option<String> {
    let name = std::str::from_utf8(name).ok()?;
    let base = name.split('.').next().unwrap_or(name);
    if base.is_empty() {
        return None;
    }
    if base.contains('_') {
        let parts: Option<String> = base.split('_').map(glyph_component).collect();
        return parts;
    }
    glyph_component(base)
}

fn glyph_component(name: &str) -> Option<String> {
    if let Some(s) = glyph_table().get(name) {
        return Some(s.clone());
    }
    if let Some(hex) = name.strip_prefix("uni")
        && hex.len() >= 4
        && hex.len() % 4 == 0
        && hex.bytes().all(|b| b.is_ascii_hexdigit())
    {
        let units: Vec<u16> = (0..hex.len() / 4)
            .filter_map(|i| u16::from_str_radix(&hex[i * 4..i * 4 + 4], 16).ok())
            .collect();
        let s: String = char::decode_utf16(units)
            .map(|r| r.unwrap_or(char::REPLACEMENT_CHARACTER))
            .collect();
        return Some(s);
    }
    if let Some(hex) = name.strip_prefix('u')
        && (4..=6).contains(&hex.len())
        && hex.bytes().all(|b| b.is_ascii_hexdigit())
    {
        let v = u32::from_str_radix(hex, 16).ok()?;
        return char::from_u32(v).map(|c| c.to_string());
    }
    None
}

/// 有些字体的 ToUnicode 把汉字映射到康熙部首或部首补充区，归一到统一汉字。
pub(crate) fn unify_radical(c: char) -> char {
    let v = c as u32;
    let mapped = match v {
        0x2F00..=0x2FD5 => Some(u32::from(KANGXI[(v - 0x2F00) as usize])),
        0x2E80..=0x2EF3 => RADICAL_SUPPLEMENT
            .binary_search_by_key(&v, |&(r, _)| u32::from(r))
            .ok()
            .map(|i| u32::from(RADICAL_SUPPLEMENT[i].1)),
        _ => None,
    };
    mapped.and_then(char::from_u32).unwrap_or(c)
}

/// U+2F00..=U+2FD5 的 NFKC 结果。
const KANGXI: [u16; 214] = [
    0x4E00, 0x4E28, 0x4E36, 0x4E3F, 0x4E59, 0x4E85, 0x4E8C, 0x4EA0, 0x4EBA, 0x513F, 0x5165, 0x516B,
    0x5182, 0x5196, 0x51AB, 0x51E0, 0x51F5, 0x5200, 0x529B, 0x52F9, 0x5315, 0x531A, 0x5338, 0x5341,
    0x535C, 0x5369, 0x5382, 0x53B6, 0x53C8, 0x53E3, 0x56D7, 0x571F, 0x58EB, 0x5902, 0x590A, 0x5915,
    0x5927, 0x5973, 0x5B50, 0x5B80, 0x5BF8, 0x5C0F, 0x5C22, 0x5C38, 0x5C6E, 0x5C71, 0x5DDB, 0x5DE5,
    0x5DF1, 0x5DFE, 0x5E72, 0x5E7A, 0x5E7F, 0x5EF4, 0x5EFE, 0x5F0B, 0x5F13, 0x5F50, 0x5F61, 0x5F73,
    0x5FC3, 0x6208, 0x6236, 0x624B, 0x652F, 0x6534, 0x6587, 0x6597, 0x65A4, 0x65B9, 0x65E0, 0x65E5,
    0x66F0, 0x6708, 0x6728, 0x6B20, 0x6B62, 0x6B79, 0x6BB3, 0x6BCB, 0x6BD4, 0x6BDB, 0x6C0F, 0x6C14,
    0x6C34, 0x706B, 0x722A, 0x7236, 0x723B, 0x723F, 0x7247, 0x7259, 0x725B, 0x72AC, 0x7384, 0x7389,
    0x74DC, 0x74E6, 0x7518, 0x751F, 0x7528, 0x7530, 0x758B, 0x7592, 0x7676, 0x767D, 0x76AE, 0x76BF,
    0x76EE, 0x77DB, 0x77E2, 0x77F3, 0x793A, 0x79B8, 0x79BE, 0x7A74, 0x7ACB, 0x7AF9, 0x7C73, 0x7CF8,
    0x7F36, 0x7F51, 0x7F8A, 0x7FBD, 0x8001, 0x800C, 0x8012, 0x8033, 0x807F, 0x8089, 0x81E3, 0x81EA,
    0x81F3, 0x81FC, 0x820C, 0x821B, 0x821F, 0x826E, 0x8272, 0x8278, 0x864D, 0x866B, 0x8840, 0x884C,
    0x8863, 0x897E, 0x898B, 0x89D2, 0x8A00, 0x8C37, 0x8C46, 0x8C55, 0x8C78, 0x8C9D, 0x8D64, 0x8D70,
    0x8DB3, 0x8EAB, 0x8ECA, 0x8F9B, 0x8FB0, 0x8FB5, 0x9091, 0x9149, 0x91C6, 0x91CC, 0x91D1, 0x9577,
    0x9580, 0x961C, 0x96B6, 0x96B9, 0x96E8, 0x9751, 0x975E, 0x9762, 0x9769, 0x97CB, 0x97ED, 0x97F3,
    0x9801, 0x98A8, 0x98DB, 0x98DF, 0x9996, 0x9999, 0x99AC, 0x9AA8, 0x9AD8, 0x9ADF, 0x9B25, 0x9B2F,
    0x9B32, 0x9B3C, 0x9B5A, 0x9CE5, 0x9E75, 0x9E7F, 0x9EA5, 0x9EBB, 0x9EC3, 0x9ECD, 0x9ED1, 0x9EF9,
    0x9EFD, 0x9F0E, 0x9F13, 0x9F20, 0x9F3B, 0x9F4A, 0x9F52, 0x9F8D, 0x9F9C, 0x9FA0,
];

/// 部首补充区里有对应统一汉字的部分（Unicode EquivalentUnifiedIdeograph）。
const RADICAL_SUPPLEMENT: &[(u16, u16)] = &[
    (0x2E81, 0x5382),
    (0x2E82, 0x4E5B),
    (0x2E83, 0x4E5A),
    (0x2E84, 0x4E59),
    (0x2E85, 0x4EBB),
    (0x2E86, 0x5182),
    (0x2E89, 0x5202),
    (0x2E8A, 0x535C),
    (0x2E8C, 0x5C0F),
    (0x2E8E, 0x5140),
    (0x2E8F, 0x5C23),
    (0x2E90, 0x5C22),
    (0x2E92, 0x5DF3),
    (0x2E93, 0x5E7A),
    (0x2E94, 0x5F51),
    (0x2E96, 0x5FC4),
    (0x2E97, 0x5FC3),
    (0x2E98, 0x624C),
    (0x2E99, 0x6535),
    (0x2E9B, 0x65E1),
    (0x2E9C, 0x65E5),
    (0x2E9D, 0x6708),
    (0x2E9E, 0x6B7A),
    (0x2E9F, 0x6BCD),
    (0x2EA0, 0x6C11),
    (0x2EA1, 0x6C35),
    (0x2EA2, 0x6C3A),
    (0x2EA3, 0x706C),
    (0x2EA4, 0x722B),
    (0x2EA6, 0x4E2C),
    (0x2EA7, 0x725B),
    (0x2EA8, 0x72AD),
    (0x2EA9, 0x738B),
    (0x2EAA, 0x758B),
    (0x2EAB, 0x76EE),
    (0x2EAC, 0x793A),
    (0x2EAD, 0x793B),
    (0x2EAF, 0x7CF9),
    (0x2EB0, 0x7E9F),
    (0x2EB1, 0x7F53),
    (0x2EB2, 0x7F52),
    (0x2EB6, 0x7F8A),
    (0x2EB7, 0x7F8A),
    (0x2EB8, 0x7F8B),
    (0x2EB9, 0x8002),
    (0x2EBA, 0x8080),
    (0x2EBB, 0x807F),
    (0x2EBC, 0x8089),
    (0x2EBD, 0x81FC),
    (0x2EBE, 0x8279),
    (0x2EBF, 0x8279),
    (0x2EC0, 0x8279),
    (0x2EC1, 0x864E),
    (0x2EC2, 0x8864),
    (0x2EC3, 0x8980),
    (0x2EC4, 0x897F),
    (0x2EC5, 0x89C1),
    (0x2EC6, 0x89D2),
    (0x2EC8, 0x8BA0),
    (0x2EC9, 0x8D1D),
    (0x2ECB, 0x8F66),
    (0x2ECC, 0x8FB6),
    (0x2ECD, 0x8FB6),
    (0x2ECE, 0x8FB6),
    (0x2ECF, 0x961D),
    (0x2ED0, 0x9485),
    (0x2ED1, 0x9577),
    (0x2ED2, 0x9578),
    (0x2ED3, 0x957F),
    (0x2ED4, 0x95E8),
    (0x2ED6, 0x961D),
    (0x2ED7, 0x96E8),
    (0x2ED8, 0x9752),
    (0x2ED9, 0x97E6),
    (0x2EDA, 0x9875),
    (0x2EDB, 0x98CE),
    (0x2EDC, 0x98DE),
    (0x2EDD, 0x98DF),
    (0x2EDF, 0x98E0),
    (0x2EE0, 0x9963),
    (0x2EE2, 0x9A6C),
    (0x2EE3, 0x9AA8),
    (0x2EE4, 0x9B3C),
    (0x2EE5, 0x9C7C),
    (0x2EE6, 0x9E1F),
    (0x2EE7, 0x5364),
    (0x2EE8, 0x9EA6),
    (0x2EE9, 0x9EC4),
    (0x2EEA, 0x9EFE),
    (0x2EEB, 0x6589),
    (0x2EEC, 0x9F50),
    (0x2EED, 0x6B6F),
    (0x2EEE, 0x9F7F),
    (0x2EEF, 0x7ADC),
    (0x2EF0, 0x9F99),
    (0x2EF1, 0x9F9C),
    (0x2EF2, 0x4E80),
    (0x2EF3, 0x9F9F),
];

#[derive(Debug, Clone, Copy)]
pub(crate) enum Standard14 {
    Helvetica,
    Times,
    Courier,
}

/// 标准 14 字体的字宽（千分之一 em），只覆盖 ASCII，其余按平均宽度估算。
pub(crate) fn standard_width(font: Standard14, ch: char) -> f64 {
    let code = ch as u32;
    match font {
        Standard14::Courier => 600.0,
        Standard14::Helvetica => match code {
            0x20..=0x7e => f64::from(HELVETICA[(code - 0x20) as usize]),
            _ => 556.0,
        },
        Standard14::Times => match code {
            0x20..=0x7e => f64::from(TIMES[(code - 0x20) as usize]),
            _ => 500.0,
        },
    }
}

pub(crate) fn standard14(base_font: &str) -> Option<Standard14> {
    let n = base_font.to_ascii_lowercase();
    if n.contains("courier") {
        Some(Standard14::Courier)
    } else if n.contains("helvetica") || n.contains("arial") {
        Some(Standard14::Helvetica)
    } else if n.contains("times") {
        Some(Standard14::Times)
    } else {
        None
    }
}

const HELVETICA: [u16; 95] = [
    278, 278, 355, 556, 556, 889, 667, 191, 333, 333, 389, 584, 278, 333, 278, 278, 556, 556, 556,
    556, 556, 556, 556, 556, 556, 556, 278, 278, 584, 584, 584, 556, 1015, 667, 667, 722, 722, 667,
    611, 778, 722, 278, 500, 667, 556, 833, 722, 778, 667, 778, 722, 667, 611, 722, 667, 944, 667,
    667, 611, 278, 278, 278, 469, 556, 333, 556, 556, 500, 556, 556, 278, 556, 556, 222, 222, 500,
    222, 833, 556, 556, 556, 556, 333, 500, 278, 556, 500, 722, 500, 500, 500, 334, 260, 334, 584,
];

const TIMES: [u16; 95] = [
    250, 333, 408, 500, 500, 833, 778, 180, 333, 333, 500, 564, 250, 333, 250, 278, 500, 500, 500,
    500, 500, 500, 500, 500, 500, 500, 278, 278, 564, 564, 564, 444, 921, 722, 667, 667, 722, 611,
    556, 722, 722, 333, 389, 722, 611, 889, 722, 722, 556, 722, 667, 556, 611, 722, 722, 944, 722,
    722, 611, 333, 278, 333, 469, 500, 333, 444, 500, 444, 500, 444, 333, 500, 500, 278, 278, 500,
    278, 778, 500, 500, 500, 500, 333, 389, 278, 500, 500, 722, 500, 500, 444, 480, 200, 480, 541,
];

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn glyph_names() {
        assert_eq!(glyph_to_unicode(b"A").as_deref(), Some("A"));
        assert_eq!(glyph_to_unicode(b"eacute").as_deref(), Some("é"));
        assert_eq!(glyph_to_unicode(b"uni4E2D").as_deref(), Some("中"));
        assert_eq!(glyph_to_unicode(b"u1F600").as_deref(), Some("😀"));
        assert_eq!(glyph_to_unicode(b"f_f_i").as_deref(), Some("ffi"));
        assert_eq!(glyph_to_unicode(b"a.sc").as_deref(), Some("a"));
        assert_eq!(glyph_to_unicode(b"g123"), None);
        assert_eq!(unify_radical('\u{2F08}'), '人');
        assert_eq!(unify_radical('\u{2ECB}'), '车');
        assert_eq!(unify_radical('a'), 'a');
    }

    #[test]
    fn base_encodings() {
        assert_eq!(base_char(BaseEncoding::WinAnsi, 0x93), Some('“'));
        assert_eq!(base_char(BaseEncoding::MacRoman, 0x8e), Some('é'));
        assert_eq!(base_char(BaseEncoding::Standard, 0x27), Some('’'));
        assert_eq!(base_char(BaseEncoding::Standard, 0xae), Some('ﬁ'));
    }
}
