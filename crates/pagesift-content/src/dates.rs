//! 发布日期：htmldate（Apache-2.0，Adrien Barbaresi）的检索顺序和规则的 Rust 移植，外加自研的多语言日期写法。
//!
//! 按可靠程度依次尝试，找到就停：
//! 1. URL 里的年-月-日（没给 URL 时用 `<link rel=canonical>`）；
//! 2. `<meta>`：约 80 个发布日期属性名（Open Graph、Dublin Core、各 CMS 的私有名字），修改时间只作备选；
//! 3. JSON-LD 里的 `datePublished`；
//! 4. `<abbr>`（Facebook 的 `data-utime`、`class="published"` 的 `title`）；
//! 5. id/class 带日期线索的元素（`date`、`time`、`publish`、`byline`、`meta`、页脚…）、`<title>` 和 `<h1>`、`<time>`；
//! 6. 带时刻的时间戳、`og:image` 地址里的日期、"Posted on / Veröffentlicht am" 这类固定说法；
//! 7. 正文里的短文本片段（取最早的一个）；
//! 8. 整页文本模式统计：版权年份、各种数字日期写法，按出现次数和年份合理性挑选（htmldate 的 search_page）。
//!
//! 和 htmldate 的区别（自研部分）：
//! - htmldate 解析自然语言日期时，英、法、德、印尼、土耳其以外的语言要调用很慢的 dateparser；这里直接内置了
//!   二十来种语言的月份名（含俄语、波兰语等的变格形式，阿拉伯语、波斯语）和中日韩的"年月日"写法，
//!   以及 `26 | 01 | 2022` 这类带空格、竖线的数字写法，不需要外部解析器；
//! - 结果保留页面上写的时区偏移：发布日期按发布方所在时区的那一天算，不换算成 UTC；meta 给 UTC、
//!   JSON-LD 给同一时刻的本地时间时用本地的；
//! - 页头"今日日期"栏（edition、today、current-date）不当发布日期；URL 里不带分隔符的 `/20200428/` 也认；
//! - 后几步的整页文本模式先去掉脚本、样式、SVG 和注释（平均只剩四成），八类数字日期写法和版权年份用一遍
//!   手写扫描代替多个正则（随机测试保证和原正则逐个匹配相同），月份名日期只在年份附近的小窗口里找。
//!   htmldate 在评测集上每页 4 ms，这里 0.06 ms。

use std::collections::HashMap;
use std::sync::LazyLock;
use std::time::{SystemTime, UNIX_EPOCH};

use aho_corasick::AhoCorasick;
use pagesift_html::{Html, Tag};
use regex::Regex;

use crate::dom::{NodeR, element};

/// 一个日期，可能带时刻和时区偏移（秒）。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) struct Date {
    pub y: i32,
    pub m: u32,
    pub d: u32,
    pub time: Option<(u32, u32, u32, Option<i32>)>,
}

impl Date {
    fn ymd(y: i32, m: u32, d: u32) -> Option<Date> {
        valid_ymd(y, m, d).then_some(Date {
            y,
            m,
            d,
            time: None,
        })
    }

    /// 墙上时间，用来比较先后（同 htmldate：按页面上写的时刻比，不换算时区）。
    fn key(&self) -> (i32, u32, u32, u32, u32, u32) {
        let (h, mi, s, _) = self.time.unwrap_or((0, 0, 0, None));
        (self.y, self.m, self.d, h, mi, s)
    }

    /// RFC 3339：有时刻时 `2020-06-07T08:30:00+02:00`（偏移为 0 写 `Z`，没写时区就不带偏移），只有日期时 `2020-06-07`。
    pub(crate) fn render(&self) -> String {
        let mut s = format!("{:04}-{:02}-{:02}", self.y, self.m, self.d);
        if let Some((h, mi, sec, off)) = self.time {
            s.push_str(&format!("T{h:02}:{mi:02}:{sec:02}"));
            match off {
                Some(0) => s.push('Z'),
                Some(o) => {
                    let sign = if o < 0 { '-' } else { '+' };
                    let a = o.abs();
                    s.push_str(&format!("{sign}{:02}:{:02}", a / 3600, a / 60 % 60));
                }
                None => {}
            }
        }
        s
    }
}

fn leap(y: i32) -> bool {
    (y % 4 == 0 && y % 100 != 0) || y % 400 == 0
}

fn valid_ymd(y: i32, m: u32, d: u32) -> bool {
    let days = match m {
        1 | 3 | 5 | 7 | 8 | 10 | 12 => 31,
        4 | 6 | 9 | 11 => 30,
        2 if leap(y) => 29,
        2 => 28,
        _ => return false,
    };
    (1..=days).contains(&d)
}

/// 合理日期的范围：1995-01-01（同 htmldate）到今天。
#[derive(Clone, Copy)]
struct Bounds {
    min: (i32, u32, u32),
    max: (i32, u32, u32),
}

impl Bounds {
    fn now() -> Bounds {
        let days = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .map_or(0, |d| d.as_secs() / 86_400) as i64;
        Bounds {
            min: (1995, 1, 1),
            max: civil_from_days(days),
        }
    }

    fn ok(&self, d: &Date) -> bool {
        let k = (d.y, d.m, d.d);
        self.min <= k && k <= self.max
    }

    fn year_ok(&self, y: i32) -> bool {
        self.min.0 <= y && y <= self.max.0
    }

    fn check(&self, d: Option<Date>) -> Option<Date> {
        d.filter(|d| self.ok(d))
    }
}

fn civil_from_days(z: i64) -> (i32, u32, u32) {
    let z = z + 719_468;
    let era = z.div_euclid(146_097);
    let doe = z.rem_euclid(146_097);
    let yoe = (doe - doe / 1460 + doe / 36_524 - doe / 146_096) / 365;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let d = (doy - (153 * mp + 2) / 5 + 1) as u32;
    let m = if mp < 10 { mp + 3 } else { mp - 9 } as u32;
    let y = (yoe + era * 400 + i64::from(m <= 2)) as i32;
    (y, m, d)
}

/// 两位年份补成四位（同 htmldate 的 correct_year）。
fn full_year(y: i32) -> i32 {
    if y < 100 {
        if y >= 90 { y + 1900 } else { y + 2000 }
    } else {
        y
    }
}

/// 日、月写反了（月份大于 12、日不大于 12）就交换。
fn dmy(day: u32, month: u32, year: i32) -> Option<Date> {
    let (day, month) = if month > 12 && day <= 12 {
        (month, day)
    } else {
        (day, month)
    };
    Date::ymd(full_year(year), month, day)
}

// ---------------------------------------------------------------------------
// 单个日期表达式的解析

const YEAR: &str = "(?:199[0-9]|20[0-3][0-9])";

static YMD_NO_SEP: LazyLock<Regex> = LazyLock::new(|| Regex::new(r"\b(\d{8})\b").unwrap());
static YMD: LazyLock<Regex> = LazyLock::new(|| {
    Regex::new(&format!(
        r"(?:\D|^)(?:({YEAR})[\-/.]([0-1]?[0-9])[\-/.]([0-3]?[0-9])|([0-3]?[0-9])[\-/.]([0-1]?[0-9])[\-/.](\d{{2,4}}))(?:\D|$)"
    ))
    .unwrap()
});
static YM: LazyLock<Regex> = LazyLock::new(|| {
    Regex::new(&format!(
        r"(?:\D|^)(?:({YEAR})[\-/.]([0-1]?[0-9])|([0-1]?[0-9])[\-/.]({YEAR}))(?:\D|$)"
    ))
    .unwrap()
});
/// 分隔符两边有空格或用竖线、圆点：`26 | 01 | 2022`、`2022 · 01 · 26`（自研，htmldate 要交给 dateparser）。
static SPACED: LazyLock<Regex> = LazyLock::new(|| {
    Regex::new(&format!(
        r"(?:\D|^)(?:({YEAR})\s*[|·•/.-]\s*([0-1]?[0-9])\s*[|·•/.-]\s*([0-3]?[0-9])|([0-3]?[0-9])\s*[|·•/.-]\s*([0-1]?[0-9])\s*[|·•/.-]\s*({YEAR}))(?:\D|$)"
    ))
    .unwrap()
});
/// 月份名在前：`June 7, 2020`、`Juni 7 2020`；粘在前一个词上的月份名（`SmithMarch`）按最长后缀找。
static LONG_MDY: LazyLock<Regex> = LazyLock::new(|| {
    Regex::new(&format!(
        r"(?i)(\p{{L}}{{3,}})\.?\s+([0-3]?[0-9])(?:st|nd|rd|th)?,?\s+({YEAR})"
    ))
    .unwrap()
});
/// 日在前：`7 June 2020`、`7. Juni 2020`、`7th of June, 2020`、`7 de junio de 2020`、`7 июня 2020`。
static LONG_DMY: LazyLock<Regex> = LazyLock::new(|| {
    Regex::new(&format!(
        r"(?i)([0-3]?[0-9])(?:st|nd|rd|th|\.)?\s+(?:of\s+|de\s+)?(\p{{L}}{{3,}})[,.]?\s+(?:de\s+|del\s+)?({YEAR})"
    ))
    .unwrap()
});
/// 年在前：`2020. június 7.`（匈牙利语）、`2020 June 7`。
static LONG_YMD: LazyLock<Regex> = LazyLock::new(|| {
    Regex::new(&format!(
        r"(?i)({YEAR})\.?\s+(\p{{L}}{{3,}})\.?\s+([0-3]?[0-9])(?:\D|$)"
    ))
    .unwrap()
});
/// 中日韩：`2020年6月7日`、`2020년 6월 7일`。
static CJK: LazyLock<Regex> = LazyLock::new(|| {
    Regex::new(&format!(
        r"({YEAR})\s*[年년]\s*([0-1]?[0-9])\s*[月월]\s*([0-3]?[0-9])\s*[日일]?"
    ))
    .unwrap()
});

/// 各语言的月份名（小写，土耳其语的 ı 折成 i）→ 月份。
static MONTHS: LazyLock<HashMap<String, u32>> = LazyLock::new(|| {
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

fn fold(s: &str) -> String {
    s.replace(['ı', 'İ'], "i").to_lowercase()
}

/// 月份名：先整词查；月份在前时允许粘在前一个词后面（取最长的已知后缀，同 htmldate）。
fn month_of(word: &str, glued: bool) -> Option<u32> {
    let w = fold(word);
    if let Some(&m) = MONTHS.get(&w) {
        return Some(m);
    }
    if !glued {
        return None;
    }
    let chars: Vec<char> = w.chars().collect();
    let tail = &chars[chars.len().saturating_sub(12)..];
    (0..tail.len().saturating_sub(2))
        .find_map(|i| MONTHS.get(&tail[i..].iter().collect::<String>()).copied())
}

/// 自然语言写法（月份名、"年月日"），返回第一个合法的（同 htmldate 的 regex_parse，覆盖更多语言）。
fn parse_long(s: &str) -> Option<Date> {
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
fn zone(c: &mut Cursor<'_>) -> Option<Option<i32>> {
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
fn parse_iso(s: &str, strict: bool) -> Option<Date> {
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
fn parse_rfc2822(s: &str) -> Option<Date> {
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
            Some(z) => {
                let mut c = Cursor {
                    b: z.as_bytes(),
                    i: 0,
                };
                zone(&mut c)?
            }
            None => None,
        };
        if h > 23 || mi > 59 || sec > 60 {
            return None;
        }
        date.time = Some((h, mi, sec.min(59), off));
    }
    Some(date)
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
fn custom_parse(s: &str, b: &Bounds) -> Option<Date> {
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
    if let Some(c) = YMD.captures(s) {
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
    if let Some(c) = YM.captures(s) {
        let (y, m) = match c.get(1) {
            Some(y) => (y.as_str(), &c[2]),
            None => (&c[4], &c[3]),
        };
        if let Some(d) = b.check(Date::ymd(y.parse().ok()?, m.parse().ok()?, 1)) {
            return Some(d);
        }
    }
    if let Some(c) = SPACED.captures(s) {
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
    b.check(parse_long(s))
}

static DISCARD: LazyLock<Regex> = LazyLock::new(|| {
    Regex::new(
        r"^\d{2}:\d{2}(?: |:|$)|^\D*\d{4}\D*$|[$€¥Ұ£¢₽₱฿#₹]|[A-Z]{3}[^A-Z]|(?:^|\D)(?:\+\d{2}|\d{3}|\d{5})\D|ftps?|https?|sftp|\.(?:com|net|org|info|gov|edu|de|fr|io)\b|IBAN|[A-Z]{2}[0-9]{2}|®",
    )
    .unwrap()
});

const MIN_SEGMENT: usize = 6;
const MAX_SEGMENT: usize = 52;

fn trim_text(s: &str) -> String {
    s.split_whitespace().collect::<Vec<_>>().join(" ")
}

fn cut(s: &str, max_chars: usize) -> &str {
    match s.char_indices().nth(max_chars) {
        Some((i, _)) => &s[..i],
        None => s,
    }
}

/// htmldate 的 try_date_expr：一小段文本里的日期。数字太少或太多、像时刻、价格、电话、网址的都不要。
fn date_expr(s: &str, b: &Bounds) -> Option<Date> {
    let t = trim_text(s);
    let t = cut(&t, MAX_SEGMENT);
    let digits = t.chars().filter(char::is_ascii_digit).count();
    if !(4..=18).contains(&digits) || DISCARD.is_match(t) {
        return None;
    }
    custom_parse(t, b)
}

/// htmldate 的 examine_text：元素文本，去掉结尾的非数字部分再解析。
fn examine_text(s: &str, b: &Bounds) -> Option<Date> {
    let t = trim_text(s);
    if t.chars().count() <= MIN_SEGMENT {
        return None;
    }
    let t = cut(&t, MAX_SEGMENT);
    let t = t.trim_end_matches(|c: char| !c.is_ascii_digit());
    date_expr(t, b)
}

// ---------------------------------------------------------------------------
// 页面上的检索

static URL_DATE: LazyLock<Regex> = LazyLock::new(|| {
    Regex::new(&format!(
        r"\D({YEAR})[/_-]([0-1]?[0-9])[/_-]([0-3]?[0-9])(?:\D|$)"
    ))
    .unwrap()
});

/// URL 路径里不带分隔符的 `/20200428/`（前后是 `/`、`-`、`_`、`.` 或结尾，长 ID 不算；自研）。
static URL_DATE_COMPACT: LazyLock<Regex> = LazyLock::new(|| {
    Regex::new(&format!(
        r"/({YEAR})(0[1-9]|1[0-2])([0-3][0-9])(?:[/_.-]|$)"
    ))
    .unwrap()
});

fn url_date(url: &str, b: &Bounds) -> Option<Date> {
    let path = url.split(['?', '#']).next().unwrap_or(url);
    let from = |re: &Regex, s: &str| {
        let c = re.captures(s)?;
        b.check(Date::ymd(
            c[1].parse().ok()?,
            c[2].parse().ok()?,
            c[3].parse().ok()?,
        ))
    };
    from(&URL_DATE, url).or_else(|| from(&URL_DATE_COMPACT, path))
}

/// 表示发布日期的 meta 名字（htmldate 的 DATE_ATTRIBUTES）。
const DATE_ATTRIBUTES: &[&str] = &[
    "analyticsattributes.articledate",
    "article.created",
    "article_date_original",
    "article:post_date",
    "article.published",
    "article:published",
    "article:published_date",
    "article:published_time",
    "article:publicationdate",
    "bt:pubdate",
    "citation_date",
    "citation_publication_date",
    "content_create_date",
    "created",
    "cxenseparse:recs:publishtime",
    "date",
    "date_created",
    "date_published",
    "datecreated",
    "dateposted",
    "datepublished",
    "dc.date",
    "dc.created",
    "dc.date.created",
    "dc.date.issued",
    "dc.date.publication",
    "dcsext.articlefirstpublished",
    "dcterms.created",
    "dcterms.date",
    "dcterms.issued",
    "dc:created",
    "dc:date",
    "displaydate",
    "doc_date",
    "field-name-post-date",
    "gentime",
    "mediator_published_time",
    "meta",
    "og:article:published",
    "og:article:published_time",
    "og:datepublished",
    "og:pubdate",
    "og:publish_date",
    "og:published_time",
    "og:question:published_time",
    "og:regdate",
    "originalpublicationdate",
    "parsely-pub-date",
    "pdate",
    "ptime",
    "pubdate",
    "publishdate",
    "publish_date",
    "publish_time",
    "publish-date",
    "published-date",
    "published_date",
    "published_time",
    "publisheddate",
    "publication_date",
    "rbpubdate",
    "release_date",
    "rnews:datepublished",
    "sailthru.date",
    "shareaholic:article_published_time",
    "timestamp",
    "twt-published-at",
    "video:release_date",
    "vr:published_time",
];
const NAME_MODIFIED: &[&str] = &[
    "lastdate",
    "lastmod",
    "lastmodified",
    "last-modified",
    "modified",
    "utime",
];
const PROPERTY_MODIFIED: &[&str] = &[
    "article:modified",
    "article:modified_date",
    "article:modified_time",
    "article:post_modified",
    "bt:moddate",
    "datemodified",
    "dc.modified",
    "dcterms.modified",
    "lastmodified",
    "modified_time",
    "modificationdate",
    "og:article:modified_time",
    "og:modified_time",
    "og:updated_time",
    "release_date",
    "revision_date",
    "updated_time",
];

/// htmldate 的 examine_header（原始发布日期模式）。
fn examine_header(metas: &[NodeR<'_>], b: &Bounds) -> Option<Date> {
    let mut reserve: Option<Date> = None;
    for node in metas {
        let Some(el) = element(node) else { continue };
        let content = el.attr("content");
        if content.is_none() && el.attr("datetime").is_none() {
            continue;
        }
        let tryf = |s: Option<&str>| s.and_then(|s| date_expr(s, b));
        let mut found = None;
        if let Some(name) = el.attr("name") {
            let a = name.to_ascii_lowercase();
            if a == "og:url" {
                reserve = content.and_then(|c| url_date(c, b)).or(reserve);
            } else if DATE_ATTRIBUTES.contains(&a.as_str()) {
                found = tryf(content);
            } else if NAME_MODIFIED.contains(&a.as_str()) {
                reserve = tryf(content).or(reserve);
            }
        } else if let Some(prop) = el.attr("property") {
            let a = prop.to_ascii_lowercase();
            if DATE_ATTRIBUTES.contains(&a.as_str()) {
                found = tryf(content);
            } else if PROPERTY_MODIFIED.contains(&a.as_str()) {
                reserve = tryf(content).or(reserve);
            }
        } else if let Some(ip) = el.attr("itemprop") {
            let a = ip.to_ascii_lowercase();
            if matches!(a.as_str(), "datecreated" | "datepublished" | "pubyear") {
                found = tryf(el.attr("datetime").or(content));
            } else if a == "copyrightyear"
                && let Some(y) = content.and_then(|c| c.trim().parse::<i32>().ok())
                && let Some(d) = b.check(Date::ymd(y, 1, 1))
            {
                reserve = Some(d);
            }
        } else if el
            .attr("pubdate")
            .is_some_and(|p| p.eq_ignore_ascii_case("pubdate"))
        {
            found = tryf(content);
        } else if let Some(he) = el.attr("http-equiv") {
            let a = he.to_ascii_lowercase();
            if a == "date" {
                found = tryf(content);
            } else if a == "last-modified" {
                reserve = tryf(content).or(reserve);
            }
        }
        if found.is_some() {
            return found;
        }
    }
    reserve
}

static JSON_PUBLISHED: LazyLock<Regex> = LazyLock::new(|| {
    Regex::new(&format!(
        r#"(?i)"datePublished": ?"({YEAR}-[0-1]?[0-9]-[0-3]?[0-9](?:[T ][0-9]{{2}}:[0-9]{{2}}:[0-9]{{2}}(?:\.[0-9]+)?(?:Z|[+-][0-9]{{2}}:?[0-9]{{2}})?)?)"#
    ))
    .unwrap()
});

/// JSON-LD（和 `application/settings+json`）里第一个 `"datePublished"`。
fn json_search(scripts: &[NodeR<'_>], b: &Bounds) -> Option<Date> {
    for node in scripts {
        let Some(el) = element(node) else { continue };
        let ty = el.attr("type").unwrap_or("").trim().to_ascii_lowercase();
        if ty != "application/ld+json" && ty != "application/settings+json" {
            continue;
        }
        let text: String = node
            .children()
            .filter_map(|c| c.as_text().map(str::to_string))
            .collect();
        if !text.contains("\"date") {
            continue;
        }
        if let Some(c) = JSON_PUBLISHED.captures(&text)
            && let Some(d) = b.check(parse_iso(&c[1], false))
        {
            return Some(d);
        }
    }
    None
}

/// 换算成 UTC 的秒数（只有带时区的才能换算）。
fn instant(d: &Date) -> Option<i64> {
    let (h, mi, s, off) = d.time?;
    let off = off?;
    let days = days_from_civil(d.y, d.m, d.d);
    Some(days * 86_400 + i64::from(h * 3600 + mi * 60 + s) - i64::from(off))
}

fn days_from_civil(y: i32, m: u32, d: u32) -> i64 {
    let y = i64::from(y) - i64::from(m <= 2);
    let era = y.div_euclid(400);
    let yoe = y.rem_euclid(400);
    let mp = (i64::from(m) + 9) % 12;
    let doy = (153 * mp + 2) / 5 + i64::from(d) - 1;
    let doe = yoe * 365 + yoe / 4 - yoe / 100 + doy;
    era * 146_097 + doe - 719_468
}

/// meta 给的是 UTC 时刻，而 JSON-LD 给的是同一时刻的本地时间：用本地的，发布日期按发布方当地的那一天算（自研规则）。
fn prefer_local(found: Option<Date>, other: impl Fn() -> Option<Date>) -> Option<Date> {
    let d = found?;
    if d.time.is_some_and(|t| t.3 == Some(0))
        && let Some(o) = other()
        && o.time.is_some_and(|t| t.3.is_some_and(|x| x != 0))
        && instant(&o) == instant(&d)
    {
        return Some(o);
    }
    Some(d)
}

fn earliest(a: Option<Date>, c: Date) -> Option<Date> {
    Some(match a {
        Some(a) if a.key() <= c.key() => a,
        _ => c,
    })
}

const MAX_CANDIDATES: usize = 1000;

/// 元素文本的开头部分：空白折叠后够 [`MAX_SEGMENT`] 个字符就停（后面的规则只看这么多），
/// 不用拼出整棵子树的文本（候选里常有整个页脚这样的大元素）。
fn text_prefix(node: &NodeR<'_>) -> String {
    let mut out = String::new();
    let mut seen = 0usize;
    let mut in_ws = true;
    for d in node.descendants() {
        let Some(t) = d.as_text() else { continue };
        out.push_str(t);
        for c in t.chars() {
            if c.is_whitespace() {
                if !in_ws {
                    seen += 1;
                    in_ws = true;
                }
            } else {
                seen += 1;
                in_ws = false;
            }
        }
        if seen > MAX_SEGMENT + 1 {
            break;
        }
    }
    out
}

/// htmldate 的 examine_elements：候选元素的文本和 `title` 属性，第一个能解析的。
fn examine_elements(nodes: &[NodeR<'_>], b: &Bounds) -> Option<Date> {
    if nodes.is_empty() || nodes.len() > MAX_CANDIDATES {
        return None;
    }
    nodes.iter().find_map(|n| {
        let text = text_prefix(n);
        examine_text(&text, b)
            .or_else(|| element(n)?.attr("title").and_then(|t| examine_text(t, b)))
    })
}

/// htmldate 的 examine_abbr_elements。
fn examine_abbr(abbrs: &[NodeR<'_>], b: &Bounds) -> Option<Date> {
    if abbrs.is_empty() || abbrs.len() > MAX_CANDIDATES {
        return None;
    }
    let mut reference = None;
    for n in abbrs {
        let el = element(n)?;
        if let Some(ut) = el.attr("data-utime") {
            if let Ok(secs) = ut.trim().parse::<i64>() {
                let (y, m, d) = civil_from_days(secs.div_euclid(86_400));
                let r = secs.rem_euclid(86_400) as u32;
                let date = Date {
                    y,
                    m,
                    d,
                    time: Some((r / 3600, r / 60 % 60, r % 60, Some(0))),
                };
                reference = earliest(reference, date);
            }
        } else if el
            .attr("class")
            .is_some_and(|c| matches!(c, "date-published" | "published" | "time published"))
        {
            if let Some(t) = el.attr("title") {
                if let Some(d) = date_expr(t, b) {
                    return Some(d);
                }
            } else {
                let text = text_prefix(n);
                if text.len() > 10
                    && let Some(d) = date_expr(&text, b)
                {
                    reference = earliest(reference, d);
                }
            }
        }
    }
    b.check(reference).or_else(|| examine_elements(abbrs, b))
}

/// htmldate 的 examine_time_elements（原始发布日期模式：`pubdate` 或 `entry-date` 类直接采用，否则取最早）。
fn examine_time(times: &[NodeR<'_>], b: &Bounds) -> Option<Date> {
    if times.is_empty() || times.len() > MAX_CANDIDATES {
        return None;
    }
    let mut reference = None;
    for n in times {
        let el = element(n)?;
        let dt = el.attr("datetime").unwrap_or("");
        if dt.len() > 6 {
            let class = el.attr("class").unwrap_or("");
            if el.attr("pubdate") == Some("pubdate")
                || class.starts_with("entry-date")
                || class.starts_with("entry-time")
            {
                if let Some(d) = date_expr(dt, b) {
                    return Some(d);
                }
            } else if let Some(d) = date_expr(dt, b) {
                reference = earliest(reference, d);
            }
        } else {
            let own: String = n
                .children()
                .filter_map(|c| c.as_text().map(str::to_string))
                .collect();
            if own.len() > 6
                && let Some(d) = date_expr(&own, b)
            {
                reference = earliest(reference, d);
            }
        }
    }
    b.check(reference)
}

/// 元素的 id/class 里有没有日期线索（htmldate 的 is_date_candidate：id 和 class 只看源码里先出现的那个）。
/// 候选元素的 id/class 线索，按用途分组（同 htmldate 的 is_date_candidate），一个自动机一遍扫完。
const CUE_DATE: u8 = 1;
const CUE_FIRST: u8 = 2;
const CUE_CLASS: u8 = 4;
const CUE_LASTMOD: u8 = 8;
const CUES: &[(&str, u8)] = &[
    ("date", CUE_DATE),
    ("Date", CUE_DATE),
    ("datum", CUE_DATE),
    ("Datum", CUE_DATE),
    ("meta", CUE_FIRST),
    ("Meta", CUE_FIRST),
    ("time", CUE_FIRST),
    ("publish", CUE_FIRST),
    ("footer", CUE_FIRST),
    ("info", CUE_CLASS),
    ("post_detail", CUE_CLASS),
    ("block-content", CUE_CLASS),
    ("byline", CUE_CLASS),
    ("subline", CUE_CLASS),
    ("posted", CUE_CLASS),
    ("submitted", CUE_CLASS),
    ("created-post", CUE_CLASS),
    ("publication", CUE_CLASS),
    ("author", CUE_CLASS),
    ("autor", CUE_CLASS),
    ("field-content", CUE_CLASS),
    ("fa-clock-o", CUE_CLASS),
    ("fa-calendar", CUE_CLASS),
    ("fecha", CUE_CLASS),
    ("parution", CUE_CLASS),
    ("footer-info-lastmod", CUE_LASTMOD),
];
static AC_CUES: LazyLock<AhoCorasick> =
    LazyLock::new(|| AhoCorasick::new(CUES.iter().map(|c| c.0)).unwrap());
static AC_BANNER: LazyLock<AhoCorasick> = LazyLock::new(|| {
    AhoCorasick::builder()
        .ascii_case_insensitive(true)
        .build(["edition", "today", "current-date", "currentdate"])
        .unwrap()
});

/// 属性值命中了哪些分组。
fn cue_bits(v: &str) -> u8 {
    AC_CUES
        .find_overlapping_iter(v)
        .fold(0, |acc, m| acc | CUES[m.pattern().as_usize()].1)
}

fn is_date_candidate(el: &pagesift_html::Element<'_>) -> bool {
    let (mut first, mut itemprop, mut class, mut id) = (None, None, 0u8, 0u8);
    for (k, v) in el.attrs() {
        match k {
            "itemprop" => itemprop = Some(v),
            "class" if first.is_none() || class == 0 => {
                class = cue_bits(v);
                first.get_or_insert(class);
            }
            "id" if first.is_none() || id == 0 => {
                id = cue_bits(v);
                first.get_or_insert(id);
            }
            _ => {}
        }
    }
    // htmldate 把 D 换成 d 再找 date/datum，等价于找这四种写法
    if first.is_some_and(|f| f & CUE_DATE != 0)
        || itemprop.is_some_and(|v| cue_bits(v) & CUE_DATE != 0)
    {
        return true;
    }
    let Some(first) = first else { return false };
    first & CUE_FIRST != 0 || class & CUE_CLASS != 0 || id & CUE_LASTMOD != 0
}

/// "今日日期"栏（class/id 带 edition、today、current-date）：显示的是访问当天，不是发布日期（自研规则）。
fn is_banner(node: &NodeR<'_>, el: &pagesift_html::Element<'_>) -> bool {
    let named = |e: &pagesift_html::Element<'_>| {
        [e.attr("class"), e.attr("id")]
            .into_iter()
            .flatten()
            .any(|v| AC_BANNER.is_match(v))
    };
    named(el)
        || node
            .ancestors()
            .take(3)
            .filter_map(|a| element(&a))
            .any(|a| named(&a))
}

/// 清理时整段跳过的元素（htmldate 的 CLEANING_LIST，和 archive.org 的横幅）。
fn pruned(tag: Tag, el: &pagesift_html::Element<'_>) -> bool {
    match tag {
        Tag::Applet
        | Tag::Audio
        | Tag::Canvas
        | Tag::Datalist
        | Tag::Embed
        | Tag::Frame
        | Tag::Frameset
        | Tag::Iframe
        | Tag::Label
        | Tag::Map
        | Tag::Math
        | Tag::Noframes
        | Tag::Object
        | Tag::Picture
        | Tag::Svg
        | Tag::Track
        | Tag::Video => true,
        Tag::Other => el.name() == "rdf",
        Tag::Div => matches!(el.attr("id"), Some("wm-ipp-base" | "wm-ipp")),
        _ => false,
    }
}

static TIMESTAMP: LazyLock<Regex> = LazyLock::new(|| {
    Regex::new(&format!(
        r"({YEAR}-[0-1]?[0-9]-[0-3]?[0-9])[T ][0-9]{{2}}:[0-9]{{2}}:[0-9]{{2}}(?:\.[0-9]+)?(?:Z|[+-][0-9]{{2}}:?[0-9]{{2}})?"
    ))
    .unwrap()
});

fn first_timestamp(text: &str, b: &Bounds) -> Option<Date> {
    let c = TIMESTAMP.captures(text)?;
    b.check(parse_iso(&c[0], false)).map(|mut d| {
        d.time = None;
        d
    })
}

static TEXT_PATTERNS: LazyLock<Regex> = LazyLock::new(|| {
    Regex::new(concat!(
        r#"(?i)(?:date[^0-9"]{0,20}|updated|last-modified|published|posted|on)[ :]{0,9}?([0-9]{1,4})[./]([0-9]{1,2})[./]([0-9]{2,4})|"#,
        r"(?:Datum|Stand|Veröffentlicht am):? ?([0-9]{1,2})\.([0-9]{1,2})\.([0-9]{2,4})|",
        r"(?:güncellen?me|yayı(?:m|n)lan?ma) {0,9}?(?:tarihi)? {0,9}?:? {0,9}?([0-9]{1,2})[./]([0-9]{1,2})[./]([0-9]{2,4})|",
        r"([0-9]{1,2})[./]([0-9]{1,2})[./]([0-9]{2,4}) {0,9}?(?:'de|'da|'te|'ta|’de|’da|’te|’ta|tarihinde) {0,9}(?:güncellendi|yayı(?:m|n)landı)",
    ))
    .unwrap()
});

/// 各种说法里必有的关键词（不含 "on"，它单独判断）。
static AC_PHRASES: LazyLock<AhoCorasick> = LazyLock::new(|| {
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

static DATE_CORE: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(r"[0-9]{1,4}[./][0-9]{1,2}[./][0-9]{2,4}").unwrap());

/// htmldate 的 idiosyncrasies_search："Posted on 07/06/2020"、"Veröffentlicht am 7.6.2020" 这类说法。
fn idiosyncrasies(text: &str, b: &Bounds) -> Option<Date> {
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

// ---- 整页模式统计（htmldate 的 search_page）

/// 按出现次数挑候选（htmldate 的 select_candidate，原始发布日期模式）。
/// `items` 是匹配到的字符串（按首次出现顺序）和次数；返回选中的字符串。
fn select_candidate(
    items: &[(String, usize)],
    year_of: impl Fn(&str) -> Option<i32>,
    b: &Bounds,
) -> Option<String> {
    if items.is_empty() || items.len() > MAX_CANDIDATES {
        return None;
    }
    if items.len() == 1 {
        return Some(items[0].0.clone());
    }
    // 次数最多的 10 个（次数相同按首次出现顺序），再按字符串升序取前两个
    let mut by_count: Vec<&(String, usize)> = items.iter().collect();
    by_count.sort_by_key(|a| std::cmp::Reverse(a.1));
    let mut top: Vec<&(String, usize)> = by_count.into_iter().take(10).collect();
    top.sort_by(|a, c| a.0.cmp(&c.0).then(a.1.cmp(&c.1)));
    let best = &top[..2];
    let years: Vec<Option<i32>> = best.iter().map(|(p, _)| year_of(p)).collect();
    let valid: Vec<bool> = years
        .iter()
        .map(|y| y.is_some_and(|y| b.year_ok(y)))
        .collect();
    if valid.iter().all(|&v| v) {
        let (c0, c1) = (best[0].1, best[1].1);
        if c0 != c1 && years[1] != years[0] && c1 as f64 / c0 as f64 > 0.5 {
            Some(best[1].0.clone())
        } else {
            Some(best[0].0.clone())
        }
    } else {
        valid.iter().position(|&v| v).map(|i| best[i].0.clone())
    }
}

fn leading_year(s: &str) -> Option<i32> {
    let t = s.strip_prefix(|c: char| !c.is_ascii_digit()).unwrap_or(s);
    let y: i32 = t.get(..4)?.parse().ok()?;
    ((1990..=2039).contains(&y)).then_some(y)
}

fn trailing_year(s: &str) -> Option<i32> {
    let t = s.trim_end_matches(|c: char| !c.is_ascii_digit());
    let digits: String = t
        .chars()
        .rev()
        .take_while(char::is_ascii_digit)
        .collect::<Vec<_>>()
        .into_iter()
        .rev()
        .collect();
    if digits.len() >= 4 {
        let y: i32 = digits[digits.len() - 4..].parse().ok()?;
        (1990..=2039).contains(&y).then_some(y)
    } else if digits.len() == 2 {
        digits.parse().ok()
    } else {
        None
    }
}

#[cfg(test)]
macro_rules! re {
    ($name:ident, $pat:expr) => {
        static $name: LazyLock<Regex> =
            LazyLock::new(|| Regex::new(AsRef::<str>::as_ref(&$pat)).unwrap());
    };
}

#[cfg(test)]
re!(
    COPYRIGHT,
    format!(r"(?:©|&copy;|Copyright|\(c\))\D{{0,99}}(?:{YEAR})?-?({YEAR})\D")
);
#[cfg(test)]
re!(THREE, r"/([0-9]{4}/[0-9]{2}/[0-9]{2})[01/]");
#[cfg(test)]
re!(THREE_LOOSE, r"\D([0-9]{4}[/.-][0-9]{2}[/.-][0-9]{2})\D");
#[cfg(test)]
re!(
    SELECT_YMD,
    r"\D([0-3]?[0-9][/.-][0-1]?[0-9][/.-][0-9]{4})\D"
);
#[cfg(test)]
re!(
    DATESTRINGS,
    r"\D((?:19[0-9]{2}|20[0-9]{2})[01][0-9][0-3][0-9])\D"
);
#[cfg(test)]
re!(
    SLASHES,
    r"\D([0-3]?[0-9]/[0-1]?[0-9]/[0129][0-9]|[0-3][0-9]\.[01][0-9]\.[0129][0-9])\D"
);
#[cfg(test)]
re!(YYYYMM, r"\D([12][0-9]{3}[/.-](?:1[0-2]|0[1-9]))\D");
#[cfg(test)]
re!(MMYYYY, r"\D([0-1]?[0-9][/.-][12][0-9]{3})\D");
#[cfg(test)]
re!(SIMPLE, format!(r"\D({YEAR})\D"));

fn split3(s: &str) -> Option<(i64, i64, i64)> {
    let mut it = s.split(['/', '.', '-']).map(|p| p.parse::<i64>().ok());
    Some((it.next()??, it.next()??, it.next()??))
}

/// 版权年份之前的日期不可信（filter_ymd_candidate）。
fn after_copyright(d: Option<Date>, copyear: i32, b: &Bounds) -> Option<Date> {
    b.check(d).filter(|d| copyear == 0 || d.y >= copyear)
}

/// 整页里各类数字日期写法的所有匹配：一遍扫过所有数字串的起点，同时判断八类写法（自研，代替八个正则各扫一遍）。
/// 每类单独记录上一次匹配的结束位置，语义和对应正则（`\D(…)\D` 这类）的 find_iter 一致：
/// 匹配互不重叠，前后的分隔字符会被吃掉。
#[derive(Default)]
struct Numeric<'t> {
    /// `/YYYY/MM/DD[01/]`
    three: Vec<&'t str>,
    /// `YYYY-MM-DD`（分隔符 `/.-`）
    three_loose: Vec<&'t str>,
    /// `DD-MM-YYYY`
    select_ymd: Vec<&'t str>,
    /// `YYYYMMDD`
    datestrings: Vec<&'t str>,
    /// `DD/MM/YY`、`DD.MM.YY`
    slashes: Vec<&'t str>,
    /// `YYYY-MM`
    yyyymm: Vec<&'t str>,
    /// `MM-YYYY`
    mmyyyy: Vec<&'t str>,
    /// 单独的年份
    simple: Vec<&'t str>,
}

fn scan_numeric(text: &str) -> Numeric<'_> {
    let b = text.as_bytes();
    let n = b.len();
    let dig = |i: usize| i < n && b[i].is_ascii_digit();
    let run = |i: usize| {
        b[i.min(n)..]
            .iter()
            .take_while(|c| c.is_ascii_digit())
            .count()
    };
    let sep = |i: usize| i < n && matches!(b[i], b'/' | b'.' | b'-');
    // `\D`：那里有一个字符而且不是数字；返回这个字符之后的位置
    let nd = |i: usize| -> Option<usize> {
        if i >= n || b[i].is_ascii_digit() {
            return None;
        }
        let mut j = i + 1;
        while j < n && !text.is_char_boundary(j) {
            j += 1;
        }
        Some(j)
    };
    let mut out = Numeric::default();
    let mut last = [0usize; 8];
    let mut s = 1;
    while s < n {
        if !b[s].is_ascii_digit() || b[s - 1].is_ascii_digit() {
            s += 1;
            continue;
        }
        let r = run(s);
        let free = |k: usize, last: &[usize; 8]| s > last[k];
        // 0 THREE
        if free(0, &last)
            && b[s - 1] == b'/'
            && r == 4
            && b.get(s + 4) == Some(&b'/')
            && dig(s + 5)
            && dig(s + 6)
            && b.get(s + 7) == Some(&b'/')
            && dig(s + 8)
            && dig(s + 9)
            && b.get(s + 10)
                .is_some_and(|c| matches!(c, b'0' | b'1' | b'/'))
        {
            out.three.push(&text[s..s + 10]);
            last[0] = s + 11;
        }
        // 1 THREE_LOOSE
        if free(1, &last)
            && r == 4
            && sep(s + 4)
            && dig(s + 5)
            && dig(s + 6)
            && sep(s + 7)
            && dig(s + 8)
            && dig(s + 9)
            && let Some(e) = nd(s + 10)
        {
            out.three_loose.push(&text[s..s + 10]);
            last[1] = e;
        }
        // 日、月：1 位，或 2 位且首位不超过 max
        let small = |i: usize, len: usize, max: u8| len == 1 || (len == 2 && b[i] <= max);
        // 2 SELECT_YMD
        if free(2, &last) && small(s, r, b'3') && sep(s + r) {
            let m = s + r + 1;
            let mr = run(m);
            if small(m, mr, b'1') && sep(m + mr) {
                let y = m + mr + 1;
                if run(y) == 4
                    && let Some(e) = nd(y + 4)
                {
                    out.select_ymd.push(&text[s..y + 4]);
                    last[2] = e;
                }
            }
        }
        // 3 DATESTRINGS
        if free(3, &last)
            && r == 8
            && (&b[s..s + 2] == b"19" || &b[s..s + 2] == b"20")
            && matches!(b[s + 4], b'0' | b'1')
            && matches!(b[s + 6], b'0'..=b'3')
            && let Some(e) = nd(s + 8)
        {
            out.datestrings.push(&text[s..s + 8]);
            last[3] = e;
        }
        // 4 SLASHES
        if free(4, &last) {
            let mut hit = None;
            if small(s, r, b'3') && b.get(s + r) == Some(&b'/') {
                let m = s + r + 1;
                let mr = run(m);
                if small(m, mr, b'1') && b.get(m + mr) == Some(&b'/') {
                    let y = m + mr + 1;
                    if run(y) == 2
                        && matches!(b[y], b'0' | b'1' | b'2' | b'9')
                        && let Some(e) = nd(y + 2)
                    {
                        hit = Some((y + 2, e));
                    }
                }
            }
            if hit.is_none()
                && r == 2
                && b[s] <= b'3'
                && b.get(s + 2) == Some(&b'.')
                && run(s + 3) == 2
                && b[s + 3] <= b'1'
                && b.get(s + 5) == Some(&b'.')
                && run(s + 6) == 2
                && matches!(b[s + 6], b'0' | b'1' | b'2' | b'9')
                && let Some(e) = nd(s + 8)
            {
                hit = Some((s + 8, e));
            }
            if let Some((g, e)) = hit {
                out.slashes.push(&text[s..g]);
                last[4] = e;
            }
        }
        // 5 YYYYMM
        if free(5, &last)
            && r == 4
            && matches!(b[s], b'1' | b'2')
            && sep(s + 4)
            && run(s + 5) == 2
            && matches!(&b[s + 5..s + 7], [b'1', b'0'..=b'2'] | [b'0', b'1'..=b'9'])
            && let Some(e) = nd(s + 7)
        {
            out.yyyymm.push(&text[s..s + 7]);
            last[5] = e;
        }
        // 6 MMYYYY
        if free(6, &last) && small(s, r, b'1') && sep(s + r) {
            let y = s + r + 1;
            if run(y) == 4
                && matches!(b[y], b'1' | b'2')
                && let Some(e) = nd(y + 4)
            {
                out.mmyyyy.push(&text[s..y + 4]);
                last[6] = e;
            }
        }
        // 7 SIMPLE（w3.org 的 DTD 地址里的年份不算）
        if free(7, &last)
            && r == 4
            && leading_year(&text[s..s + 4]).is_some()
            && !b[..s - 1].ends_with(b"w3.org")
            && let Some(e) = nd(s + 4)
        {
            out.simple.push(&text[s..s + 4]);
            last[7] = e;
        }
        s += r.max(1);
    }
    out
}

/// 一组匹配按首次出现顺序计数；年份不在合理范围的去掉（plausible_year_filter）。
fn count(
    items: &[&str],
    year_of: impl Fn(&str) -> Option<i32>,
    b: &Bounds,
) -> Vec<(String, usize)> {
    let mut order: Vec<(String, usize)> = Vec::new();
    let mut index: HashMap<&str, usize> = HashMap::new();
    for &s in items {
        match index.get(s) {
            Some(&i) => order[i].1 += 1,
            None => {
                index.insert(s, order.len());
                order.push((s.to_string(), 1));
            }
        }
    }
    order.retain(|(s, _)| year_of(s).is_some_and(|y| b.year_ok(full_year(y))));
    order
}

/// 自然语言写法的日期：只在每个四位年份附近的小窗口里找（自研，代替整页跑几个 Unicode 正则），返回第一个。
fn parse_long_scan(text: &str) -> Option<Date> {
    let b = text.as_bytes();
    let floor = |mut i: usize| {
        while !text.is_char_boundary(i) {
            i -= 1;
        }
        i
    };
    let ends_ci = |t: &str, suf: &str| {
        t.len() >= suf.len()
            && t.as_bytes()[t.len() - suf.len()..].eq_ignore_ascii_case(suf.as_bytes())
    };
    let mut s = 0;
    while s + 4 <= b.len() {
        if !(b[s].is_ascii_digit() && (s == 0 || !b[s - 1].is_ascii_digit())) {
            s += 1;
            continue;
        }
        let r = b[s..].iter().take_while(|c| c.is_ascii_digit()).count();
        if r == 4 && leading_year(&text[s..s + 4]).is_some() {
            // 年份前后的样子先粗筛一下，只有可能是"日 月 年""月 日, 年""年 月 日""年月日"时才跑模式
            let before = &text[floor(s.saturating_sub(64))..s];
            let after = &text[s + 4..floor((s + 4 + 24).min(b.len()))];
            let mut t = before.trim_end_matches(char::is_whitespace);
            let spaced = t.len() < before.len();
            for de in ["del", "de"] {
                if ends_ci(t, de) && t[..t.len() - de.len()].ends_with(char::is_whitespace) {
                    t = t[..t.len() - de.len()].trim_end_matches(char::is_whitespace);
                    break;
                }
            }
            let month_before = t
                .trim_end_matches([',', '.'])
                .chars()
                .next_back()
                .is_some_and(char::is_alphabetic);
            let mut d = before
                .trim_end_matches(char::is_whitespace)
                .trim_end_matches(',');
            for suf in ["st", "nd", "rd", "th"] {
                if ends_ci(d, suf) {
                    d = &d[..d.len() - 2];
                    break;
                }
            }
            let day_before = d.ends_with(|c: char| c.is_ascii_digit());
            let a = after.strip_prefix('.').unwrap_or(after);
            let month_after = a.starts_with(char::is_whitespace)
                && a.trim_start_matches(char::is_whitespace)
                    .starts_with(char::is_alphabetic);
            let cjk_after = after
                .trim_start_matches(char::is_whitespace)
                .starts_with(['年', '년']);
            if (spaced && (month_before || day_before)) || month_after || cjk_after {
                let start = floor(s.saturating_sub(64));
                let end = floor((s + 4 + 40).min(b.len()));
                if let Some(d) = parse_long(&text[start..end]) {
                    return Some(d);
                }
            }
        }
        s += r;
    }
    None
}

static AC_COPY: LazyLock<AhoCorasick> = LazyLock::new(|| {
    AhoCorasick::builder()
        .match_kind(aho_corasick::MatchKind::LeftmostFirst)
        .build(["©", "&copy;", "Copyright", "(c)"])
        .unwrap()
});

/// 版权年份（htmldate 的 COPYRIGHT_PATTERN：关键词后最多 99 个非数字字符，然后是年份或年份区间，取后一个年份）。
/// 手写扫描代替带有界重复的正则（自研）；语义和正则的 find_iter 相同。
fn copyright_years(text: &str) -> Vec<&str> {
    let b = text.as_bytes();
    let n = b.len();
    let year_at = |i: usize| -> bool {
        i + 4 <= n
            && b[i..i + 4].iter().all(u8::is_ascii_digit)
            && leading_year(&text[i..i + 4]).is_some()
    };
    let nondigit_at = |i: usize| i < n && !b[i].is_ascii_digit();
    let mut out = Vec::new();
    let mut pos = 0;
    while let Some(m) = AC_COPY.find(&text[pos..]) {
        let kstart = pos + m.start();
        let mut j = pos + m.end();
        let mut skipped = 0;
        while j < n && !b[j].is_ascii_digit() && skipped < 99 {
            j += 1;
            while j < n && !text.is_char_boundary(j) {
                j += 1;
            }
            skipped += 1;
        }
        let mut hit = None;
        if j < n && b[j].is_ascii_digit() && year_at(j) {
            // 先试"年份-年份"，再试"年份年份"，最后单个年份
            let k = if b.get(j + 4) == Some(&b'-') {
                j + 5
            } else {
                j + 4
            };
            if year_at(k) && nondigit_at(k + 4) {
                hit = Some((k, k + 4));
            } else if k != j + 4 && year_at(j + 4) && nondigit_at(j + 8) {
                hit = Some((j + 4, j + 8));
            } else if nondigit_at(j + 4) {
                hit = Some((j, j + 4));
            }
        }
        match hit {
            Some((g, e)) => {
                out.push(&text[g..e]);
                let mut end = e + 1;
                while end < n && !text.is_char_boundary(end) {
                    end += 1;
                }
                pos = end;
            }
            None => {
                let mut next = kstart + 1;
                while next < n && !text.is_char_boundary(next) {
                    next += 1;
                }
                pos = next;
            }
        }
        if pos >= n {
            break;
        }
    }
    out
}

fn search_page(text: &str, b: &Bounds) -> Option<Date> {
    let num = scan_numeric(text);
    let mut copyear = 0;
    let cy = count(&copyright_years(text), |s| s.parse().ok(), b);
    if let Some(y) =
        select_candidate(&cy, |s| s.parse().ok(), b).and_then(|s| s.parse::<i32>().ok())
        && b.year_ok(y)
    {
        copyear = y;
    }
    // 三段式：先看 URL 风格的 /YYYY/MM/DD/，再宽松一些
    for items in [&num.three, &num.three_loose] {
        let occ = count(items, leading_year, b);
        if let Some(s) = select_candidate(&occ, leading_year, b)
            && let Some((y, m, d)) = split3(&s)
            && let Some(d) = after_copyright(Date::ymd(y as i32, m as u32, d as u32), copyear, b)
        {
            return Some(d);
        }
    }
    // DD/MM/YYYY：先换成 YYYY-MM-DD 再挑
    let normalized =
        |occ: Vec<(String, usize)>, f: &dyn Fn(&str) -> Option<String>| -> Vec<(String, usize)> {
            let mut out: Vec<(String, usize)> = Vec::new();
            for (s, n) in occ {
                if let Some(k) = f(&s) {
                    match out.iter_mut().find(|(x, _)| *x == k) {
                        Some(e) => e.1 += n,
                        None => out.push((k, n)),
                    }
                }
            }
            out
        };
    let to_ymd = |s: &str| -> Option<String> {
        let (d, m, y) = split3(s)?;
        Some(format!("{:04}-{:02}-{:02}", full_year(y as i32), m, d))
    };
    let pick_ymd = |occ: Vec<(String, usize)>| -> Option<Date> {
        let occ = normalized(occ, &to_ymd);
        let s = select_candidate(&occ, leading_year, b)?;
        let (y, m, d) = split3(&s)?;
        after_copyright(Date::ymd(y as i32, m as u32, d as u32), copyear, b)
    };
    if let Some(d) = pick_ymd(count(&num.select_ymd, trailing_year, b)) {
        return Some(d);
    }
    let occ = count(&num.datestrings, leading_year, b);
    if let Some(s) = select_candidate(&occ, leading_year, b)
        && let Some(d) = after_copyright(
            Date::ymd(
                s[..4].parse().ok()?,
                s[4..6].parse().ok()?,
                s[6..8].parse().ok()?,
            ),
            copyear,
            b,
        )
    {
        return Some(d);
    }
    if let Some(d) = pick_ymd(count(&num.slashes, trailing_year, b)) {
        return Some(d);
    }
    // 两段式：YYYY-MM，然后 MM-YYYY（都取当月 1 日）
    let occ = count(&num.yyyymm, leading_year, b);
    if let Some(s) = select_candidate(&occ, leading_year, b) {
        let mut it = s.split(['/', '.', '-']);
        let (y, m) = (it.next()?.parse().ok()?, it.next()?.parse().ok()?);
        if let Some(d) = after_copyright(Date::ymd(y, m, 1), copyear, b) {
            return Some(d);
        }
    }
    let occ = count(&num.mmyyyy, trailing_year, b);
    let occ = normalized(occ, &|s: &str| {
        let mut it = s.split(['/', '.', '-']);
        let m: u32 = it.next()?.parse().ok()?;
        let y: i32 = it.next()?.parse().ok()?;
        Some(format!("{y:04}-{m:02}-01"))
    });
    if let Some(s) = select_candidate(&occ, leading_year, b)
        && let Some((y, m, d)) = split3(&s)
        && let Some(d) = after_copyright(Date::ymd(y as i32, m as u32, d as u32), copyear, b)
    {
        return Some(d);
    }
    if let Some(d) = after_copyright(parse_long_scan(text), copyear, b) {
        return Some(d);
    }
    if copyear != 0 {
        return Date::ymd(copyear, 1, 1);
    }
    // 最后：单独的年份（w3.org 的 DTD 地址里的年份不算）
    let occ = count(&num.simple, leading_year, b);
    let s = select_candidate(&occ, leading_year, b)?;
    after_copyright(Date::ymd(s.parse().ok()?, 1, 1), copyear, b)
}

/// 去掉 `<script>`、`<style>`、`<svg>` 的内容和注释后的源码，给后面几步的整页文本模式用：
/// 平均只剩原来的四成，脚本里的版本号、时间戳也不会混进候选（自研）。
fn strip_heavy(source: &str) -> String {
    let b = source.as_bytes();
    let mut out = String::with_capacity(source.len() / 2);
    let mut last = 0;
    let mut i = 0;
    let lower_starts = |at: usize, pat: &[u8]| {
        b.len() >= at + pat.len() && b[at..at + pat.len()].eq_ignore_ascii_case(pat)
    };
    while let Some(off) = memchr_lt(&b[i..]) {
        let at = i + off;
        let close: Option<&[u8]> = if b[at..].starts_with(b"<!--") {
            Some(b"-->")
        } else if lower_starts(at, b"<script") {
            Some(b"</script")
        } else if lower_starts(at, b"<style") {
            Some(b"</style")
        } else if lower_starts(at, b"<svg") {
            Some(b"</svg")
        } else {
            None
        };
        let Some(close) = close else {
            i = at + 1;
            continue;
        };
        out.push_str(&source[last..at]);
        let mut j = at + 4;
        let end = loop {
            match memchr_lt_or(&b[j..], close[0]) {
                Some(o) if lower_starts(j + o, close) => break j + o + close.len(),
                Some(o) => j += o + 1,
                None => break b.len(),
            }
        };
        // 跳过结束标签剩下的部分（`</script >`）
        let end = if close[0] == b'<' {
            b[end..]
                .iter()
                .position(|&c| c == b'>')
                .map_or(b.len(), |p| end + p + 1)
        } else {
            end
        };
        out.push(' ');
        last = end;
        i = end;
    }
    out.push_str(&source[last..]);
    out
}

fn memchr_lt(b: &[u8]) -> Option<usize> {
    memchr::memchr(b'<', b)
}

fn memchr_lt_or(b: &[u8], c: u8) -> Option<usize> {
    memchr::memchr(c, b)
}

/// 先序遍历 `root` 的后代；`visit` 返回 false 时不进入这个节点的子树。
fn walk<'a>(root: NodeR<'a>, mut visit: impl FnMut(&NodeR<'a>) -> bool) {
    let mut cur = root.first_child();
    while let Some(n) = cur {
        if visit(&n)
            && let Some(c) = n.first_child()
        {
            cur = Some(c);
            continue;
        }
        let mut x = n;
        cur = loop {
            if let Some(s) = x.next_sibling() {
                break Some(s);
            }
            match x.parent() {
                Some(p) if p.id() != root.id() => x = p,
                _ => break None,
            }
        };
    }
}

/// URL、meta、JSON-LD 这几步用到的元素：一遍轻量遍历就能拿到，大多数页面到这里就有结果。
#[derive(Default)]
struct Head<'a> {
    canonical: Option<String>,
    og_image: Option<String>,
    metas: Vec<NodeR<'a>>,
    scripts: Vec<NodeR<'a>>,
}

fn collect_head<'a>(html: &'a Html<'a>) -> Head<'a> {
    let mut h = Head::default();
    walk(html.root(), |node| {
        let Some(el) = element(node) else { return true };
        match el.name() {
            "link" if h.canonical.is_none() => {
                if el.attr("rel").is_some_and(|r| {
                    r.split_whitespace()
                        .any(|x| x.eq_ignore_ascii_case("canonical"))
                }) {
                    h.canonical = el.attr("href").map(str::to_string);
                }
            }
            "meta" => {
                if h.og_image.is_none() && el.attr("property") == Some("og:image") {
                    h.og_image = el.attr("content").map(str::to_string);
                }
                h.metas.push(*node);
            }
            "script" => {
                h.scripts.push(*node);
                return false;
            }
            "style" => return false,
            _ => {}
        }
        true
    });
    h
}

/// 后面几步用到的元素，只在前几步都没有结果时才收集。被清理的元素（svg、iframe…）整棵子树跳过。
#[derive(Default)]
struct Body<'a> {
    abbrs: Vec<NodeR<'a>>,
    candidates: Vec<NodeR<'a>>,
    title_h1: Vec<NodeR<'a>>,
    times: Vec<NodeR<'a>>,
    free_text: Vec<&'a str>,
}

fn collect_body<'a>(html: &'a Html<'a>) -> Body<'a> {
    let mut b = Body::default();
    walk(html.root(), |node| {
        if let Some(t) = node.as_text() {
            if node.parent().and_then(|p| p.tag()).is_some_and(|t| {
                matches!(
                    t,
                    Tag::Div
                        | Tag::H2
                        | Tag::H3
                        | Tag::H4
                        | Tag::Li
                        | Tag::P
                        | Tag::Span
                        | Tag::Time
                        | Tag::Ul
                )
            }) {
                let t = t.trim();
                if t.len() > MIN_SEGMENT && t.len() < MAX_SEGMENT * 4 {
                    let n = t.chars().count();
                    if n > MIN_SEGMENT && n < MAX_SEGMENT {
                        b.free_text.push(t);
                    }
                }
            }
            return false;
        }
        let Some(el) = element(node) else { return true };
        let tag = el.tag();
        if pruned(tag, &el) {
            return false;
        }
        if (matches!(tag, Tag::Footer | Tag::Small) || is_date_candidate(&el))
            && !is_banner(node, &el)
        {
            b.candidates.push(*node);
        }
        match tag {
            Tag::Title | Tag::H1 => b.title_h1.push(*node),
            Tag::Time => b.times.push(*node),
            Tag::Abbr => b.abbrs.push(*node),
            _ => {}
        }
        true
    });
    b
}

fn run(
    html: &Html<'_>,
    source: &str,
    url: Option<&str>,
    trace: Option<&mut Vec<(&'static str, Option<String>, u64)>>,
) -> Option<Date> {
    let b = Bounds::now();
    let collect_start = std::time::Instant::now();
    let head = collect_head(html);
    let body = std::cell::OnceCell::new();
    let body = || body.get_or_init(|| collect_body(html));
    let collect_ns = collect_start.elapsed().as_nanos() as u64;
    let url = url.map(str::to_string).or_else(|| head.canonical.clone());
    let lite = std::cell::OnceCell::new();
    let lite = || lite.get_or_init(|| strip_heavy(source)).as_str();
    let free = || {
        let mut reference = None;
        for seg in &body().free_text {
            if let Some(d) = date_expr(seg, &b) {
                reference = earliest(reference, d);
            }
        }
        b.check(reference)
    };
    let stages: [(&'static str, &dyn Fn() -> Option<Date>); 12] = [
        ("url", &|| url.as_deref().and_then(|u| url_date(u, &b))),
        ("meta", &|| {
            prefer_local(examine_header(&head.metas, &b), || {
                json_search(&head.scripts, &b)
            })
        }),
        ("json", &|| json_search(&head.scripts, &b)),
        ("abbr", &|| examine_abbr(&body().abbrs, &b)),
        ("elements", &|| examine_elements(&body().candidates, &b)),
        ("title", &|| examine_elements(&body().title_h1, &b)),
        ("time", &|| examine_time(&body().times, &b)),
        ("timestamp", &|| first_timestamp(source, &b)),
        ("og:image", &|| {
            head.og_image.as_deref().and_then(|u| url_date(u, &b))
        }),
        ("phrases", &|| idiosyncrasies(lite(), &b)),
        ("free text", &free),
        ("page", &|| search_page(lite(), &b)),
    ];
    match trace {
        None => stages.iter().find_map(|(_, f)| f()),
        Some(out) => {
            out.push(("collect", None, collect_ns));
            for (name, f) in &stages {
                let t = std::time::Instant::now();
                let d = f().map(|d| d.render());
                out.push((name, d, t.elapsed().as_nanos() as u64));
            }
            None
        }
    }
}

/// 页面的原始发布日期。`source` 是原始 HTML，`url` 是页面地址（没有时用 canonical 链接）。
pub(crate) fn published(html: &Html<'_>, source: &str, url: Option<&str>) -> Option<Date> {
    run(html, source, url, None)
}

/// 调试用：按真实流程（找到就停）找发布日期的耗时（纳秒），不含解析 HTML。
#[doc(hidden)]
pub fn time_published(source: &str, url: Option<&str>) -> (Option<String>, u64) {
    let html = Html::parse(source);
    let t = std::time::Instant::now();
    let d = published(&html, source, url).map(|d| d.render());
    (d, t.elapsed().as_nanos() as u64)
}

/// 调试用：每一步各自找到的日期和耗时（纳秒），不会在第一个结果处停下。第一项是收集元素的耗时。
#[doc(hidden)]
pub fn trace_published(
    source: &str,
    url: Option<&str>,
) -> Vec<(&'static str, Option<String>, u64)> {
    let html = Html::parse(source);
    let mut out = Vec::new();
    run(&html, source, url, Some(&mut out));
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    fn b() -> Bounds {
        Bounds {
            min: (1995, 1, 1),
            max: (2026, 10, 21),
        }
    }

    fn ex(s: &str) -> Option<String> {
        date_expr(s, &b()).map(|d| d.render())
    }

    #[test]
    fn parses_expressions() {
        assert_eq!(
            ex("2020-06-07T08:30:00+02:00").as_deref(),
            Some("2020-06-07T08:30:00+02:00")
        );
        assert_eq!(ex("2020-06-07").as_deref(), Some("2020-06-07"));
        assert_eq!(ex("20200607").as_deref(), Some("2020-06-07"));
        assert_eq!(ex("Posted 07.06.2020").as_deref(), Some("2020-06-07"));
        assert_eq!(ex("June 7th, 2020").as_deref(), Some("2020-06-07"));
        assert_eq!(ex("7. Juni 2020").as_deref(), Some("2020-06-07"));
        assert_eq!(ex("7 de junio de 2020").as_deref(), Some("2020-06-07"));
        assert_eq!(ex("7 червня 2020").as_deref(), Some("2020-06-07"));
        assert_eq!(ex("7 июня 2020 г.").as_deref(), Some("2020-06-07"));
        assert_eq!(ex("2020年6月7日").as_deref(), Some("2020-06-07"));
        assert_eq!(ex("2020. június 7.").as_deref(), Some("2020-06-07"));
        assert_eq!(
            ex("الأربعاء، 07 أكتوبر 2020 - 12:09 م").as_deref(),
            Some("2020-10-07")
        );
        assert_eq!(ex("26 | 01 | 2022").as_deref(), Some("2022-01-26"));
        // 固定说法（预筛之后照样认得）
        let bb = b();
        assert_eq!(
            idiosyncrasies("<p>Posted on 07/06/2020 by x</p>", &bb)
                .map(|d| d.render())
                .as_deref(),
            Some("2020-06-07")
        );
        assert_eq!(
            idiosyncrasies("<p>Veröffentlicht am 7.6.2020</p>", &bb)
                .map(|d| d.render())
                .as_deref(),
            Some("2020-06-07")
        );
        assert_eq!(
            idiosyncrasies("<p>Stand: 01.02.2021</p>", &bb)
                .map(|d| d.render())
                .as_deref(),
            Some("2021-02-01")
        );
        assert_eq!(
            idiosyncrasies("<p>on 2020/06/07</p>", &bb)
                .map(|d| d.render())
                .as_deref(),
            Some("2020-06-07")
        );
        assert_eq!(idiosyncrasies("<p>build 1.2.2020</p>", &bb), None);
        assert_eq!(
            ex("Tue, 09 Jun 2020 08:00:00 GMT").as_deref(),
            Some("2020-06-09T08:00:00Z")
        );
        // 时刻、价格、电话、超出范围的都不是日期
        assert_eq!(ex("12:30"), None);
        assert_eq!(ex("$2020.06"), None);
        assert_eq!(ex("1985-06-07"), None);
        assert_eq!(ex("2030-01-01"), None);
    }

    /// 单遍扫描器和原来的正则在随机文本上逐类给出完全相同的匹配。
    #[test]
    fn numeric_scanner_matches_the_regexes() {
        let alphabet: Vec<char> = "0123456789012345678919202/.-/.- ax©é".chars().collect();
        let words = [
            "©",
            "(c)",
            "Copyright",
            "&copy;",
            "2019",
            "2020-2021",
            "1999",
            " ",
            "-",
        ];
        let mut seed: u64 = 0x2545_f491_4f6c_dd1d;
        let mut next = || {
            seed ^= seed << 13;
            seed ^= seed >> 7;
            seed ^= seed << 17;
            seed
        };
        // 参照实现：Python 正则的 find_iter；`w3` 时模拟 `(?<!w3.org)` 反向预查（那个位置匹配失败，从下一个位置接着找）
        let regex_all = |re: &Regex, t: &str, w3: bool| -> Vec<String> {
            let mut out = Vec::new();
            let mut pos = 0;
            while pos <= t.len() {
                let Some(c) = re.captures_at(t, pos) else {
                    break;
                };
                let m = c.get(0).unwrap();
                if w3 && t.as_bytes()[..m.start()].ends_with(b"w3.org") {
                    pos = m.start() + t[m.start()..].chars().next().map_or(1, char::len_utf8);
                    continue;
                }
                out.push(c[1].to_string());
                pos = if m.end() > m.start() {
                    m.end()
                } else {
                    m.end() + 1
                };
            }
            out
        };
        for _ in 0..20_000 {
            let len = (next() % 40) as usize;
            let mut t = String::new();
            for _ in 0..len {
                if next() % 5 == 0 {
                    t.push_str(words[(next() % words.len() as u64) as usize]);
                } else {
                    t.push(alphabet[(next() % alphabet.len() as u64) as usize]);
                }
            }
            if next() % 7 == 0 {
                t.insert_str(0, "w3.org/");
            }
            let n = scan_numeric(&t);
            let own = |v: &Vec<&str>| v.iter().map(|x| x.to_string()).collect::<Vec<_>>();
            assert_eq!(own(&n.three), regex_all(&THREE, &t, false), "three {t:?}");
            assert_eq!(
                own(&n.three_loose),
                regex_all(&THREE_LOOSE, &t, false),
                "three_loose {t:?}"
            );
            assert_eq!(
                own(&n.select_ymd),
                regex_all(&SELECT_YMD, &t, false),
                "select_ymd {t:?}"
            );
            assert_eq!(
                own(&n.datestrings),
                regex_all(&DATESTRINGS, &t, false),
                "datestrings {t:?}"
            );
            assert_eq!(
                own(&n.slashes),
                regex_all(&SLASHES, &t, false),
                "slashes {t:?}"
            );
            assert_eq!(
                own(&n.yyyymm),
                regex_all(&YYYYMM, &t, false),
                "yyyymm {t:?}"
            );
            assert_eq!(
                own(&n.mmyyyy),
                regex_all(&MMYYYY, &t, false),
                "mmyyyy {t:?}"
            );
            assert_eq!(own(&n.simple), regex_all(&SIMPLE, &t, true), "simple {t:?}");
            let c: Vec<String> = copyright_years(&t).iter().map(|x| x.to_string()).collect();
            assert_eq!(c, regex_all(&COPYRIGHT, &t, false), "copyright {t:?}");
        }
    }

    #[test]
    fn strips_scripts_styles_and_comments() {
        let s = "<p>a</p><script type=x>var d='2020-01-01';</script ><!-- 2019 --><STYLE>b{}</STYLE><svg><text>1999</text></svg><p>é</p><script>unterminated";
        assert_eq!(strip_heavy(s), "<p>a</p>    <p>é</p> ");
    }

    #[test]
    fn normalizes_structured_values_keeping_the_offset() {
        assert_eq!(
            normalize("2020-02-15T07:00:00+09:00").as_deref(),
            Some("2020-02-15T07:00:00+09:00")
        );
        assert_eq!(
            normalize("2020-04-02T04:11:00+0000").as_deref(),
            Some("2020-04-02T04:11:00Z")
        );
        assert_eq!(normalize("2020-04-02").as_deref(), Some("2020-04-02"));
        assert_eq!(
            normalize("2020-04-02T04:11").as_deref(),
            Some("2020-04-02T04:11:00")
        );
        assert_eq!(normalize("not a date"), None);
    }

    fn page_date(html: &str, url: Option<&str>) -> Option<String> {
        let dom = Html::parse(html);
        published(&dom, html, url).map(|d| d.render())
    }

    #[test]
    fn follows_the_htmldate_order() {
        // URL 里不带分隔符的日期
        assert_eq!(
            page_date(
                "<html></html>",
                Some("https://www.thelocal.se/20200428/meet")
            )
            .as_deref(),
            Some("2020-04-28")
        );
        assert_eq!(
            page_date("<html></html>", Some("https://a.com/item/2020042812/x")),
            None
        );
        // URL 优先
        assert_eq!(
            page_date(
                "<html><body>x</body></html>",
                Some("https://a.com/2020/06/07/story")
            )
            .as_deref(),
            Some("2020-06-07")
        );
        // meta：发布日期优先于修改日期
        let h = r#"<html><head><meta property="article:modified_time" content="2021-01-01T00:00:00Z"><meta property="article:published_time" content="2020-06-07T08:00:00+02:00"></head></html>"#;
        assert_eq!(
            page_date(h, None).as_deref(),
            Some("2020-06-07T08:00:00+02:00")
        );
        // 只有修改日期时退而求其次
        let h = r#"<html><head><meta property="article:modified_time" content="2021-01-01"></head></html>"#;
        assert_eq!(page_date(h, None).as_deref(), Some("2021-01-01"));
        // 带日期类名的元素
        let h = r#"<html><body><p>Intro text</p><div class="post-date">Published on 3 March 2019</div></body></html>"#;
        assert_eq!(page_date(h, None).as_deref(), Some("2019-03-03"));
        // 页头的"今日日期"不算
        let h = r#"<html><body><div><span class="date-edition"><span>lun. 5 août 2019</span></span></div><ul class="info"><li>27 juin 2019 Par Elba</li></ul></body></html>"#;
        assert_eq!(page_date(h, None).as_deref(), Some("2019-06-27"));
        // meta 是 UTC、JSON-LD 是同一时刻的本地时间：取本地的
        let h = r#"<html><head><meta property="article:published_time" content="2020-12-24T23:59:50+00:00"><script type="application/ld+json">{"datePublished":"2020-12-25T01:59:50+02:00"}</script></head></html>"#;
        assert_eq!(
            page_date(h, None).as_deref(),
            Some("2020-12-25T01:59:50+02:00")
        );
        // 多个 <time> 取最早
        let h = r#"<html><body><time datetime="2019-05-01">a</time><time datetime="2018-04-01">b</time></body></html>"#;
        assert_eq!(page_date(h, None).as_deref(), Some("2018-04-01"));
        // 整页统计：版权年份之前的日期不要
        let h = "<html><body><p>Lorem ipsum dolor sit amet, a long paragraph mentioning 2003/01/02 in passing.</p><p>Another long paragraph with enough words to not be a short segment. © 2015 Example</p></body></html>";
        assert_eq!(page_date(h, None).as_deref(), Some("2015-01-01"));
        // 多字节字符紧挨着年份也不会切坏字符串
        let h = "<html><body><p>这是一个很长很长的段落，里面没有任何像样的日期信息，只有很多很多的文字内容。2019年。</p></body></html>";
        assert_eq!(page_date(h, None).as_deref(), Some("2019-01-01"));
    }
}
