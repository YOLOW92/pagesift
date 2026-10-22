use std::collections::HashMap;
use std::sync::LazyLock;

use aho_corasick::AhoCorasick;

use super::{Bounds, Date, MAX_CANDIDATES, full_year, parse_long};

// ---- 整页模式统计（htmldate 的 search_page）

/// 按出现次数挑候选（htmldate 的 select_candidate，原始发布日期模式）。
/// `items` 是匹配到的字符串（按首次出现顺序）和次数；返回选中的字符串。
pub(super) fn select_candidate(
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

pub(super) fn leading_year(s: &str) -> Option<i32> {
    let t = s.strip_prefix(|c: char| !c.is_ascii_digit()).unwrap_or(s);
    let y: i32 = t.get(..4)?.parse().ok()?;
    ((1990..=2039).contains(&y)).then_some(y)
}

pub(super) fn trailing_year(s: &str) -> Option<i32> {
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

pub(super) fn split3(s: &str) -> Option<(i64, i64, i64)> {
    let mut it = s.split(['/', '.', '-']).map(|p| p.parse::<i64>().ok());
    Some((it.next()??, it.next()??, it.next()??))
}

/// 版权年份之前的日期不可信（filter_ymd_candidate）。
pub(super) fn after_copyright(d: Option<Date>, copyear: i32, b: &Bounds) -> Option<Date> {
    b.check(d).filter(|d| copyear == 0 || d.y >= copyear)
}

/// 整页里各类数字日期写法的所有匹配：一遍扫过所有数字串的起点，同时判断八类写法（自研，代替八个正则各扫一遍）。
/// 每类单独记录上一次匹配的结束位置，语义和对应正则（`\D(…)\D` 这类）的 find_iter 一致：
/// 匹配互不重叠，前后的分隔字符会被吃掉。
#[derive(Default)]
pub(super) struct Numeric<'t> {
    /// `/YYYY/MM/DD[01/]`
    pub(super) three: Vec<&'t str>,
    /// `YYYY-MM-DD`（分隔符 `/.-`）
    pub(super) three_loose: Vec<&'t str>,
    /// `DD-MM-YYYY`
    pub(super) select_ymd: Vec<&'t str>,
    /// `YYYYMMDD`
    pub(super) datestrings: Vec<&'t str>,
    /// `DD/MM/YY`、`DD.MM.YY`
    pub(super) slashes: Vec<&'t str>,
    /// `YYYY-MM`
    pub(super) yyyymm: Vec<&'t str>,
    /// `MM-YYYY`
    pub(super) mmyyyy: Vec<&'t str>,
    /// 单独的年份
    pub(super) simple: Vec<&'t str>,
}

pub(super) fn scan_numeric(text: &str) -> Numeric<'_> {
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
pub(super) fn count(
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
pub(super) fn parse_long_scan(text: &str) -> Option<Date> {
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

pub(super) static AC_COPY: LazyLock<AhoCorasick> = LazyLock::new(|| {
    AhoCorasick::builder()
        .match_kind(aho_corasick::MatchKind::LeftmostFirst)
        .build(["©", "&copy;", "Copyright", "(c)"])
        .unwrap()
});

/// 版权年份（htmldate 的 COPYRIGHT_PATTERN：关键词后最多 99 个非数字字符，然后是年份或年份区间，取后一个年份）。
/// 手写扫描代替带有界重复的正则（自研）；语义和正则的 find_iter 相同。
pub(super) fn copyright_years(text: &str) -> Vec<&str> {
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

pub(super) fn search_page(text: &str, b: &Bounds) -> Option<Date> {
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
