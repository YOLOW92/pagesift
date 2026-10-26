//! 数字格式（ECMA-376 18.8.30 numFmt、18.8.31 内置格式）：把单元格的数值按格式代码渲染成显示值。
//!
//! 支持：分段（正;负;零;文本）、字面量（"…"、\x、_x、*x）、颜色和条件（忽略）、货币符号 [$€-407]、
//! 数字占位符 0 # ?、千分位与按千缩放、百分号、科学计数、分数、日期时间（含 [h] 经过时间、
//! AM/PM、秒的小数）、1900/1904 日期系统（含 1900 年 2 月 29 日这个历史错误）。
//! 依赖区域设置的内置格式按 en-US 渲染。

/// 内置格式（numFmtId 0–49）。区域相关的（14、22、37–40 等）用 en-US 的写法。
pub(crate) fn builtin(id: u32) -> Option<&'static str> {
    Some(match id {
        0 => "General",
        1 => "0",
        2 => "0.00",
        3 => "#,##0",
        4 => "#,##0.00",
        5 => "$#,##0_);($#,##0)",
        6 => "$#,##0_);[Red]($#,##0)",
        7 => "$#,##0.00_);($#,##0.00)",
        8 => "$#,##0.00_);[Red]($#,##0.00)",
        9 => "0%",
        10 => "0.00%",
        11 => "0.00E+00",
        12 => "# ?/?",
        13 => "# ??/??",
        14 | 27..=31 | 50..=58 => "m/d/yyyy",
        15 => "d-mmm-yy",
        16 => "d-mmm",
        17 => "mmm-yy",
        18 => "h:mm AM/PM",
        19 => "h:mm:ss AM/PM",
        20 | 32 | 33 => "h:mm",
        21 | 34 | 35 => "h:mm:ss",
        22 => "m/d/yyyy h:mm",
        36 => "m/d/yyyy",
        37 => "#,##0_);(#,##0)",
        38 => "#,##0_);[Red](#,##0)",
        39 => "#,##0.00_);(#,##0.00)",
        40 => "#,##0.00_);[Red](#,##0.00)",
        45 => "mm:ss",
        46 => "[h]:mm:ss",
        47 => "mm:ss.0",
        48 => "##0.0E+0",
        49 => "@",
        _ => return None,
    })
}

/// 按格式代码渲染数值。
pub(crate) fn format(v: f64, code: &str, date1904: bool) -> String {
    if !v.is_finite() {
        return general(v);
    }
    let sections = split_sections(code);
    let (section, abs) = match sections.len() {
        0 => return general(v),
        1 => (sections[0], false),
        2 => (if v < 0.0 { sections[1] } else { sections[0] }, v < 0.0),
        _ => {
            if v < 0.0 {
                (sections[1], true)
            } else if v == 0.0 {
                (sections[2], false)
            } else {
                (sections[0], false)
            }
        }
    };
    let toks = tokenize(section);
    if toks.iter().all(|t| matches!(t, Tok::Lit(_) | Tok::General))
        && !toks.iter().any(|t| matches!(t, Tok::General))
        && !toks.is_empty()
    {
        // 纯文字的格式（比如零值段写成 "-"）
        return toks
            .iter()
            .map(|t| if let Tok::Lit(s) = t { s.as_str() } else { "" })
            .collect();
    }
    let value = if abs { v.abs() } else { v };
    if toks.iter().any(|t| matches!(t, Tok::Date(_))) {
        return super::datefmt::date(value, &toks, date1904);
    }
    // 文本格式 @ 用在数值上时，数值按常规格式显示
    let no_digits = !toks.iter().any(|t| matches!(t, Tok::Digit(_)));
    if toks.iter().any(|t| matches!(t, Tok::General))
        || toks.is_empty()
        || (no_digits && toks.contains(&Tok::Text))
    {
        let mut out = String::new();
        for t in &toks {
            match t {
                Tok::General | Tok::Text => out.push_str(&general(value)),
                Tok::Lit(s) => out.push_str(s),
                _ => {}
            }
        }
        return if out.is_empty() { general(value) } else { out };
    }
    number(value, &toks)
}

/// “常规”格式：整数照写；小数最多 15 位有效数字（去掉末尾的 0）；很大或很小时用科学计数。
pub(crate) fn general(v: f64) -> String {
    if v.is_nan() {
        return "#NUM!".into();
    }
    if v == 0.0 {
        return "0".into();
    }
    let a = v.abs();
    if !(1e-9..1e15).contains(&a) {
        let s = format!("{v:.5E}");
        let (m, e) = s.split_once('E').unwrap_or((&s, "0"));
        let m = m.trim_end_matches('0').trim_end_matches('.');
        let e: i32 = e.parse().unwrap_or(0);
        return format!("{m}E{}{:02}", if e < 0 { '-' } else { '+' }, e.abs());
    }
    let digits = 15 - (a.log10().floor() as i32 + 1);
    let s = format!("{:.*}", digits.clamp(0, 20) as usize, v);
    if s.contains('.') {
        s.trim_end_matches('0').trim_end_matches('.').to_string()
    } else {
        s
    }
}

#[derive(Debug, Clone, PartialEq)]
pub(super) enum Tok {
    Lit(String),
    /// 数字占位符 0 # ?
    Digit(char),
    Point,
    Comma,
    Percent,
    /// E+ / E-
    Exp(bool),
    Slash,
    General,
    Text,
    Date(String),
    Elapsed(char),
    AmPm(String),
}

fn split_sections(code: &str) -> Vec<&str> {
    let mut out = Vec::new();
    let (mut start, mut quote, mut bracket, mut escape) = (0, false, false, false);
    for (i, c) in code.char_indices() {
        if escape {
            escape = false;
            continue;
        }
        match c {
            '\\' if !quote => escape = true,
            '"' => quote = !quote,
            '[' if !quote => bracket = true,
            ']' if !quote => bracket = false,
            ';' if !quote && !bracket => {
                out.push(&code[start..i]);
                start = i + 1;
            }
            _ => {}
        }
    }
    out.push(&code[start..]);
    out
}

fn tokenize(s: &str) -> Vec<Tok> {
    let chars: Vec<char> = s.chars().collect();
    let mut out = Vec::new();
    let mut i = 0;
    let lit = |out: &mut Vec<Tok>, s: &str| match out.last_mut() {
        Some(Tok::Lit(prev)) => prev.push_str(s),
        _ => out.push(Tok::Lit(s.to_string())),
    };
    while i < chars.len() {
        let c = chars[i];
        let rest: String = chars[i..].iter().collect::<String>().to_ascii_lowercase();
        match c {
            '"' => {
                let end = chars[i + 1..]
                    .iter()
                    .position(|&x| x == '"')
                    .map_or(chars.len(), |p| i + 1 + p);
                lit(&mut out, &chars[i + 1..end].iter().collect::<String>());
                i = end + 1;
            }
            '\\' => {
                if let Some(&n) = chars.get(i + 1) {
                    lit(&mut out, &n.to_string());
                }
                i += 2;
            }
            '_' => {
                lit(&mut out, " ");
                i += 2;
            }
            '*' => i += 2,
            '[' => {
                let end = chars[i..]
                    .iter()
                    .position(|&x| x == ']')
                    .map_or(chars.len(), |p| i + p);
                let inner: String = chars[i + 1..end].iter().collect();
                let lower = inner.to_ascii_lowercase();
                if let Some(cur) = inner.strip_prefix('$') {
                    lit(&mut out, cur.split('-').next().unwrap_or(""));
                } else if !lower.is_empty() && lower.chars().all(|x| x == 'h') {
                    out.push(Tok::Elapsed('h'));
                } else if !lower.is_empty() && lower.chars().all(|x| x == 'm') {
                    out.push(Tok::Elapsed('m'));
                } else if !lower.is_empty() && lower.chars().all(|x| x == 's') {
                    out.push(Tok::Elapsed('s'));
                }
                // 颜色、条件忽略
                i = end + 1;
            }
            '0' | '#' | '?' => {
                out.push(Tok::Digit(c));
                i += 1;
            }
            '.' => {
                out.push(Tok::Point);
                i += 1;
            }
            ',' => {
                out.push(Tok::Comma);
                i += 1;
            }
            '%' => {
                out.push(Tok::Percent);
                i += 1;
            }
            '/' => {
                out.push(Tok::Slash);
                i += 1;
            }
            '@' => {
                out.push(Tok::Text);
                i += 1;
            }
            'E' | 'e' if matches!(chars.get(i + 1), Some('+' | '-')) => {
                out.push(Tok::Exp(chars[i + 1] == '+'));
                i += 2;
            }
            _ if rest.starts_with("general") => {
                out.push(Tok::General);
                i += 7;
            }
            _ if rest.starts_with("am/pm") => {
                out.push(Tok::AmPm(chars[i..i + 5].iter().collect()));
                i += 5;
            }
            _ if rest.starts_with("a/p") => {
                out.push(Tok::AmPm(chars[i..i + 3].iter().collect()));
                i += 3;
            }
            'y' | 'Y' | 'm' | 'M' | 'd' | 'D' | 'h' | 'H' | 's' | 'S' => {
                let lc = c.to_ascii_lowercase();
                let n = chars[i..]
                    .iter()
                    .take_while(|x| x.to_ascii_lowercase() == lc)
                    .count();
                out.push(Tok::Date(std::iter::repeat_n(lc, n).collect()));
                i += n;
            }
            'e' | 'E' => {
                out.push(Tok::Date("yyyy".into()));
                i += 1;
            }
            _ => {
                lit(&mut out, &c.to_string());
                i += 1;
            }
        }
    }
    out
}

fn number(v: f64, toks: &[Tok]) -> String {
    let mut v = v;
    if toks.contains(&Tok::Percent) {
        v *= 100.0;
    }
    // 数字占位符后面紧跟的逗号（后面没有占位符）每个除以 1000
    let last_digit = toks.iter().rposition(|t| matches!(t, Tok::Digit(_)));
    if let Some(ld) = last_digit {
        let trailing = toks[ld + 1..]
            .iter()
            .take_while(|t| **t == Tok::Comma)
            .count();
        v /= 1000f64.powi(trailing as i32);
    }
    if let Some(e) = toks.iter().position(|t| matches!(t, Tok::Exp(_))) {
        return scientific(v, toks, e);
    }
    if toks.contains(&Tok::Slash) {
        return fraction(v, toks);
    }
    let point = toks.iter().position(|t| *t == Tok::Point);
    let int_toks = &toks[..point.unwrap_or(toks.len())];
    let frac_toks = point.map_or(&toks[toks.len()..], |p| &toks[p + 1..]);
    let decimals = frac_toks
        .iter()
        .filter(|t| matches!(t, Tok::Digit(_)))
        .count();
    let grouping = int_toks
        .windows(2)
        .any(|w| w[0] == Tok::Comma && matches!(w[1], Tok::Digit(_)))
        || int_toks.windows(3).any(|w| {
            matches!(w[0], Tok::Digit(_)) && w[1] == Tok::Comma && matches!(w[2], Tok::Digit(_))
        });
    let rounded = round_decimal(v.abs(), decimals);
    let (int_digits, frac_digits) = rounded.split_once('.').unwrap_or((&rounded, ""));
    let negative = v < 0.0 && rounded.bytes().any(|b| (b'1'..=b'9').contains(&b));
    let int_digits = if int_digits == "0" { "" } else { int_digits };

    // 整数部分：从右往左填，多出来的数字全放进最左边的占位符
    let places: Vec<usize> = int_toks
        .iter()
        .enumerate()
        .filter(|(_, t)| matches!(t, Tok::Digit(_)))
        .map(|(i, _)| i)
        .collect();
    let mut digits: Vec<char> = int_digits.chars().collect();
    let mut filled: Vec<String> = int_toks
        .iter()
        .map(|t| match t {
            Tok::Lit(s) => s.clone(),
            Tok::Percent => "%".into(),
            _ => String::new(),
        })
        .collect();
    for (k, &p) in places.iter().enumerate().rev() {
        let s = if k == 0 {
            let all: String = digits.drain(..).collect();
            if all.is_empty() {
                placeholder(&int_toks[p])
            } else {
                all
            }
        } else {
            digits
                .pop()
                .map(String::from)
                .unwrap_or_else(|| placeholder(&int_toks[p]))
        };
        filled[p] = s;
    }
    let mut int_part: String = filled.concat();
    if grouping {
        int_part = group_thousands(&int_part);
    }
    // 小数部分：从左往右填；# 占位的末尾 0 去掉
    let mut frac = String::new();
    let mut fd = frac_digits.chars();
    let mut pending_zeros = String::new();
    for t in frac_toks {
        match t {
            Tok::Digit(p) => {
                let d = fd.next().unwrap_or('0');
                if *p == '#' && d == '0' {
                    pending_zeros.push('0');
                } else {
                    frac.push_str(&pending_zeros);
                    pending_zeros.clear();
                    frac.push(if *p == '?' && d == '0' { ' ' } else { d });
                }
            }
            Tok::Lit(s) => frac.push_str(s),
            Tok::Percent => frac.push('%'),
            _ => {}
        }
    }
    let mut out = String::new();
    if negative {
        out.push('-');
    }
    out.push_str(&int_part);
    if point.is_some() {
        out.push('.');
        out.push_str(&frac);
    }
    out
}

/// 照 Excel：先取 15 位有效数字的十进制值，再按十进制逢五进位，保留 `decimals` 位小数。
/// （直接按二进制取舍会把 0.0095×100 这样的值舍成 0.9。）
pub(crate) fn round_decimal(v: f64, decimals: usize) -> String {
    let s = format!("{:.14e}", v.abs());
    let (mant, exp) = s.split_once('e').unwrap_or((&s, "0"));
    let exp: i64 = exp.parse().unwrap_or(0);
    let digits: Vec<u8> = mant
        .bytes()
        .filter(u8::is_ascii_digit)
        .map(|b| b - b'0')
        .collect();
    // digits[0] 是 10^exp 位；需要保留到 10^-decimals 位
    let keep = exp + 1 + decimals as i64;
    let mut kept: Vec<u8> = if keep <= 0 {
        Vec::new()
    } else {
        digits.iter().take(keep as usize).copied().collect()
    };
    while (kept.len() as i64) < keep {
        kept.push(0);
    }
    let round_up = keep >= 0 && digits.get(keep as usize).is_some_and(|&d| d >= 5);
    let mut int_len = exp + 1;
    if round_up {
        let mut i = kept.len();
        loop {
            if i == 0 {
                kept.insert(0, 1);
                int_len += 1;
                break;
            }
            i -= 1;
            if kept[i] == 9 {
                kept[i] = 0;
            } else {
                kept[i] += 1;
                break;
            }
        }
    }
    // 拼成定点小数
    let total = (int_len.max(0) as usize) + decimals;
    let mut all: Vec<u8> = Vec::with_capacity(total);
    if int_len < 0 {
        all.extend(std::iter::repeat_n(0, (-int_len) as usize));
    }
    all.extend(kept);
    while all.len() < total.max(decimals) {
        all.insert(0, 0);
    }
    let split = all.len() - decimals;
    let int: String = all[..split].iter().map(|d| char::from(b'0' + d)).collect();
    let int = int.trim_start_matches('0');
    let int = if int.is_empty() { "0" } else { int };
    if decimals == 0 {
        int.to_string()
    } else {
        let frac: String = all[split..].iter().map(|d| char::from(b'0' + d)).collect();
        format!("{int}.{frac}")
    }
}

fn placeholder(t: &Tok) -> String {
    match t {
        Tok::Digit('0') => "0".into(),
        Tok::Digit('?') => " ".into(),
        _ => String::new(),
    }
}

/// 给连续数字串加千分位（只动数字，不动前后的字面量）。
fn group_thousands(s: &str) -> String {
    let start = s.find(|c: char| c.is_ascii_digit()).unwrap_or(s.len());
    let end = s[start..]
        .find(|c: char| !c.is_ascii_digit())
        .map_or(s.len(), |e| start + e);
    let digits = &s[start..end];
    let mut g = String::new();
    for (i, c) in digits.chars().enumerate() {
        if i > 0 && (digits.len() - i).is_multiple_of(3) {
            g.push(',');
        }
        g.push(c);
    }
    format!("{}{g}{}", &s[..start], &s[end..])
}

fn scientific(v: f64, toks: &[Tok], e: usize) -> String {
    let mant_toks = &toks[..e];
    let decimals = mant_toks
        .iter()
        .skip_while(|t| **t != Tok::Point)
        .filter(|t| matches!(t, Tok::Digit(_)))
        .count();
    let exp_digits = toks[e + 1..]
        .iter()
        .filter(|t| matches!(t, Tok::Digit(_)))
        .count()
        .max(1);
    let plus = matches!(toks[e], Tok::Exp(true));
    let int_places = mant_toks
        .iter()
        .take_while(|t| **t != Tok::Point)
        .filter(|t| matches!(t, Tok::Digit(_)))
        .count()
        .max(1);
    // ##0.0E+0 这样整数位多于 1 的写法：指数取整数位个数的倍数（工程计数法）
    let mut exp = if v == 0.0 {
        0
    } else {
        v.abs().log10().floor() as i32
    };
    if int_places > 1 {
        exp = exp.div_euclid(int_places as i32) * int_places as i32;
    }
    let mut mant = v / 10f64.powi(exp);
    let mut m = format!("{:.*}", decimals, mant.abs());
    if m.split('.').next().map_or(0, str::len) > int_places.max(1) && int_places == 1 {
        exp += 1;
        mant = v / 10f64.powi(exp);
        m = format!("{:.*}", decimals, mant.abs());
    }
    let sign = if v < 0.0 { "-" } else { "" };
    let es = if exp < 0 {
        "-"
    } else if plus {
        "+"
    } else {
        ""
    };
    format!("{sign}{m}E{es}{:0width$}", exp.abs(), width = exp_digits)
}

fn fraction(v: f64, toks: &[Tok]) -> String {
    let slash = toks.iter().position(|t| *t == Tok::Slash).unwrap_or(0);
    let den_digits = toks[slash + 1..]
        .iter()
        .filter(|t| matches!(t, Tok::Digit(_)))
        .count()
        .max(1);
    let has_whole = toks[..slash]
        .iter()
        .filter(|t| matches!(t, Tok::Digit(_)))
        .count()
        > 1
        || toks[..slash]
            .iter()
            .any(|t| matches!(t, Tok::Lit(s) if s.contains(' ')));
    let max_den = 10u64.pow(den_digits.min(6) as u32) - 1;
    let a = v.abs();
    let whole = if has_whole { a.trunc() } else { 0.0 };
    let (n, d) = best_fraction(a - whole, max_den);
    let sign = if v < 0.0 { "-" } else { "" };
    let n = if has_whole { n } else { n + (whole as u64) * d };
    match (has_whole, n) {
        (true, 0) => format!("{sign}{whole}"),
        (true, _) if whole > 0.0 => format!("{sign}{whole} {n}/{d}"),
        _ => format!("{sign}{n}/{d}"),
    }
}

/// 分母不超过 `max_den` 的最佳近似分数（连分数）。
fn best_fraction(x: f64, max_den: u64) -> (u64, u64) {
    let (mut h0, mut h1, mut k0, mut k1) = (0u64, 1u64, 1u64, 0u64);
    let mut f = x;
    for _ in 0..32 {
        let a = f.floor() as u64;
        let (h2, k2) = (
            a.saturating_mul(h1).saturating_add(h0),
            a.saturating_mul(k1).saturating_add(k0),
        );
        if k2 > max_den {
            break;
        }
        (h0, h1, k0, k1) = (h1, h2, k1, k2);
        let frac = f - a as f64;
        if frac < 1e-9 {
            break;
        }
        f = 1.0 / frac;
    }
    if k1 == 0 { (0, 1) } else { (h1, k1) }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn f(v: f64, code: &str) -> String {
        format(v, code, false)
    }

    #[test]
    fn numbers_percent_grouping_and_sections() {
        assert_eq!(f(1234.5, "General"), "1234.5");
        assert_eq!(f(0.1 + 0.2, "General"), "0.3");
        assert_eq!(f(15322400.0, "#,##0"), "15,322,400");
        assert_eq!(f(0.279, "0.0%"), "27.9%");
        assert_eq!(f(-1234.567, "#,##0.00"), "-1,234.57");
        assert_eq!(f(-5.0, "#,##0_);(#,##0)"), "(5)");
        assert_eq!(f(0.0, "#,##0;-#,##0;\"-\""), "-");
        assert_eq!(f(3.0, "000"), "003");
        assert_eq!(f(12345678.0, "#,##0,,\"M\""), "12M");
        assert_eq!(f(1.5, "0.##"), "1.5");
        assert_eq!(f(2.0, "0.##"), "2.");
        assert_eq!(f(9.99, "[$£-809]#,##0.00"), "£9.99");
        assert_eq!(f(22.27, "0.00\"°\""), "22.27°");
        assert_eq!(f(12345.0, "0.00E+00"), "1.23E+04");
        assert_eq!(f(0.75, "# ?/?"), "3/4");
        assert_eq!(f(1.5, "# ?/?"), "1 1/2");
        assert_eq!(general(1e20), "1E+20");
        assert_eq!(f(122.0, "@"), "122");
        assert_eq!(f(0.0095, "0.0%"), "1.0%");
        assert_eq!(f(2.675, "0.00"), "2.68");
        for (v, d, want) in [
            (0.0, 2, "0.00"),
            (9.995, 2, "10.00"),
            (0.0004, 3, "0.000"),
            (0.0005, 3, "0.001"),
            (123456.0, 0, "123456"),
            (1e-7, 2, "0.00"),
        ] {
            assert_eq!(round_decimal(v, d), want, "{v} {d}");
        }
    }
}
