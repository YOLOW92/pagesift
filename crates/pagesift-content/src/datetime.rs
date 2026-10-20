//! 日期时间：常见的 ISO 8601（含 RFC 3339）和 RFC 2822 写法统一换算成 UTC 的 RFC 3339。
//! 没写时区的按 UTC 处理；只有日期的取当天 00:00:00。

pub(crate) fn to_utc_rfc3339(s: &str) -> Option<String> {
    let s = s.trim();
    let secs = parse_iso(s).or_else(|| parse_rfc2822(s))?;
    let (days, rem) = (secs.div_euclid(86_400), secs.rem_euclid(86_400));
    let (y, m, d) = civil_from_days(days);
    if !(1000..=9999).contains(&y) {
        return None;
    }
    Some(format!(
        "{y:04}-{m:02}-{d:02}T{:02}:{:02}:{:02}Z",
        rem / 3600,
        rem / 60 % 60,
        rem % 60
    ))
}

struct Cursor<'a> {
    b: &'a [u8],
    i: usize,
}

impl Cursor<'_> {
    fn peek(&self) -> Option<u8> {
        self.b.get(self.i).copied()
    }

    fn eat(&mut self, c: u8) -> bool {
        if self.peek() == Some(c) {
            self.i += 1;
            true
        } else {
            false
        }
    }

    /// 读 min..=max 位十进制数字。
    fn digits(&mut self, min: usize, max: usize) -> Option<i64> {
        let start = self.i;
        while self.i - start < max && self.peek().is_some_and(|c| c.is_ascii_digit()) {
            self.i += 1;
        }
        if self.i - start < min {
            return None;
        }
        std::str::from_utf8(&self.b[start..self.i])
            .ok()?
            .parse()
            .ok()
    }

    fn skip_spaces(&mut self) {
        while self.peek() == Some(b' ') {
            self.i += 1;
        }
    }

    fn done(&self) -> bool {
        self.i == self.b.len()
    }
}

fn parse_iso(s: &str) -> Option<i64> {
    let mut c = Cursor {
        b: s.as_bytes(),
        i: 0,
    };
    let y = c.digits(4, 4)?;
    let sep = c.peek()?;
    if sep != b'-' && sep != b'/' {
        return None;
    }
    c.i += 1;
    let mo = c.digits(1, 2)?;
    if !c.eat(sep) {
        return None;
    }
    let d = c.digits(1, 2)?;
    let date = days_checked(y, mo, d)?;
    if c.done() {
        return Some(date * 86_400);
    }
    if !(c.eat(b'T') || c.eat(b't') || c.eat(b' ')) {
        return None;
    }
    let h = c.digits(2, 2)?;
    if !c.eat(b':') {
        return None;
    }
    let mi = c.digits(2, 2)?;
    let mut sec = 0;
    if c.eat(b':') {
        sec = c.digits(2, 2)?;
        if c.eat(b'.') || c.eat(b',') {
            c.digits(1, 9)?;
        }
    }
    if h > 23 || mi > 59 || sec > 60 {
        return None;
    }
    c.skip_spaces();
    let offset = zone(&mut c)?;
    c.skip_spaces();
    c.done()
        .then(|| date * 86_400 + h * 3600 + mi * 60 + sec.min(59) - offset)
}

/// 时区：`Z`、`UTC`、`GMT`、`+08:00`、`+0800`、`+08`，或者没有（按 UTC）。返回相对 UTC 的秒数。
fn zone(c: &mut Cursor<'_>) -> Option<i64> {
    let rest = &c.b[c.i..];
    for name in [&b"UTC"[..], b"GMT", b"UT", b"Z", b"z"] {
        if rest.starts_with(name) {
            c.i += name.len();
            return Some(0);
        }
    }
    let sign = match c.peek() {
        Some(b'+') => 1,
        Some(b'-') => -1,
        None => return Some(0),
        _ => return None,
    };
    c.i += 1;
    let h = c.digits(2, 2)?;
    c.eat(b':');
    let m = c.digits(2, 2).unwrap_or(0);
    (h <= 14 && m <= 59).then_some(sign * (h * 3600 + m * 60))
}

/// `Tue, 01 Oct 2024 08:00:00 GMT` 这类写法（HTTP 头和 RSS 常用）。
fn parse_rfc2822(s: &str) -> Option<i64> {
    let mut parts = s.split([' ', ',']).filter(|p| !p.is_empty()).peekable();
    if parts.peek()?.starts_with(|c: char| c.is_ascii_alphabetic()) {
        parts.next();
    }
    let d: i64 = parts.next()?.parse().ok()?;
    let mon = parts.next()?.get(..3)?.to_ascii_lowercase();
    let mo = [
        "jan", "feb", "mar", "apr", "may", "jun", "jul", "aug", "sep", "oct", "nov", "dec",
    ]
    .iter()
    .position(|m| *m == mon)? as i64
        + 1;
    let y: i64 = parts.next()?.parse().ok()?;
    let date = days_checked(y, mo, d)?;
    let Some(time) = parts.next() else {
        return Some(date * 86_400);
    };
    let mut t = time.split(':');
    let h: i64 = t.next()?.parse().ok()?;
    let mi: i64 = t.next()?.parse().ok()?;
    let sec: i64 = t.next().map_or(Some(0), |x| x.parse().ok())?;
    if h > 23 || mi > 59 || sec > 60 {
        return None;
    }
    let offset = match parts.next() {
        None => 0,
        Some(z) => match z.to_ascii_uppercase().as_str() {
            "GMT" | "UT" | "UTC" | "Z" => 0,
            "EST" => -5 * 3600,
            "EDT" => -4 * 3600,
            "CST" => -6 * 3600,
            "CDT" => -5 * 3600,
            "MST" => -7 * 3600,
            "MDT" => -6 * 3600,
            "PST" => -8 * 3600,
            "PDT" => -7 * 3600,
            _ => {
                let mut c = Cursor {
                    b: z.as_bytes(),
                    i: 0,
                };
                let o = zone(&mut c)?;
                if !c.done() {
                    return None;
                }
                o
            }
        },
    };
    Some(date * 86_400 + h * 3600 + mi * 60 + sec.min(59) - offset)
}

fn days_checked(y: i64, m: i64, d: i64) -> Option<i64> {
    let leap = (y % 4 == 0 && y % 100 != 0) || y % 400 == 0;
    let dim = match m {
        1 | 3 | 5 | 7 | 8 | 10 | 12 => 31,
        4 | 6 | 9 | 11 => 30,
        2 if leap => 29,
        2 => 28,
        _ => return None,
    };
    (1..=dim).contains(&d).then(|| days_from_civil(y, m, d))
}

/// 公历日期到 1970-01-01 起的天数（Howard Hinnant 的算法）。
fn days_from_civil(y: i64, m: i64, d: i64) -> i64 {
    let y = if m <= 2 { y - 1 } else { y };
    let era = y.div_euclid(400);
    let yoe = y - era * 400;
    let mp = (m + 9) % 12;
    let doy = (153 * mp + 2) / 5 + d - 1;
    let doe = yoe * 365 + yoe / 4 - yoe / 100 + doy;
    era * 146_097 + doe - 719_468
}

fn civil_from_days(z: i64) -> (i64, i64, i64) {
    let z = z + 719_468;
    let era = z.div_euclid(146_097);
    let doe = z - era * 146_097;
    let yoe = (doe - doe / 1460 + doe / 36_524 - doe / 146_096) / 365;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let d = doy - (153 * mp + 2) / 5 + 1;
    let m = if mp < 10 { mp + 3 } else { mp - 9 };
    (yoe + era * 400 + i64::from(m <= 2), m, d)
}

#[cfg(test)]
mod tests {
    use super::to_utc_rfc3339 as f;

    #[test]
    fn converts_to_utc() {
        assert_eq!(f("2026-10-01T08:00:00Z").unwrap(), "2026-10-01T08:00:00Z");
        assert_eq!(
            f("2026-10-01T08:00:00+08:00").unwrap(),
            "2026-10-01T00:00:00Z"
        );
        assert_eq!(
            f("2026-01-01T01:30:00.123+0200").unwrap(),
            "2025-12-31T23:30:00Z"
        );
        assert_eq!(f("2024-02-29").unwrap(), "2024-02-29T00:00:00Z");
        assert_eq!(f("2024-06-24 10:00").unwrap(), "2024-06-24T10:00:00Z");
        assert_eq!(f("2024/6/4").unwrap(), "2024-06-04T00:00:00Z");
        assert_eq!(
            f("2024-12-31T22:00:00-05:00").unwrap(),
            "2025-01-01T03:00:00Z"
        );
        assert_eq!(
            f("Tue, 01 Oct 2024 08:00:00 GMT").unwrap(),
            "2024-10-01T08:00:00Z"
        );
        assert_eq!(
            f("1 Oct 2024 08:00:00 +0100").unwrap(),
            "2024-10-01T07:00:00Z"
        );
        assert_eq!(
            f("Tue, 01 Oct 2024 08:00:00 CST").unwrap(),
            "2024-10-01T14:00:00Z"
        );
        assert_eq!(
            f("Tue, 01 Oct 2024 08:00:00 EDT").unwrap(),
            "2024-10-01T12:00:00Z"
        );
    }

    #[test]
    fn rejects_garbage() {
        for s in [
            "",
            "yesterday",
            "2023-02-29",
            "2024-13-01",
            "2024-01-01T25:00",
            "2024-01-01Tfoo",
            "12345",
            "2024-01-01T10:00:00+99:00",
        ] {
            assert_eq!(f(s), None, "{s}");
        }
    }
}
