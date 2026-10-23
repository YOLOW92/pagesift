//! 纯 Rust 字符集统计特征模型与歧义消除打分器。
//! 对非 UTF-8 文本在 CJK、西里尔文、中欧 Latin-2、希腊文、希伯来文、土耳其文及遗留代码页之间进行精确仲裁。

use encoding_rs::{
    BIG5, EUC_KR, Encoding, GB18030, IBM866, ISO_8859_5, KOI8_R, SHIFT_JIS, WINDOWS_1250,
    WINDOWS_1251, WINDOWS_1253, WINDOWS_1254, WINDOWS_1255, WINDOWS_1256,
};

/// 基于纯 Rust 统计分布分析非 UTF-8 字节。
pub fn analyze_statistical(
    bytes: &[u8],
    tld: Option<&[u8]>,
) -> Option<(&'static Encoding, &'static str)> {
    if bytes.is_empty() {
        return None;
    }
    let sample = &bytes[..bytes.len().min(4096)];

    // 1. 先检查是否包含 GB18030 独有的四字节序列
    if has_gb18030_four_bytes(sample) {
        return Some((GB18030, "gb18030"));
    }

    // 2. 检查 Shift-JIS 明确的假名特征
    if has_shift_jis_kana(sample) {
        return Some((SHIFT_JIS, "shift_jis"));
    }

    // 3. 统计高位字节 (>= 0x80) 密度
    let high_bytes_count = sample.iter().filter(|&&b| b >= 0x80).count();
    if high_bytes_count == 0 {
        return None;
    }

    // 4. 判断是多字节 CJK 还是单字节拼音文字（西里尔/欧洲）
    let space_count = sample.iter().filter(|&&b| b == b' ').count();
    let is_spaced_text = space_count * 15 >= sample.len();

    // 5. CJK 双字节常用字打分 (GB18030 vs Big5 vs Shift_JIS vs EUC-KR)
    if let Some((enc, name)) = score_cjk(sample, tld, is_spaced_text) {
        return Some((enc, name));
    }

    // 6. 土耳其文特征识别（ISO-8859-9 vs Windows-1254）
    if let Some((enc, name)) = score_turkish(sample) {
        return Some((enc, name));
    }

    // 7. 西里尔文判决 (Windows-1251 vs KOI8-R vs ISO-8859-5 vs IBM866)
    if is_spaced_text {
        if let Some((enc, name)) = score_cyrillic(sample) {
            return Some((enc, name));
        }
    }

    // 8. 欧洲特殊字符集判决 (Latin-2 vs Greek vs Hebrew vs Arabic)
    if is_spaced_text {
        if let Some((enc, name)) = score_special_alphabets(sample) {
            return Some((enc, name));
        }
    }

    None
}

/// 检查是否存在 GB18030 4 字节编码特征
fn has_gb18030_four_bytes(sample: &[u8]) -> bool {
    if sample.len() < 4 {
        return false;
    }
    for w in sample.windows(4) {
        if (0x81..=0xFE).contains(&w[0])
            && (0x30..=0x39).contains(&w[1])
            && (0x81..=0xFE).contains(&w[2])
            && (0x30..=0x39).contains(&w[3])
        {
            return true;
        }
    }
    false
}

/// 检查 Shift-JIS 平假名与片假名字节对
fn has_shift_jis_kana(sample: &[u8]) -> bool {
    let mut count = 0;
    for w in sample.windows(2) {
        if (w[0] == 0x82 && (0x9F..=0xF1).contains(&w[1]))
            || (w[0] == 0x83 && (0x40..=0x96).contains(&w[1]))
        {
            count += 1;
            if count >= 2 {
                return true;
            }
        }
    }
    false
}

/// 针对中日韩常用字的二元组/特征打分
fn score_cjk(
    sample: &[u8],
    tld: Option<&[u8]>,
    is_spaced: bool,
) -> Option<(&'static Encoding, &'static str)> {
    let mut gbk_score = 0;
    let mut big5_score = 0;
    let mut euckr_score = 0;

    for w in sample.windows(2) {
        // 简体常用: 的, 是, 不, 了, 人, 我, 在, 有, 一, 这
        if matches!(
            (w[0], w[1]),
            (0xB5, 0xC4)
                | (0xCA, 0xC7)
                | (0xB2, 0xBB)
                | (0xC1, 0xCB)
                | (0xC8, 0xCB)
                | (0xCE, 0xD2)
                | (0xD2, 0xBB)
                | (0xD4, 0xDA)
                | (0xD3, 0xD0)
                | (0xD5, 0xE2)
                | (0xB8, 0xF6)
                | (0xC3, 0xC7)
                | (0xD6, 0xD0)
                | (0xCE, 0xC4)
        ) {
            gbk_score += 4;
        }

        // 繁体常用: 的, 是, 不, 了, 我, 在, 有, 人, 這, 他
        if matches!(
            (w[0], w[1]),
            (0xAA, 0xBA)
                | (0xAC, 0x4F)
                | (0xA4, 0xA3)
                | (0xA4, 0x46)
                | (0xA7, 0xDA)
                | (0xA6, 0x62)
                | (0xA6, 0x53)
                | (0xA4, 0x40)
                | (0xB3, 0x6F)
                | (0xA5, 0x4C)
        ) {
            big5_score += 4;
        }

        // 韩文常用: 다, 이, 는, 하, 에
        if matches!(
            (w[0], w[1]),
            (0xB4, 0xD9) | (0xC0, 0xCC) | (0xB4, 0xC2) | (0xC7, 0xCF) | (0xBF, 0xA1)
        ) {
            euckr_score += 4;
        }
    }

    if let Some(t) = tld {
        if t == b"tw" || t == b"hk" {
            big5_score += 6;
        } else if t == b"cn" {
            gbk_score += 6;
        } else if t == b"kr" {
            euckr_score += 6;
        }
    }

    if !is_spaced && (gbk_score > 0 || big5_score > 0) {
        gbk_score += 3;
    }

    let max_score = gbk_score.max(big5_score).max(euckr_score);
    if max_score >= 4 {
        if gbk_score == max_score {
            return Some((GB18030, "gb18030"));
        }
        if big5_score == max_score {
            return Some((BIG5, "big5"));
        }
        if euckr_score == max_score {
            return Some((EUC_KR, "euc-kr"));
        }
    }

    None
}

/// 土耳其文特征识别（ISO-8859-9 vs Windows-1254）
pub fn score_turkish(sample: &[u8]) -> Option<(&'static Encoding, &'static str)> {
    let mut turk_chars = 0;
    let mut c1_ctrl = 0;
    for &b in sample {
        if matches!(b, 0xFD | 0xDD | 0xFE | 0xDE | 0xF0 | 0xD0) {
            turk_chars += 1;
        } else if (0x80..=0x9F).contains(&b) {
            c1_ctrl += 1;
        }
    }
    if turk_chars >= 3 {
        if c1_ctrl == 0 {
            Some((WINDOWS_1254, "iso-8859-9"))
        } else {
            Some((WINDOWS_1254, "windows-1254"))
        }
    } else {
        None
    }
}

/// 针对俄语/西里尔字母的高频单字频次打分
fn score_cyrillic(sample: &[u8]) -> Option<(&'static Encoding, &'static str)> {
    let mut win1251_score = 0;
    let mut koi8_score = 0;
    let mut iso_score = 0;
    let mut ibm866_score = 0;

    for &b in sample {
        if matches!(b, 0xEE | 0xE5 | 0xE0 | 0xE8 | 0xED | 0xF2 | 0xF1 | 0xF0) {
            win1251_score += 1;
        }
        if matches!(b, 0xCF | 0xC5 | 0xC1 | 0xC9 | 0xCE | 0xD4 | 0xD3 | 0xD2) {
            koi8_score += 1;
        }
        if matches!(b, 0xDE | 0xD5 | 0xD0 | 0xD8 | 0xDD | 0xD2 | 0xDB) {
            iso_score += 1;
        }
        if matches!(b, 0xAE | 0xA5 | 0xA0 | 0xA8 | 0xAD | 0xEB | 0xEA | 0xA2) {
            ibm866_score += 1;
        }
    }

    let max_score = win1251_score
        .max(koi8_score)
        .max(iso_score)
        .max(ibm866_score);
    if max_score < 10 {
        return None;
    }

    if win1251_score == max_score && win1251_score > koi8_score + 5 {
        return Some((WINDOWS_1251, "windows-1251"));
    }
    if koi8_score == max_score && koi8_score > win1251_score + 5 {
        return Some((KOI8_R, "koi8-r"));
    }
    if iso_score == max_score && iso_score > win1251_score + 5 {
        return Some((ISO_8859_5, "iso-8859-5"));
    }
    if ibm866_score == max_score && ibm866_score > win1251_score + 5 {
        return Some((IBM866, "cp866"));
    }

    None
}

/// 针对中欧、希腊、希伯来、阿拉伯字母的特征识别
fn score_special_alphabets(sample: &[u8]) -> Option<(&'static Encoding, &'static str)> {
    let mut latin2 = 0;
    let mut greek = 0;
    let mut hebrew = 0;
    let mut arabic = 0;

    for &b in sample {
        if matches!(
            b,
            0xB9 | 0xEA | 0xBF | 0xE6 | 0xF1 | 0xF3 | 0x9C | 0x9F | 0xAF | 0xEC
        ) {
            latin2 += 1;
        }
        if (0xE1..=0xF9).contains(&b) {
            greek += 1;
        }
        if (0xE0..=0xFA).contains(&b) {
            hebrew += 1;
        }
        if (0xC1..=0xEC).contains(&b) {
            arabic += 1;
        }
    }

    if latin2 >= 10 && latin2 > greek.max(hebrew).max(arabic) {
        return Some((WINDOWS_1250, "windows-1250"));
    }
    if greek >= 20 && greek > latin2.max(hebrew).max(arabic) {
        return Some((WINDOWS_1253, "windows-1253"));
    }
    if hebrew >= 20 && hebrew > latin2.max(greek).max(arabic) {
        return Some((WINDOWS_1255, "windows-1255"));
    }
    if arabic >= 20 && arabic > latin2.max(greek).max(hebrew) {
        return Some((WINDOWS_1256, "windows-1256"));
    }

    None
}
