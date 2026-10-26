use std::collections::HashMap;
use std::sync::LazyLock;

use aho_corasick::AhoCorasick;
use regex::Regex;

use super::{Bounds, Date, dmy, full_year};

// ---------------------------------------------------------------------------
// 单个日期表达式的解析

pub(super) const YEAR: &str = "(?:199[0-9]|20[0-3][0-9])";

pub(super) static YMD_NO_SEP: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(r"\b(\d{8})\b").unwrap());
pub(super) static YMD: LazyLock<Regex> = LazyLock::new(|| {
    Regex::new(&format!(
        r"(?:\D|^)(?:({YEAR})[\-/.]([0-1]?[0-9])[\-/.]([0-3]?[0-9])|([0-3]?[0-9])[\-/.]([0-3]?[0-9])[\-/.](\d{{2,4}}))(?:\D|$)"
    ))
    .unwrap()
});
pub(super) static YM: LazyLock<Regex> = LazyLock::new(|| {
    Regex::new(&format!(
        r"(?:\D|^)(?:({YEAR})[\-/.]([0-1]?[0-9])|([0-1]?[0-9])[\-/.]({YEAR}))(?:\D|$)"
    ))
    .unwrap()
});
/// 分隔符两边有空格或用竖线、圆点：`26 | 01 | 2022`、`2022 · 01 · 26`（自研，htmldate 要交给 dateparser）。
pub(super) static SPACED: LazyLock<Regex> = LazyLock::new(|| {
    Regex::new(&format!(
        r"(?:\D|^)(?:({YEAR})\s*[|·•/.-]\s*([0-1]?[0-9])\s*[|·•/.-]\s*([0-3]?[0-9])|([0-3]?[0-9])\s*[|·•/.-]\s*([0-3]?[0-9])\s*[|·•/.-]\s*({YEAR}))(?:\D|$)"
    ))
    .unwrap()
});
/// 月份名在前：`June 7, 2020`、`Juni 7 2020`；粘在前一个词上的月份名（`SmithMarch`）按最长后缀找。
pub(super) static LONG_MDY: LazyLock<Regex> = LazyLock::new(|| {
    Regex::new(&format!(
        r"(?i)(\p{{L}}{{3,}})\.?\s+([0-3]?[0-9])(?:st|nd|rd|th)?,?\s+({YEAR})"
    ))
    .unwrap()
});
/// 日在前：`7 June 2020`、`7. Juni 2020`、`7th of June, 2020`、`7 de junio de 2020`、`7 июня 2020`。
pub(super) static LONG_DMY: LazyLock<Regex> = LazyLock::new(|| {
    Regex::new(&format!(
        r"(?i)([0-3]?[0-9])(?:st|nd|rd|th|\.)?\s+(?:of\s+|de\s+)?(\p{{L}}{{3,}})[,.]?\s+(?:de\s+|del\s+)?({YEAR})"
    ))
    .unwrap()
});
/// 年在前：`2020. június 7.`（匈牙利语）、`2020 June 7`。
pub(super) static LONG_YMD: LazyLock<Regex> = LazyLock::new(|| {
    Regex::new(&format!(
        r"(?i)({YEAR})\.?\s+(\p{{L}}{{3,}})\.?\s+([0-3]?[0-9])(?:\D|$)"
    ))
    .unwrap()
});
/// 中日韩：`2020年6月7日`、`2020년 6월 7일`。
pub(super) static CJK: LazyLock<Regex> = LazyLock::new(|| {
    Regex::new(&format!(
        r"({YEAR})\s*[年년]\s*([0-1]?[0-9])\s*[月월]\s*([0-3]?[0-9])\s*[日일]?"
    ))
    .unwrap()
});

/// 各语言的月份名（小写，土耳其语的 ı 折成 i）→ 月份。
pub(super) static MONTHS: LazyLock<HashMap<String, u32>> = LazyLock::new(|| {
    const TABLE: [&str; 12] = [
        "jan january januar jänner januari janvier janv ocak oca enero ene janeiro gennaio gen \
         января январь январе січня січень stycznia styczeń styczen ledna leden ianuarie tammikuuta \
         tammikuu január ιανουαρίου ιανουάριος يناير ژانویه",
        "feb february februar feber februari février fevrier févr fevr şubat subat şub sub febrero \
         fevereiro fev febbraio февраля февраль лютого лютий lutego luty února únor februarie \
         helmikuuta helmikuu február φεβρουαρίου φεβρουάριος فبراير شباط فوریه",
        "mar march mär märz maerz maret mart mars marzo março marco maart mrt марта март березня \
         березень marca marzec března březen martie maaliskuuta maaliskuu március μαρτίου μάρτιος مارس آذار",
        "apr april avril avr nisan nis abril abr aprile апреля апрель квітня квітень kwietnia \
         kwiecień kwiecien dubna duben aprilie huhtikuuta huhtikuu április απριλίου απρίλιος أبريل إبريل ابريل نيسان آوریل",
        "may mai mei mayıs mayis mayo maio maggio mag мая май травня травень maja maj května květen \
         toukokuuta toukokuu május μαΐου μαΐου μάιος مايو أيار مه",
        "jun june juni juin haziran haz junio junho giugno giu июня июнь червня червень czerwca \
         czerwiec června červen iunie kesäkuuta kesäkuu június ιουνίου ιούνιος يونيو يونيه حزيران ژوئن",
        "jul july juli juillet juil temmuz tem julio julho luglio lug июля июль липня липень lipca \
         lipiec července červenec iulie heinäkuuta heinäkuu július ιουλίου ιούλιος يوليو يوليه تموز ژوئیه",
        "aug august agustus ağustos agustos ağu agu aout août agosto ago augustus augusti августа \
         август серпня серпень sierpnia sierpień sierpien srpna srpen elokuuta elokuu augusztus \
         αυγούστου αύγουστος أغسطس اغسطس آب اوت",
        "sep sept september septembre eylül eylul eyl septiembre setiembre setembro set settembre \
         sentyabrya сентября сентябрь вересня вересень września wrzesień wrzesien září zari \
         septembrie syyskuuta syyskuu szeptember σεπτεμβρίου σεπτέμβριος سبتمبر أيلول سپتامبر",
        "oct october oktober octobre okt ekim eki octubre outubro out ottobre ott октября октябрь \
         жовтня жовтень października październik pazdziernik října říjen octombrie lokakuuta \
         lokakuu október οκτωβρίου οκτώβριος أكتوبر اكتوبر اکتبر",
        "nov november novembre kasım kasim kas noviembre novembro ноября ноябрь листопада \
         листопад listopada listopad listopadu noiembrie marraskuuta marraskuu νοεμβρίου νοέμβριος نوفمبر نوامبر",
        "dec december dez dezember décembre decembre déc desember aralık aralik ara diciembre dic \
         dezembro dicembre декабря декабрь грудня грудень grudnia grudzień grudzien prosince \
         prosinec decembrie joulukuuta joulukuu december δεκεμβρίου δεκέμβριος ديسمبر دسامبر",
    ];
    let mut map = HashMap::new();
    for (i, names) in TABLE.iter().enumerate() {
        for n in names.split_whitespace() {
            map.entry(fold(n)).or_insert(i as u32 + 1);
        }
    }
    map
});

pub(super) fn fold(s: &str) -> String {
    s.replace(['ı', 'İ'], "i").to_lowercase()
}

/// 月份名：先整词查；月份在前时允许粘在前一个词后面（取最长的已知后缀，同 htmldate）。
///
/// 粘连只认相邻节点文字拼在一起的样子：后缀以大写字母开头、前一个字符不是大写（`SmithMarch`），
/// 或者是不分大小写的文字（阿拉伯文等）。否则 `about`、`sunset` 的结尾会被当成葡语的
/// `out`（outubro）、`set`（setembro）。
pub(super) fn month_of(word: &str, glued: bool) -> Option<u32> {
    let w = fold(word);
    if let Some(&m) = MONTHS.get(&w) {
        return Some(m);
    }
    if !glued {
        return None;
    }
    let chars: Vec<char> = word.chars().collect();
    let start = chars.len().saturating_sub(12).max(1);
    (start..chars.len().saturating_sub(2)).find_map(|i| {
        let (prev, first) = (chars[i - 1], chars[i]);
        let boundary = if first.is_uppercase() {
            !prev.is_uppercase()
        } else {
            !first.is_lowercase()
        };
        if !boundary {
            return None;
        }
        MONTHS
            .get(&fold(&chars[i..].iter().collect::<String>()))
            .copied()
    })
}

/// 自然语言写法（月份名、"年月日"），返回第一个合法的（同 htmldate 的 regex_parse，覆盖更多语言）。
pub(super) fn parse_long(s: &str) -> Option<Date> {
    let mut best: Option<(usize, Date)> = None;
    let mut consider = |pos: usize, d: Option<Date>| {
        if let Some(d) = d
            && best.is_none_or(|(p, _)| pos < p)
        {
            best = Some((pos, d));
        }
    };
    if let Some(c) = LONG_MDY.captures_iter(s).find_map(|c| {
        let m = month_of(&c[1], true)?;
        dmy(c[2].parse().ok()?, m, c[3].parse().ok()?).map(|d| (c.get(0).unwrap().start(), d))
    }) {
        consider(c.0, Some(c.1));
    }
    if let Some(c) = LONG_DMY.captures_iter(s).find_map(|c| {
        let m = month_of(&c[2], false)?;
        dmy(c[1].parse().ok()?, m, c[3].parse().ok()?).map(|d| (c.get(0).unwrap().start(), d))
    }) {
        consider(c.0, Some(c.1));
    }
    if let Some(c) = LONG_YMD.captures_iter(s).find_map(|c| {
        let m = month_of(&c[2], false)?;
        Date::ymd(c[1].parse().ok()?, m, c[3].parse().ok()?).map(|d| (c.get(0).unwrap().start(), d))
    }) {
        consider(c.0, Some(c.1));
    }
    if let Some(c) = CJK.captures_iter(s).find_map(|c| {
        Date::ymd(c[1].parse().ok()?, c[2].parse().ok()?, c[3].parse().ok()?)
            .map(|d| (c.get(0).unwrap().start(), d))
    }) {
        consider(c.0, Some(c.1));
    }
    best.map(|(_, d)| d)
}

pub(super) struct Cursor<'a> {
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
    fn digits(&mut self, min: usize, max: usize) -> Option<i64> {
        let start = self.i;
        while self.i - start < max && self.peek().is_some_and(|c| c.is_ascii_digit()) {
            self.i += 1;
        }
        if self.i - start < min {
            self.i = start;
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
}

/// 时区：`Z`、`UTC`、`GMT`、`+08:00`、`+0800`、`+08`。没有时返回 `Some(None)`，写错了返回 `None`。
pub(super) fn zone(c: &mut Cursor<'_>) -> Option<Option<i32>> {
    let rest = &c.b[c.i..];
    for name in [&b"UTC"[..], b"GMT", b"UT", b"Z", b"z"] {
        if rest.starts_with(name) {
            c.i += name.len();
            return Some(Some(0));
        }
    }
    let sign = match c.peek() {
        Some(b'+') => 1,
        Some(b'-') => -1,
        _ => return Some(None),
    };
    c.i += 1;
    let h = c.digits(2, 2)?;
    c.eat(b':');
    let m = c.digits(2, 2).unwrap_or(0);
    (h <= 14 && m <= 59).then_some(Some(sign * (h * 3600 + m * 60) as i32))
}

/// ISO 8601 一类的写法：`2020-06-07`、`2020/6/7`、`2020.06.07`，后面可以跟 `T08:30[:00[.123]]` 和时区。
/// `strict` 时整个字符串必须都是日期；否则日期后面可以有别的内容（只取日期）。
pub(super) fn parse_iso(s: &str, strict: bool) -> Option<Date> {
    let mut c = Cursor {
        b: s.as_bytes(),
        i: 0,
    };
    let y = c.digits(4, 4)? as i32;
    let sep = c.peek()?;
    if !matches!(sep, b'-' | b'/' | b'.') {
        return None;
    }
    c.i += 1;
    let mo = c.digits(1, 2)? as u32;
    if !c.eat(sep) {
        return None;
    }
    let d = c.digits(1, 2)? as u32;
    let mut date = Date::ymd(y, mo, d)?;
    if c.peek().is_some_and(|b| b.is_ascii_digit()) {
        return None;
    }
    let save = c.i;
    let time = (|| {
        if !(c.eat(b'T') || c.eat(b't') || c.eat(b' ')) {
            return None;
        }
        let h = c.digits(2, 2)? as u32;
        if !c.eat(b':') {
            return None;
        }
        let mi = c.digits(2, 2)? as u32;
        let mut sec = 0;
        if c.eat(b':') {
            sec = c.digits(2, 2)? as u32;
            if c.eat(b'.') || c.eat(b',') {
                c.digits(1, 9)?;
            }
        }
        if h > 23 || mi > 59 || sec > 60 {
            return None;
        }
        c.skip_spaces();
        let off = zone(&mut c)?;
        Some((h, mi, sec.min(59), off))
    })();
    match time {
        Some(t) => date.time = Some(t),
        None => c.i = save,
    }
    if strict {
        c.skip_spaces();
        if c.i != c.b.len() {
            return None;
        }
    }
    Some(date)
}

/// `Tue, 01 Oct 2024 08:00:00 GMT`（HTTP 头、RSS）。
pub(super) fn parse_rfc2822(s: &str) -> Option<Date> {
    let mut parts = s.split([' ', ',']).filter(|p| !p.is_empty()).peekable();
    if parts.peek()?.starts_with(|c: char| c.is_ascii_alphabetic()) {
        parts.next();
    }
    let d: u32 = parts.next()?.parse().ok()?;
    let m = month_of(parts.next()?, false)?;
    let y: i32 = parts.next()?.parse().ok()?;
    let mut date = Date::ymd(full_year(y), m, d)?;
    if let Some(t) = parts.next() {
        let mut hms = t.split(':').map(|x| x.parse::<u32>().ok());
        let (h, mi) = (hms.next()??, hms.next()??);
        let sec = hms.next().flatten().unwrap_or(0);
        let off = match parts.next() {
            Some(z) => match obsolete_zone(z) {
                Some(off) => off,
                None => {
                    let mut c = Cursor {
                        b: z.as_bytes(),
                        i: 0,
                    };
                    zone(&mut c)?
                }
            },
            None => None,
        };
        if h > 23 || mi > 59 || sec > 60 {
            return None;
        }
        date.time = Some((h, mi, sec.min(59), off));
    }
    Some(date)
}

/// RFC 2822 §4.3 的旧式时区名：北美的 EST/EDT/CST/CDT/MST/MDT/PST/PDT 有固定偏移；
/// 单个字母的军用时区（Z 以外）按规范当作未知（`Some(None)`）。不是这些名字时返回 `None`。
fn obsolete_zone(z: &str) -> Option<Option<i32>> {
    let hours = match z {
        "EDT" => -4,
        "EST" | "CDT" => -5,
        "CST" | "MDT" => -6,
        "MST" | "PDT" => -7,
        "PST" => -8,
        _ if z.len() == 1 && z != "Z" && z != "z" && z.as_bytes()[0].is_ascii_alphabetic() => {
            return Some(None);
        }
        _ => return None,
    };
    Some(Some(hours * 3600))
}

/// 结构化的日期值（meta、JSON-LD、`<time datetime>`）规范化为 RFC 3339，保留原来的时区偏移。
pub(crate) fn normalize(s: &str) -> Option<String> {
    let s = s.trim();
    parse_iso(s, true)
        .or_else(|| parse_rfc2822(s))
        .filter(|d| (1000..=9999).contains(&d.y))
        .map(|d| d.render())
}

/// htmldate 的 custom_parse：按可靠程度试各种写法，返回第一个在合理范围内的。
pub(super) fn custom_parse(s: &str, b: &Bounds) -> Option<Date> {
    let bytes = s.as_bytes();
    if bytes.len() >= 4 && bytes[..4].iter().all(u8::is_ascii_digit) {
        let first = if bytes.len() >= 8 && bytes[4..8].iter().all(u8::is_ascii_digit) {
            Date::ymd(
                s[..4].parse().ok()?,
                s[4..6].parse().ok()?,
                s[6..8].parse().ok()?,
            )
        } else {
            parse_iso(s, false)
        };
        if let Some(d) = b.check(first) {
            return Some(d);
        }
    }
    if let Some(d) = b.check(parse_rfc2822(s)) {
        return Some(d);
    }
    if let Some(c) = YMD_NO_SEP.captures(s)
        && let Some(d) = b.check(Date::ymd(
            c[1][..4].parse().ok()?,
            c[1][4..6].parse().ok()?,
            c[1][6..8].parse().ok()?,
        ))
    {
        return Some(d);
    }
    // 找到过完整的年月日（哪怕不合法或超出范围）的位置，YM 不再从里面截出“当月 1 日”。
    let mut full_dates: Vec<std::ops::Range<usize>> = Vec::new();
    if let Some(c) = YMD.captures(s) {
        full_dates.push(c.get(0).unwrap().range());
        let d = if let Some(y) = c.get(1) {
            Date::ymd(
                y.as_str().parse().ok()?,
                c[2].parse().ok()?,
                c[3].parse().ok()?,
            )
        } else {
            dmy(c[4].parse().ok()?, c[5].parse().ok()?, c[6].parse().ok()?)
        };
        if let Some(d) = b.check(d) {
            return Some(d);
        }
    }
    if let Some(c) = SPACED.captures(s) {
        full_dates.push(c.get(0).unwrap().range());
        let d = if let Some(y) = c.get(1) {
            Date::ymd(
                y.as_str().parse().ok()?,
                c[2].parse().ok()?,
                c[3].parse().ok()?,
            )
        } else {
            dmy(c[4].parse().ok()?, c[5].parse().ok()?, c[6].parse().ok()?)
        };
        if let Some(d) = b.check(d) {
            return Some(d);
        }
    }
    if let Some(d) = b.check(parse_long(s)) {
        return Some(d);
    }
    if let Some(c) = YM.captures_iter(s).find(|c| {
        let r = c.get(0).unwrap().range();
        !full_dates
            .iter()
            .any(|f| f.start < r.end && r.start < f.end)
    }) {
        let (y, m) = match c.get(1) {
            Some(y) => (y.as_str(), &c[2]),
            None => (&c[4], &c[3]),
        };
        if let Some(d) = b.check(Date::ymd(y.parse().ok()?, m.parse().ok()?, 1)) {
            return Some(d);
        }
    }
    None
}

pub(super) static DISCARD: LazyLock<Regex> = LazyLock::new(|| {
    Regex::new(
        r"^\d{2}:\d{2}(?: |:|$)|^\D*\d{4}\D*$|[$€¥Ұ£¢₽₱฿#₹]|[A-Z]{3}[^A-Z]|(?:^|\D)(?:\+\d{2}|\d{3}|\d{5})\D|ftps?|https?|sftp|\.(?:com|net|org|info|gov|edu|de|fr|io)\b|IBAN|[A-Z]{2}[0-9]{2}|®",
    )
    .unwrap()
});

pub(super) const MIN_SEGMENT: usize = 6;
pub(super) const MAX_SEGMENT: usize = 52;

pub(super) fn trim_text(s: &str) -> String {
    s.split_whitespace().collect::<Vec<_>>().join(" ")
}

pub(super) fn cut(s: &str, max_chars: usize) -> &str {
    match s.char_indices().nth(max_chars) {
        Some((i, _)) => &s[..i],
        None => s,
    }
}

/// htmldate 的 try_date_expr：一小段文本里的日期。数字太少或太多、像时刻、价格、电话、网址的都不要。
pub(super) fn date_expr(s: &str, b: &Bounds) -> Option<Date> {
    let t = trim_text(s);
    let t = cut(&t, MAX_SEGMENT);
    // 整段就是一个 ISO 8601 时间戳时不用 DISCARD 的启发式（毫秒 `.123Z` 会命中“三位数字”那条）。
    if let Some(d) = parse_iso(t, true) {
        return b.check(Some(d));
    }
    let digits = t.chars().filter(char::is_ascii_digit).count();
    if !(4..=18).contains(&digits) || DISCARD.is_match(t) {
        return None;
    }
    custom_parse(t, b)
}

/// htmldate 的 examine_text：元素文本，去掉结尾的非数字部分再解析。
pub(super) fn examine_text(s: &str, b: &Bounds) -> Option<Date> {
    let t = trim_text(s);
    if t.chars().count() <= MIN_SEGMENT {
        return None;
    }
    let t = cut(&t, MAX_SEGMENT);
    let t = t.trim_end_matches(|c: char| !c.is_ascii_digit());
    date_expr(t, b)
}

pub(super) static TIMESTAMP: LazyLock<Regex> = LazyLock::new(|| {
    Regex::new(&format!(
        r"({YEAR}-[0-1]?[0-9]-[0-3]?[0-9]|{YEAR}/[0-1]?[0-9]/[0-3]?[0-9])[T ][0-9]{{2}}:[0-9]{{2}}:[0-9]{{2}}(?:\.[0-9]+)?(?:Z|[+-][0-9]{{2}}:?[0-9]{{2}})?"
    ))
    .unwrap()
});

pub(super) fn first_timestamp(text: &str, b: &Bounds) -> Option<Date> {
    let c = TIMESTAMP.captures(text)?;
    b.check(parse_iso(&c[0], false)).map(|mut d| {
        d.time = None;
        d
    })
}

pub(super) static TEXT_PATTERNS: LazyLock<Regex> = LazyLock::new(|| {
    Regex::new(concat!(
        r#"(?i)(?:date[^0-9"]{0,20}|updated|last-modified|published|posted|on)[ :]{0,9}?([0-9]{1,4})[./]([0-9]{1,2})[./]([0-9]{2,4})|"#,
        r"(?:Datum|Stand|Veröffentlicht am):? ?([0-9]{1,2})\.([0-9]{1,2})\.([0-9]{2,4})|",
        r"(?:güncellen?me|yayı(?:m|n)lan?ma) {0,9}?(?:tarihi)? {0,9}?:? {0,9}?([0-9]{1,2})[./]([0-9]{1,2})[./]([0-9]{2,4})|",
        r"([0-9]{1,2})[./]([0-9]{1,2})[./]([0-9]{2,4}) {0,9}?(?:'de|'da|'te|'ta|’de|’da|’te|’ta|tarihinde) {0,9}(?:güncellendi|yayı(?:m|n)landı)",
    ))
    .unwrap()
});

/// 各种说法里必有的关键词（不含 "on"，它单独判断）。
pub(super) static AC_PHRASES: LazyLock<AhoCorasick> = LazyLock::new(|| {
    AhoCorasick::builder()
        .ascii_case_insensitive(true)
        .build([
            "date",
            "updated",
            "last-modified",
            "published",
            "posted",
            "datum",
            "stand",
            "veröffentlicht",
            "VERÖFFENTLICHT",
            "Veröffentlicht",
            "güncel",
            "GÜNCEL",
            "Güncel",
            "yayı",
            "YAYI",
            "Yayı",
            "'de",
            "'da",
            "'te",
            "'ta",
            "’de",
            "’da",
            "’te",
            "’ta",
            "tarihinde",
        ])
        .unwrap()
});

pub(super) static DATE_CORE: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(r"[0-9]{1,4}[./][0-9]{1,2}[./][0-9]{2,4}").unwrap());

/// htmldate 的 idiosyncrasies_search："Posted on 07/06/2020"、"Veröffentlicht am 7.6.2020" 这类说法。
pub(super) fn idiosyncrasies(text: &str, b: &Bounds) -> Option<Date> {
    // 所有说法都含一个数字日期：先找这些位置，只在前后 60 字节的窗口里试完整的模式；
    // 窗口里有匹配时从窗口起点往后找（同 htmldate 的预筛；htmldate 超过 1000 处就退回整页扫描，
    // 这里有关键词预筛，窗口检查很便宜，不设上限）
    let floor = |mut i: usize| {
        while !text.is_char_boundary(i) {
            i -= 1;
        }
        i
    };
    let mut from = None;
    for core in DATE_CORE.find_iter(text) {
        let start = floor(core.start().saturating_sub(60));
        let end = floor((core.end() + 60).min(text.len()));
        // 窗口里没有任何一种说法的关键词，就不用跑完整的模式（"on" 太常见，只看紧挨在日期前面的）
        let before = text[..core.start()].trim_end_matches([' ', ':']).as_bytes();
        let on = core.start() - before.len() <= 9
            && before.len() >= 2
            && before[before.len() - 2..].eq_ignore_ascii_case(b"on");
        if !on && !AC_PHRASES.is_match(&text[start..end]) {
            continue;
        }
        if TEXT_PATTERNS.is_match(&text[start..end]) {
            from = Some(start);
            break;
        }
    }
    let c = TEXT_PATTERNS.captures_at(text, from?)?;
    let parts: Vec<&str> = c.iter().skip(1).flatten().map(|m| m.as_str()).collect();
    let (p0, p1, p2) = (parts.first()?, parts.get(1)?, parts.get(2)?);
    let d = if p0.len() == 4 {
        Date::ymd(p0.parse().ok()?, p1.parse().ok()?, p2.parse().ok()?)
    } else {
        dmy(p0.parse().ok()?, p1.parse().ok()?, p2.parse().ok()?)
    };
    b.check(d)
}
