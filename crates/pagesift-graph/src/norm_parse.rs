//! Low-level numeric, unit, date, and whitespace parsers for [`crate::norm`].

use crate::norm::DatePrecision;

pub(crate) fn parse_number_with_unit(s: &str) -> Option<(i64, Option<String>)> {
    let t = s.trim();
    if t.is_empty() {
        return None;
    }
    if let Some(num) = t.strip_suffix('%') {
        return parse_scaled_e4(num.trim(), 1).map(|v| (v, Some("%".into())));
    }
    for (suffix, scale, unit) in [
        ("KB", 1, "KB"),
        ("MB", 1, "MB"),
        ("GB", 1, "GB"),
        ("TB", 1, "TB"),
        ("ms", 1, "ms"),
        ("s", 1, "s"),
        ("K", 1_000, ""),
        ("k", 1_000, ""),
        ("M", 1_000_000, ""),
        ("B", 1_000_000_000, ""),
    ] {
        if let Some(rest) = t.strip_suffix(suffix)
            && !rest.is_empty()
            && rest.chars().last().is_some_and(|c| c.is_ascii_digit())
            && let Some(v) = parse_scaled_e4(rest.trim(), scale)
        {
            let u = (!unit.is_empty()).then(|| unit.to_owned());
            return Some((v, u));
        }
    }
    parse_scaled_e4(t, 1).map(|v| (v, None))
}

pub(crate) fn parse_scaled_e4(s: &str, multiplier: i64) -> Option<i64> {
    let cleaned: String = s.trim().chars().filter(|&c| c != ',' && c != ' ').collect();
    if cleaned.is_empty() {
        return None;
    }
    let (neg, digits) = if let Some(r) = cleaned.strip_prefix('-') {
        (true, r)
    } else if let Some(r) = cleaned.strip_prefix('+') {
        (false, r)
    } else {
        (false, cleaned.as_str())
    };
    let (whole_s, frac_s) = match digits.split_once('.') {
        Some((w, f)) => (w, f),
        None => (digits, ""),
    };
    if whole_s.is_empty() && frac_s.is_empty() {
        return None;
    }
    if !whole_s.bytes().all(|b| b.is_ascii_digit()) || !frac_s.bytes().all(|b| b.is_ascii_digit()) {
        return None;
    }
    let whole: i128 = if whole_s.is_empty() {
        0
    } else {
        whole_s.parse().ok()?
    };
    const MAX_FRAC_DIGITS: usize = 4; // [策略] 金额按万分之一(4位小数)定点数归一
    let mut frac_e4: i128 = 0;
    let mut factor: i128 = 1000;
    for b in frac_s.bytes().take(MAX_FRAC_DIGITS) {
        frac_e4 += i128::from(b - b'0') * factor;
        factor /= 10; // [推导] 十进制逐位降阶因子
    }
    let base = whole.checked_mul(10_000)?.checked_add(frac_e4)?; // [推导] 整数部分放大 10^4 对齐 4 位小数
    let scaled = base.checked_mul(i128::from(multiplier))?;
    let signed = if neg { -scaled } else { scaled };
    i64::try_from(signed).ok()
}

const MIN_YEAR: u16 = 1800; // [策略] 合理公历年份下限
const MAX_YEAR: u16 = 2100; // [策略] 合理公历年份上限
const YEAR_DIGITS: usize = 4; // [协议] 四位公历年份
const MAX_MONTH: u8 = 12; // [协议] 公历月份上限
const MAX_DAY: u8 = 31; // [协议] 公历每月最大天数

fn valid_year(y: u16) -> bool {
    (MIN_YEAR..=MAX_YEAR).contains(&y)
}

pub(crate) fn parse_date_str(s: &str, allow_year_only: bool) -> Option<(String, DatePrecision)> {
    let t = s.trim();
    if allow_year_only && t.len() == YEAR_DIGITS && t.bytes().all(|b| b.is_ascii_digit()) {
        let y: u16 = t.parse().ok()?;
        if valid_year(y) {
            return Some((t.to_owned(), DatePrecision::Year));
        }
    }
    if let Some((date_part, _)) = t.split_once('T')
        && let Some(res) = parse_ymd_sep(date_part)
    {
        return Some(res);
    }
    if let Some(res) = parse_ymd_sep(t) {
        return Some(res);
    }
    if let Some(res) = parse_cjk_date(t) {
        return Some(res);
    }
    parse_month_name_date(t)
}

fn parse_ymd_sep(s: &str) -> Option<(String, DatePrecision)> {
    let sep = if s.contains('-') {
        '-'
    } else if s.contains('/') {
        '/'
    } else {
        return None;
    };
    let parts: Vec<&str> = s.split(sep).collect();
    if parts.len() == 2 && parts[0].len() == YEAR_DIGITS && parts[1].len() <= 2 {
        let y: u16 = parts[0].parse().ok()?;
        let m: u8 = parts[1].parse().ok()?;
        if valid_year(y) && (1..=MAX_MONTH).contains(&m) {
            return Some((format!("{y:04}-{m:02}"), DatePrecision::YearMonth));
        }
    }
    const YMD_PARTS: usize = 3; // [协议] YYYY-MM-DD 三段日期格式
    if parts.len() == YMD_PARTS && parts[0].len() == YEAR_DIGITS {
        let y: u16 = parts[0].parse().ok()?;
        let m: u8 = parts[1].parse().ok()?;
        let d: u8 = parts[2].parse().ok()?;
        if valid_year(y) && (1..=MAX_MONTH).contains(&m) && (1..=MAX_DAY).contains(&d) {
            return Some((format!("{y:04}-{m:02}-{d:02}"), DatePrecision::YearMonthDay));
        }
    }
    None
}

fn parse_cjk_date(s: &str) -> Option<(String, DatePrecision)> {
    let (y_str, rest) = s.split_once('年')?;
    let y: u16 = y_str.trim().parse().ok()?;
    let (m_str, rest2) = rest.split_once('月')?;
    let m: u8 = m_str.trim().parse().ok()?;
    let d: u8 = rest2.strip_suffix('日')?.trim().parse().ok()?;
    if valid_year(y) && (1..=MAX_MONTH).contains(&m) && (1..=MAX_DAY).contains(&d) {
        Some((format!("{y:04}-{m:02}-{d:02}"), DatePrecision::YearMonthDay))
    } else {
        None
    }
}

fn parse_month_name_date(s: &str) -> Option<(String, DatePrecision)> {
    const MONTH_NAME_TOKENS: usize = 3; // [协议] 英文月名/日/年三词组合
    let cleaned = s.replace(',', " ");
    let tokens: Vec<&str> = cleaned.split_whitespace().collect();
    if tokens.len() != MONTH_NAME_TOKENS {
        return None;
    }
    if let Some(m) = month_number(tokens[0]) {
        let d: u8 = tokens[1].parse().ok()?;
        let y: u16 = tokens[2].parse().ok()?;
        if valid_year(y) && (1..=MAX_DAY).contains(&d) {
            return Some((format!("{y:04}-{m:02}-{d:02}"), DatePrecision::YearMonthDay));
        }
    } else if let Some(m) = month_number(tokens[1]) {
        let d: u8 = tokens[0].parse().ok()?;
        let y: u16 = tokens[2].parse().ok()?;
        if valid_year(y) && (1..=MAX_DAY).contains(&d) {
            return Some((format!("{y:04}-{m:02}-{d:02}"), DatePrecision::YearMonthDay));
        }
    }
    None
}

fn month_number(s: &str) -> Option<u8> {
    match s.to_ascii_lowercase().as_str() {
        // [协议] 公历 1..=12 月序
        "jan" | "january" => Some(1),
        "feb" | "february" => Some(2),
        "mar" | "march" => Some(3),              // [协议] 三月
        "apr" | "april" => Some(4),              // [协议] 四月
        "may" => Some(5),                        // [协议] 五月
        "jun" | "june" => Some(6),               // [协议] 六月
        "jul" | "july" => Some(7),               // [协议] 七月
        "aug" | "august" => Some(8),             // [协议] 八月
        "sep" | "sept" | "september" => Some(9), // [协议] 九月
        "oct" | "october" => Some(10),           // [协议] 十月
        "nov" | "november" => Some(11),          // [协议] 十一月
        "dec" | "december" => Some(12),          // [协议] 十二月
        _ => None,
    }
}

pub(crate) fn collapse_lower(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    let mut space = false;
    for ch in s.chars() {
        if ch.is_whitespace() {
            if !out.is_empty() {
                space = true;
            }
        } else {
            if space {
                out.push(' ');
                space = false;
            }
            for lc in ch.to_lowercase() {
                out.push(lc);
            }
        }
    }
    out
}
