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

use std::time::{SystemTime, UNIX_EPOCH};

use pagesift_html::{Html, Tag};

use crate::dom::{NodeR, element};

mod dom;
mod page;
mod parse;
#[cfg(test)]
mod tests;
#[cfg(test)]
mod tests_parse;
#[cfg(test)]
mod tests_text;

use dom::*;
use page::*;
pub(crate) use parse::normalize;
use parse::*;

/// 一个日期，可能带时刻和时区偏移（秒）。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) struct Date {
    pub y: i32,
    pub m: u32,
    pub d: u32,
    pub time: Option<(u32, u32, u32, Option<i32>)>,
}

impl Date {
    pub(super) fn ymd(y: i32, m: u32, d: u32) -> Option<Date> {
        valid_ymd(y, m, d).then_some(Date {
            y,
            m,
            d,
            time: None,
        })
    }

    /// 墙上时间，用来比较先后（同 htmldate：按页面上写的时刻比，不换算时区）。
    pub(super) fn key(&self) -> (i32, u32, u32, u32, u32, u32) {
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

    pub(super) fn ok(&self, d: &Date) -> bool {
        let k = (d.y, d.m, d.d);
        self.min <= k && k <= self.max
    }

    pub(super) fn year_ok(&self, y: i32) -> bool {
        self.min.0 <= y && y <= self.max.0
    }

    pub(super) fn check(&self, d: Option<Date>) -> Option<Date> {
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
pub(crate) fn walk<'a>(root: NodeR<'a>, mut visit: impl FnMut(&NodeR<'a>) -> bool) {
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
            "style" | "svg" | "p" | "table" | "ul" | "ol" | "pre" | "code" => return false,
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
    let head = std::cell::OnceCell::new();
    let head = || head.get_or_init(|| collect_head(html));
    let body = std::cell::OnceCell::new();
    let body = || body.get_or_init(|| collect_body(html));
    let collect_ns = collect_start.elapsed().as_nanos() as u64;
    let effective_url = || url.map(str::to_string).or_else(|| head().canonical.clone());
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
        ("url", &|| {
            effective_url().as_deref().and_then(|u| url_date(u, &b))
        }),
        ("meta", &|| {
            prefer_local(examine_header(&head().metas, &b), || {
                json_search(&head().scripts, &b)
            })
        }),
        ("json", &|| json_search(&head().scripts, &b)),
        ("abbr", &|| examine_abbr(&body().abbrs, &b)),
        ("elements", &|| examine_elements(&body().candidates, &b)),
        ("title", &|| {
            examine_elements(&body().title_h1, &b).or_else(|| {
                body()
                    .title_h1
                    .iter()
                    .find_map(|n| b.check(parse_long(&n.text())))
            })
        }),
        ("time", &|| examine_time(&body().times, &b)),
        ("timestamp", &|| first_timestamp(source, &b)),
        ("og:image", &|| {
            head().og_image.as_deref().and_then(|u| url_date(u, &b))
        }),
        ("phrases", &|| idiosyncrasies(lite(), &b)),
        ("free text", &free),
        ("page", &|| {
            search_page(lite(), &b).or_else(|| search_page(source, &b))
        }),
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
