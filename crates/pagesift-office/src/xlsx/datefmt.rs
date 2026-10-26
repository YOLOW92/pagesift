//! 日期时间格式：序列号转年月日（1900/1904 日期系统），按 y m d h s AM/PM 等记号渲染。

use super::numfmt::Tok;

pub(super) fn date(v: f64, toks: &[Tok], date1904: bool) -> String {
    let sub_digits = sub_second_digits(toks);
    // 不显示秒的小数时按秒四舍五入
    let scale = 10f64.powi(sub_digits as i32);
    let total = (v * 86_400.0 * scale).round() / scale;
    let days = (total / 86_400.0).floor();
    let secs = total - days * 86_400.0;
    let (y, mo, d) = civil(days as i64, date1904);
    let (h, mi, s) = (
        (secs / 3600.0) as u32,
        ((secs % 3600.0) / 60.0) as u32,
        (secs % 60.0) as u32,
    );
    let frac = secs.fract();
    let twelve = toks.iter().any(|t| matches!(t, Tok::AmPm(_)));
    let weekday = ((days as i64 + if date1904 { 5 } else { 6 }).rem_euclid(7)) as usize;
    let mut out = String::new();
    for (i, t) in toks.iter().enumerate() {
        match t {
            Tok::Lit(s) => out.push_str(s),
            Tok::Date(p) => {
                let minute = p.starts_with('m') && p.len() <= 2 && is_minute(toks, i);
                match p.as_str() {
                    "yy" | "y" => out.push_str(&format!("{:02}", y.rem_euclid(100))),
                    p if p.starts_with('y') => out.push_str(&y.to_string()),
                    "m" if minute => out.push_str(&mi.to_string()),
                    "mm" if minute => out.push_str(&format!("{mi:02}")),
                    "m" => out.push_str(&mo.to_string()),
                    "mm" => out.push_str(&format!("{mo:02}")),
                    "mmm" => out.push_str(&MONTHS[mo as usize - 1][..3]),
                    "mmmmm" => out.push_str(&MONTHS[mo as usize - 1][..1]),
                    p if p.starts_with('m') => out.push_str(MONTHS[mo as usize - 1]),
                    "d" => out.push_str(&d.to_string()),
                    "dd" => out.push_str(&format!("{d:02}")),
                    "ddd" => out.push_str(&DAYS[weekday][..3]),
                    p if p.starts_with('d') => out.push_str(DAYS[weekday]),
                    p if p.starts_with('h') => {
                        let hh = if twelve { (h + 11) % 12 + 1 } else { h };
                        out.push_str(&if p.len() > 1 {
                            format!("{hh:02}")
                        } else {
                            hh.to_string()
                        });
                    }
                    p if p.starts_with('s') => out.push_str(&if p.len() > 1 {
                        format!("{s:02}")
                    } else {
                        s.to_string()
                    }),
                    _ => {}
                }
            }
            Tok::Elapsed(c) => {
                let n = match c {
                    'h' => (total / 3600.0).floor(),
                    'm' => (total / 60.0).floor(),
                    _ => total.floor(),
                };
                out.push_str(&format!("{n:.0}"));
            }
            Tok::AmPm(p) => {
                let pm = h >= 12;
                out.push_str(
                    match (p.len(), pm, p.starts_with(|c: char| c.is_ascii_lowercase())) {
                        (5, false, _) => "AM",
                        (5, true, _) => "PM",
                        (_, false, true) => "a",
                        (_, true, true) => "p",
                        (_, false, false) => "A",
                        (_, true, false) => "P",
                    },
                );
            }
            Tok::Point if sub_digits > 0 => {
                let f = format!("{:.*}", sub_digits, frac);
                out.push_str(f.trim_start_matches('0'));
            }
            Tok::Digit(_) if sub_digits > 0 => {}
            Tok::Point => out.push('.'),
            Tok::Comma => out.push(','),
            Tok::Slash => out.push('/'),
            _ => {}
        }
    }
    out
}

const MONTHS: [&str; 12] = [
    "January",
    "February",
    "March",
    "April",
    "May",
    "June",
    "July",
    "August",
    "September",
    "October",
    "November",
    "December",
];
const DAYS: [&str; 7] = [
    "Sunday",
    "Monday",
    "Tuesday",
    "Wednesday",
    "Thursday",
    "Friday",
    "Saturday",
];

/// 时间格式里 `.0` 这样的秒小数位数。
fn sub_second_digits(toks: &[Tok]) -> usize {
    toks.iter()
        .position(|t| *t == Tok::Point)
        .filter(|&p| {
            p > 0 && matches!(&toks[p - 1], Tok::Date(s) if s.starts_with('s'))
                || matches!(toks.get(p.wrapping_sub(1)), Some(Tok::Elapsed('s')))
        })
        .map_or(0, |p| {
            toks[p + 1..]
                .iter()
                .take_while(|t| matches!(t, Tok::Digit('0')))
                .count()
        })
}

/// `m`/`mm` 紧跟在小时后面或紧挨在秒前面时是分钟。
fn is_minute(toks: &[Tok], i: usize) -> bool {
    let date_tok = |t: &Tok| matches!(t, Tok::Date(_) | Tok::Elapsed(_));
    let prev = toks[..i].iter().rev().find(|t| date_tok(t));
    let next = toks[i + 1..].iter().find(|t| date_tok(t));
    matches!(prev, Some(Tok::Date(p)) if p.starts_with('h'))
        || matches!(prev, Some(Tok::Elapsed('h')))
        || matches!(next, Some(Tok::Date(p)) if p.starts_with('s'))
        || matches!(next, Some(Tok::Elapsed('s')))
}

/// 序列号的天数 → 年月日。1900 系统里序列号 60 是并不存在的 1900-02-29（照 Excel 保留）。
fn civil(serial_days: i64, date1904: bool) -> (i64, u32, u32) {
    if !date1904 && serial_days == 60 {
        return (1900, 2, 29);
    }
    // 换成从 1970-01-01 起的天数
    let unix = if date1904 {
        serial_days - 24_107
    } else if serial_days < 60 {
        serial_days - 25_568
    } else {
        serial_days - 25_569
    };
    // Howard Hinnant 的 civil_from_days
    let z = unix + 719_468;
    let era = z.div_euclid(146_097);
    let doe = z - era * 146_097;
    let yoe = (doe - doe / 1460 + doe / 36_524 - doe / 146_096) / 365;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let d = (doy - (153 * mp + 2) / 5 + 1) as u32;
    let m = if mp < 10 { mp + 3 } else { mp - 9 } as u32;
    let y = yoe + era * 400 + i64::from(m <= 2);
    (y, m, d)
}

#[cfg(test)]
mod tests {
    fn f(v: f64, code: &str) -> String {
        super::super::numfmt::format(v, code, false)
    }

    #[test]
    fn dates_and_times() {
        assert_eq!(f(37653.0, "m/d/yyyy"), "2/1/2003");
        assert_eq!(
            f(37653.17020843, "dd-mmm-yyyy hh:mm:ss.000"),
            "01-Feb-2003 04:05:06.008"
        );
        assert_eq!(f(37653.0, "dddd-mmmm-yyyy"), "Saturday-February-2003");
        assert_eq!(f(0.5, "h:mm AM/PM"), "12:00 PM");
        assert_eq!(f(1.25, "[h]:mm:ss"), "30:00:00");
        assert_eq!(f(60.0, "yyyy-mm-dd"), "1900-02-29");
        assert_eq!(f(61.0, "yyyy-mm-dd"), "1900-03-01");
        assert_eq!(f(1.0, "yyyy-mm-dd"), "1900-01-01");
        assert_eq!(
            super::super::numfmt::format(0.0, "yyyy-mm-dd", true),
            "1904-01-01"
        );
    }
}
