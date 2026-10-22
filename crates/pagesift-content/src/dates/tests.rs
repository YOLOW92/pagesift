use std::sync::LazyLock;

use regex::Regex;

use super::*;

macro_rules! re {
    ($name:ident, $pat:expr) => {
        static $name: LazyLock<Regex> =
            LazyLock::new(|| Regex::new(AsRef::<str>::as_ref(&$pat)).unwrap());
    };
}

re!(
    COPYRIGHT,
    format!(r"(?:©|&copy;|Copyright|\(c\))\D{{0,99}}(?:{YEAR})?-?({YEAR})\D")
);
re!(THREE, r"/([0-9]{4}/[0-9]{2}/[0-9]{2})[01/]");
re!(THREE_LOOSE, r"\D([0-9]{4}[/.-][0-9]{2}[/.-][0-9]{2})\D");
re!(
    SELECT_YMD,
    r"\D([0-3]?[0-9][/.-][0-1]?[0-9][/.-][0-9]{4})\D"
);
re!(
    DATESTRINGS,
    r"\D((?:19[0-9]{2}|20[0-9]{2})[01][0-9][0-3][0-9])\D"
);
re!(
    SLASHES,
    r"\D([0-3]?[0-9]/[0-1]?[0-9]/[0129][0-9]|[0-3][0-9]\.[01][0-9]\.[0129][0-9])\D"
);
re!(YYYYMM, r"\D([12][0-9]{3}[/.-](?:1[0-2]|0[1-9]))\D");
re!(MMYYYY, r"\D([0-1]?[0-9][/.-][12][0-9]{3})\D");
re!(SIMPLE, format!(r"\D({YEAR})\D"));

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
    let h =
        r#"<html><head><meta property="article:modified_time" content="2021-01-01"></head></html>"#;
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
