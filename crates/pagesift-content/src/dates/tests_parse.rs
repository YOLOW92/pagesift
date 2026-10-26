//! 日期解析的表驱动测试：每个解析函数一张用例表，一次跑完全部用例，最后统一报告失败的条目。
//!
//! 期望值按代码实际支持的写法填写；标 `// BUG?` 的是"当前行为看起来不对"的用例，按当前行为写，
//! 留待以后决定要不要改生产代码。

use std::fmt::Debug;

use super::*;

/// 跑一张表：`f` 的结果和期望逐条比较，收集所有失败再统一 panic（打印序号、输入、期望、实际）。
pub(super) fn check<I: Debug, E: Debug, A: Debug>(
    name: &str,
    cases: &[(I, E)],
    f: impl Fn(&I) -> A,
    eq: impl Fn(&A, &E) -> bool,
) {
    let mut failures = Vec::new();
    for (i, (input, want)) in cases.iter().enumerate() {
        let got = f(input);
        if !eq(&got, want) {
            failures.push(format!(
                "  #{i} 输入 = {input:?}\n      期望 = {want:?}\n      实际 = {got:?}"
            ));
        }
    }
    assert!(
        failures.is_empty(),
        "{name}：{} / {} 条用例失败\n{}",
        failures.len(),
        cases.len(),
        failures.join("\n")
    );
}

/// 结果是 `Option<String>`、期望是 `Option<&str>` 的常见情形。
pub(super) fn check_opt<I: Debug>(
    name: &str,
    cases: &[(I, Option<&str>)],
    f: impl Fn(&I) -> Option<String>,
) {
    check(name, cases, f, |a, e| a.as_deref() == *e);
}

/// 固定的合理范围（1995-01-01 到 2026-10-21），不随运行日期变化。
pub(super) fn bounds() -> Bounds {
    Bounds {
        min: (1995, 1, 1),
        max: (2026, 10, 21),
    }
}

fn render(d: Option<Date>) -> Option<String> {
    d.map(|d| d.render())
}

// ---------------------------------------------------------------------------
// 基础：日历合法性、两位年份、日月互换

#[test]
fn table_valid_ymd() {
    const CASES: &[((i32, u32, u32), bool)] = &[
        ((2024, 2, 29), true),  // 闰年
        ((2023, 2, 29), false), // 平年
        ((2000, 2, 29), true),  // 能被 400 整除
        ((1900, 2, 29), false), // 能被 100 整除但不能被 400 整除
        ((2024, 2, 30), false),
        ((2024, 4, 30), true),
        ((2024, 4, 31), false),
        ((2024, 12, 31), true),
        ((2024, 1, 31), true),
        ((2024, 13, 1), false),
        ((2024, 0, 1), false),
        ((2024, 1, 0), false),
    ];
    check(
        "valid_ymd",
        CASES,
        |&(y, m, d)| valid_ymd(y, m, d),
        |a, e| a == e,
    );
}

#[test]
fn table_full_year() {
    const CASES: &[(i32, i32)] = &[
        (0, 2000),
        (24, 2024),
        (89, 2089), // 同 htmldate 的 correct_year：90 以下都算 20xx
        (90, 1990),
        (99, 1999),
        (100, 100),
        (2024, 2024),
    ];
    check("full_year", CASES, |&y| full_year(y), |a, e| a == e);
}

#[allow(clippy::type_complexity)] // 表驱动测试：一行就是一条用例，拆成 type 定义反而难读
#[test]
fn table_dmy() {
    const CASES: &[((u32, u32, i32), Option<&str>)] = &[
        ((5, 3, 2024), Some("2024-03-05")),
        ((13, 5, 2024), Some("2024-05-13")),
        ((5, 13, 2024), Some("2024-05-13")), // 月份 > 12、日 ≤ 12：日月互换
        ((12, 25, 2024), Some("2024-12-25")),
        ((13, 13, 2024), None),
        ((31, 2, 2024), None),
        ((5, 3, 24), Some("2024-03-05")), // 两位年份
        ((5, 3, 98), Some("1998-03-05")),
    ];
    check_opt("dmy", CASES, |&(d, m, y)| render(dmy(d, m, y)));
}

// ---------------------------------------------------------------------------
// 月份名

#[test]
fn table_month_of() {
    const CASES: &[((&str, bool), Option<u32>)] = &[
        // 英文全称、缩写，大小写不敏感
        (("January", false), Some(1)),
        (("jan", false), Some(1)),
        (("MARCH", false), Some(3)),
        (("Sept", false), Some(9)),
        (("sep", false), Some(9)),
        (("May", false), Some(5)),
        (("December", false), Some(12)),
        // 德语
        (("März", false), Some(3)),
        (("MÄRZ", false), Some(3)),
        (("Maerz", false), Some(3)),
        (("Juni", false), Some(6)),
        (("Dezember", false), Some(12)),
        (("Jänner", false), Some(1)),
        // 法语
        (("janvier", false), Some(1)),
        (("févr", false), Some(2)),
        (("fevrier", false), Some(2)),
        (("août", false), Some(8)),
        (("décembre", false), Some(12)),
        // 西、葡、意、荷、印尼
        (("marzo", false), Some(3)),
        (("março", false), Some(3)),
        (("setembro", false), Some(9)),
        (("maggio", false), Some(5)),
        (("maart", false), Some(3)),
        (("mei", false), Some(5)),
        (("Agustus", false), Some(8)),
        // 土耳其语（ı/İ 折成 i）
        (("Şubat", false), Some(2)),
        (("Ağustos", false), Some(8)),
        (("EYLÜL", false), Some(9)),
        (("KASIM", false), Some(11)),
        (("Aralık", false), Some(12)),
        (("Mayıs", false), Some(5)),
        // 俄、乌、波兰、捷克的变格
        (("марта", false), Some(3)),
        (("июня", false), Some(6)),
        (("березня", false), Some(3)),
        (("marca", false), Some(3)),
        (("października", false), Some(10)),
        (("března", false), Some(3)),
        // 罗马尼亚、芬兰、匈牙利、希腊、阿拉伯、波斯
        (("martie", false), Some(3)),
        (("maaliskuuta", false), Some(3)),
        (("március", false), Some(3)),
        (("Μαρτίου", false), Some(3)),
        (("مارس", false), Some(3)),
        (("ژوئن", false), Some(6)),
        // 粘在前一个词上：只有 glued 时找最长的已知后缀
        (("SmithMarch", true), Some(3)),
        (("SmithMarch", false), None),
        (("byJune", true), Some(6)),
        // 不是月份
        (("foo", false), None),
        (("Monday", false), None),
        (("Chapter", true), None),
        (("ma", false), None),
    ];
    check(
        "month_of",
        CASES,
        |&(w, glued)| month_of(w, glued),
        |a, e| a == e,
    );
}

// ---------------------------------------------------------------------------
// ISO 8601 / RFC 3339

#[test]
fn table_parse_iso() {
    // ((输入, strict), 期望)
    const CASES: &[((&str, bool), Option<&str>)] = &[
        // 只有日期，三种分隔符，月日可省前导零
        (("2024-03-05", true), Some("2024-03-05")),
        (("2024/03/05", true), Some("2024-03-05")),
        (("2024.03.05", true), Some("2024-03-05")),
        (("2024-3-5", true), Some("2024-03-05")),
        (("2024-03-05  ", true), Some("2024-03-05")), // 结尾空格允许
        // 带时刻，不带时区
        (("2024-03-05T08:30", true), Some("2024-03-05T08:30:00")),
        (("2024-03-05T08:30:15", true), Some("2024-03-05T08:30:15")),
        (("2024-03-05 08:30:15", true), Some("2024-03-05T08:30:15")),
        (("2024-03-05t08:30:15", true), Some("2024-03-05T08:30:15")),
        // Z、UTC、GMT
        (("2024-03-05T08:30:15Z", true), Some("2024-03-05T08:30:15Z")),
        (("2024-03-05T08:30:15z", true), Some("2024-03-05T08:30:15Z")),
        (
            ("2024-03-05 08:30:15 UTC", true),
            Some("2024-03-05T08:30:15Z"),
        ),
        (
            ("2024-03-05T08:30:15GMT", true),
            Some("2024-03-05T08:30:15Z"),
        ),
        // 偏移：+08:00、+0800、+08、负偏移、半小时、零偏移写成 Z
        (
            ("2024-03-05T08:30:15+08:00", true),
            Some("2024-03-05T08:30:15+08:00"),
        ),
        (
            ("2024-03-05T08:30:15+0800", true),
            Some("2024-03-05T08:30:15+08:00"),
        ),
        (
            ("2024-03-05T08:30:15+08", true),
            Some("2024-03-05T08:30:15+08:00"),
        ),
        (
            ("2024-03-05T08:30:15-05:00", true),
            Some("2024-03-05T08:30:15-05:00"),
        ),
        (
            ("2024-03-05T08:30:15+05:30", true),
            Some("2024-03-05T08:30:15+05:30"),
        ),
        (
            ("2024-03-05T08:30:15+00:00", true),
            Some("2024-03-05T08:30:15Z"),
        ),
        (
            ("2024-03-05T08:30:15 +08:00", true),
            Some("2024-03-05T08:30:15+08:00"),
        ),
        (
            ("2024-03-05T08:30+08:00", true),
            Some("2024-03-05T08:30:00+08:00"),
        ), // 没有秒也接时区
        // 毫秒、微秒、纳秒（小数秒丢弃），逗号小数
        (
            ("2024-03-05T08:30:15.123Z", true),
            Some("2024-03-05T08:30:15Z"),
        ),
        (
            ("2024-03-05T08:30:15.123456+08:00", true),
            Some("2024-03-05T08:30:15+08:00"),
        ),
        (
            ("2024-03-05T08:30:15.123456789Z", true),
            Some("2024-03-05T08:30:15Z"),
        ),
        (
            ("2024-03-05T08:30:15,5Z", true),
            Some("2024-03-05T08:30:15Z"),
        ),
        // 闰秒记成 59
        (("2024-03-05T23:59:60Z", true), Some("2024-03-05T23:59:59Z")),
        // 时刻不合法：时刻部分整体不要；strict 时剩下的内容使整串失败，非 strict 时只取日期
        (("2024-03-05T24:00:00Z", true), None),
        (("2024-03-05T24:00:00Z", false), Some("2024-03-05")),
        (("2024-03-05T08:61:00Z", true), None),
        (("2024-03-05T08:30:15+15:00", true), None), // 偏移超过 14 小时
        (("2024-03-05T08:30:15+15:00", false), Some("2024-03-05")),
        (("2024-03-05T0830", true), None),
        (("2024-03-05T0830", false), Some("2024-03-05")),
        // 后面有别的内容：strict 失败，非 strict 只取日期
        (("2024-03-05 garbage", true), None),
        (("2024-03-05 garbage", false), Some("2024-03-05")),
        (
            ("2024-03-05T08:30:15Z tail", false),
            Some("2024-03-05T08:30:15Z"),
        ),
        // 日历上不存在的日期
        (("2024-02-29", true), Some("2024-02-29")),
        (("2023-02-29", true), None),
        (("2024-02-30", true), None),
        (("2024-04-31", true), None),
        (("2024-13-01", true), None),
        (("2024-00-10", true), None),
        (("2024-01-00", true), None),
        // 格式不对
        (("2024-03/05", true), None), // 分隔符前后不一致
        (("20240305", true), None),   // 不带分隔符的交给 custom_parse
        (("2024-03-051", true), None),
        (("2024-03-051", false), None), // 日后面紧跟数字也不要
        (("24-03-05", true), None),     // 年必须四位
        ((" 2024-03-05", true), None),  // 开头空格由 normalize 先 trim
        (("2024_03_05", true), None),
        (("", true), None),
        (("2024", true), None),
        (("2024-03", true), None),
        // 年份本身不限制范围（范围由 normalize / Bounds 判断）
        (("0999-12-31", true), Some("0999-12-31")),
    ];
    check_opt("parse_iso", CASES, |&(s, strict)| {
        render(parse_iso(s, strict))
    });
}

// ---------------------------------------------------------------------------
// RFC 2822 / RFC 1123（HTTP 头、RSS pubDate）

#[test]
fn table_parse_rfc2822() {
    const CASES: &[(&str, Option<&str>)] = &[
        (
            "Tue, 05 Mar 2024 08:00:00 GMT",
            Some("2024-03-05T08:00:00Z"),
        ), // RFC 1123
        ("Tue, 05 Mar 2024 08:00:00 UT", Some("2024-03-05T08:00:00Z")),
        (
            "Tue, 05 Mar 2024 08:00:00 UTC",
            Some("2024-03-05T08:00:00Z"),
        ),
        ("Tue, 05 Mar 2024 08:00:00 Z", Some("2024-03-05T08:00:00Z")),
        (
            "Tue, 5 Mar 2024 08:00:00 +0800",
            Some("2024-03-05T08:00:00+08:00"),
        ),
        (
            "Tue, 05 Mar 2024 08:00:00 +0530",
            Some("2024-03-05T08:00:00+05:30"),
        ),
        (
            "Tue, 05 Mar 2024 08:00:00 -0500",
            Some("2024-03-05T08:00:00-05:00"),
        ),
        (
            "Fri, 23 Oct 2026 04:00:00 -0000",
            Some("2026-10-23T04:00:00Z"),
        ),
        (
            "Tue, 09 Jun 2020 08:00:00 GMT",
            Some("2020-06-09T08:00:00Z"),
        ),
        // 没有星期、没有秒、一位数小时
        ("05 Mar 2024 08:00 -0500", Some("2024-03-05T08:00:00-05:00")),
        ("Tue, 05 Mar 2024 8:00:00 GMT", Some("2024-03-05T08:00:00Z")),
        // 没有时刻
        ("Tue, 05 Mar 2024", Some("2024-03-05")),
        ("5 March 2024", Some("2024-03-05")),
        ("Tuesday, 05 March 2024", Some("2024-03-05")),
        // 没有时区
        ("Tue, 05 Mar 2024 08:00:00", Some("2024-03-05T08:00:00")),
        // 两位年份
        ("Tue, 05 Mar 24 08:00:00 GMT", Some("2024-03-05T08:00:00Z")),
        ("Fri, 05 Mar 99 08:00:00 GMT", Some("1999-03-05T08:00:00Z")),
        // 其他语言的月份名也认
        ("05 März 2024", Some("2024-03-05")),
        ("5 juin 2024", Some("2024-06-05")),
        // RFC 2822 §4.3 的旧式时区名（0.80 前 EST 等被丢掉，没有偏移）
        (
            "Tue, 05 Mar 2024 08:00:00 EST",
            Some("2024-03-05T08:00:00-05:00"),
        ),
        (
            "Tue, 05 Mar 2024 08:00:00 PDT",
            Some("2024-03-05T08:00:00-07:00"),
        ),
        (
            "Tue, 05 Mar 2024 08:00:00 CST",
            Some("2024-03-05T08:00:00-06:00"),
        ),
        ("Tue, 05 Mar 2024 08:00:00 UT", Some("2024-03-05T08:00:00Z")),
        ("Tue, 05 Mar 2024 08:00:00 A", Some("2024-03-05T08:00:00")), // 军用时区按规范当未知
        ("Tue, 05 Mar 2024 08:00:00 XYZ", Some("2024-03-05T08:00:00")),
        // 不合法
        ("Tue, 30 Feb 2024 08:00:00 GMT", None),
        ("Tue, 05 Foo 2024 08:00:00 GMT", None),
        ("Tue, 05 Mar 2024 25:00:00 GMT", None),
        ("Tue, 05 Mar 2024 08:60:00 GMT", None),
        ("Tue, 05 Mar 2024 08:00:00 +9999", None),
        ("Tue, 05 Mar 2024 08 GMT", None),
        // 不支持的写法：RFC 850、asctime、ISO
        ("Tuesday, 05-Mar-24 08:00:00 GMT", None),
        ("Tue Mar  5 08:00:00 2024", None),
        ("2024-03-05", None),
        ("", None),
    ];
    check_opt("parse_rfc2822", CASES, |s| render(parse_rfc2822(s)));
}

// ---------------------------------------------------------------------------
// normalize：meta、JSON-LD、<time datetime> 的结构化值（jsonld.rs 的日期字段也走这里）

#[test]
fn table_normalize() {
    const CASES: &[(&str, Option<&str>)] = &[
        // 原 normalizes_structured_values_keeping_the_offset
        (
            "2020-02-15T07:00:00+09:00",
            Some("2020-02-15T07:00:00+09:00"),
        ),
        ("2020-04-02T04:11:00+0000", Some("2020-04-02T04:11:00Z")),
        ("2020-04-02", Some("2020-04-02")),
        ("2020-04-02T04:11", Some("2020-04-02T04:11:00")),
        ("not a date", None),
        // 补充
        ("  2024-03-05  ", Some("2024-03-05")),
        (
            "2024-03-05T08:30:00.000+00:00",
            Some("2024-03-05T08:30:00Z"),
        ),
        ("2024-03-05T08:30:00.123Z", Some("2024-03-05T08:30:00Z")),
        (
            "2024-03-05T08:30:00-07:00",
            Some("2024-03-05T08:30:00-07:00"),
        ),
        ("2024-03-05 08:30:00", Some("2024-03-05T08:30:00")),
        ("2024/03/05", Some("2024-03-05")),
        (
            "Tue, 05 Mar 2024 08:00:00 GMT",
            Some("2024-03-05T08:00:00Z"),
        ),
        (
            "Tue, 05 Mar 2024 08:00:00 +0100",
            Some("2024-03-05T08:00:00+01:00"),
        ),
        // 不在 1000..=9999 的年份、多余内容、非结构化写法
        ("0999-12-31", None),
        ("2024-03-05 extra", None),
        ("05/03/2024", None),
        ("March 5, 2024", None),
        ("2024-02-30", None),
        ("", None),
    ];
    check_opt("normalize", CASES, |s| normalize(s));
}
