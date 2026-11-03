//! DASH 规划（`stream/dash.rs`）的表驱动测试：每个函数一张用例表，一次跑完全部用例，最后统一报告失败的条目。
//!
//! 期望写法：成功时是结果的文字形式；失败时写 `Err: 错误信息`，只写 `Err` 表示任何错误都算（XML 库自己的错误）。

use std::fmt::Debug;

use url::Url;

use super::dash::{dash_range, expand, iso_duration};
use super::{StreamPlan, plan_stream};

/// 跑一张表：逐条比较，收集所有失败再统一 panic（打印序号、输入、期望、实际）。
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

/// 结果文字和期望相同；期望只写 `Err` 时任何错误都算。
pub(super) fn same(got: &String, want: &&str) -> bool {
    got == want || (*want == "Err" && got.starts_with("Err"))
}

fn result<T>(r: anyhow::Result<T>, ok: impl Fn(T) -> String) -> String {
    match r {
        Ok(v) => ok(v),
        Err(e) => format!("Err: {e}"),
    }
}

// ---------------------------------------------------------------------------
// ISO 8601 时长：只认 PT 加时/分/秒，单位必须按顺序、各出现一次

#[test]
fn table_iso_duration() {
    const CASES: &[(&str, Option<f64>)] = &[
        ("PT1H2M3.5S", Some(3723.5)),
        ("PT0S", Some(0.0)),
        ("PT0.0S", Some(0.0)),
        ("PT1.5S", Some(1.5)),
        ("PT10M", Some(600.0)),
        ("PT2H", Some(7200.0)),
        ("PT1M30S", Some(90.0)),
        ("PT1H30S", Some(3630.0)),
        ("PT634.566S", Some(634.566)),
        ("PT.5S", Some(0.5)), // Rust 的 f64 解析接受省略整数部分
        // 不合法或不支持
        ("PT", None),     // 空
        ("PT5", None),    // 没有单位
        ("PT1S5", None),  // 结尾没有单位
        ("PT1M1H", None), // 顺序颠倒
        ("PT1S1S", None), // 重复
        ("PT1X", None),   // 未知单位
        ("PTS", None),
        ("PT-1S", None),
        ("PT1.5.5S", None),
        ("PT1e3S", None),
        ("PTNaNS", None),
        ("P1D", None), // 带日期部分的不支持
        ("P1DT2H", None),
        ("pt1s", None),
        ("1H", None),
        ("", None),
    ];
    check(
        "iso_duration",
        CASES,
        |s| iso_duration(s).ok(),
        |a, e| a == e,
    );
}

// ---------------------------------------------------------------------------
// SegmentTemplate 占位符展开

#[allow(clippy::type_complexity)] // 表驱动测试：一行就是一条用例，拆成 type 定义反而难读
#[test]
fn table_expand() {
    let long = "x".repeat(5000);
    // ((模板, RepresentationID, Bandwidth, Number, Time), 期望)
    let cases: Vec<((&str, &str, Option<&str>, u64, u64), &str)> = vec![
        (
            ("$RepresentationID$/$Number$.m4s", "v1", None, 7, 0),
            "v1/7.m4s",
        ),
        (
            ("chunk-$Number%05d$.m4s", "v1", None, 7, 0),
            "chunk-00007.m4s",
        ),
        (("$Number%016d$", "v1", None, 7, 0), "0000000000000007"),
        (("$Number%02d$", "v1", None, 12345, 0), "12345"), // 超过宽度不截断
        (("t-$Time$.m4s", "v1", None, 7, 90000), "t-90000.m4s"),
        (("t-$Time%08d$.m4s", "v1", None, 7, 90), "t-00000090.m4s"),
        (
            ("$Bandwidth$/s.m4s", "v1", Some("500000"), 7, 0),
            "500000/s.m4s",
        ),
        (("$Bandwidth%08d$", "v1", Some("500"), 7, 0), "00000500"),
        (
            ("$Number$-$Number%03d$-$Time$", "v1", None, 7, 100),
            "7-007-100",
        ),
        (("a$$b", "v1", None, 7, 0), "a$b"), // "$$" 是转义的 "$"
        (("$$$Number$", "v1", None, 7, 0), "$7"),
        (("init.mp4", "v1", None, 7, 0), "init.mp4"),
        (("", "v1", None, 7, 0), ""),
        // 错误
        (
            ("$Bandwidth$", "v1", None, 7, 0),
            "Err: DASH bandwidth missing",
        ),
        (
            ("$Unknown$", "v1", None, 7, 0),
            "Err: unknown DASH placeholder",
        ),
        (("$Number", "v1", None, 7, 0), "Err: unclosed DASH template"),
        (
            ("$Number%5d$", "v1", None, 7, 0),
            "Err: unknown DASH placeholder",
        ), // 宽度必须写成 %0Nd
        (
            ("$Number%05x$", "v1", None, 7, 0),
            "Err: invalid DASH format",
        ),
        (
            ("$Number%00d$", "v1", None, 7, 0),
            "Err: DASH template width exceeds budget",
        ),
        (
            ("$Number%017d$", "v1", None, 7, 0),
            "Err: DASH template width exceeds budget",
        ),
        (("$Number%0d$", "v1", None, 7, 0), "Err"), // 宽度为空
        (
            ("$RepresentationID%05d$", "v1", None, 7, 0),
            "Err: representation formatting unsupported",
        ),
        (
            ("$RepresentationID$$RepresentationID$", &long, None, 7, 0),
            "Err: DASH expanded URL exceeds budget",
        ),
    ];
    check(
        "expand",
        &cases,
        |&(t, id, bw, n, time)| result(expand(t, id, bw, n, time), |s| s),
        same,
    );
}

// ---------------------------------------------------------------------------
// 字节范围 `a-b`

#[test]
fn table_dash_range() {
    const CASES: &[(&str, Option<(u64, u64)>)] = &[
        ("0-9", Some((0, 9))),
        ("10-10", Some((10, 10))),
        ("0-18446744073709551615", Some((0, u64::MAX))),
        ("9-0", None), // 颠倒
        ("a-b", None),
        ("10", None),
        ("-5", None), // 只给后缀长度的写法不支持
        ("5-", None),
        (" 0-9", None),
        ("", None),
    ];
    check("dash_range", CASES, |s| dash_range(s).ok(), |a, e| a == e);
}

// ---------------------------------------------------------------------------
// 整个 MPD 规划成分片列表

/// 每个轨道一行：`id mime: 分片 | 分片 …`，初始化分片前面写 `init`，有字节范围时写 `[a-b]`。
fn plan(source: &str) -> String {
    let base = Url::parse("https://example.test/root/manifest.mpd").unwrap();
    result(plan_stream(source, &base), |p| match p {
        StreamPlan::Master { .. } => "master".into(),
        StreamPlan::Vod { tracks, .. } => tracks
            .iter()
            .map(|t| {
                let segs: Vec<String> = t
                    .segments
                    .iter()
                    .map(|s| {
                        let mut x = String::new();
                        if s.initialization {
                            x.push_str("init ");
                        }
                        x.push_str(&s.url);
                        if let Some((a, b)) = s.range {
                            x.push_str(&format!("[{a}-{b}]"));
                        }
                        x
                    })
                    .collect();
                format!("{} {}: {}", t.id, t.mime, segs.join(" | "))
            })
            .collect::<Vec<_>>()
            .join("\n"),
    })
}

/// 包一层最简单的 MPD：`body` 放在 AdaptationSet 里。
fn mpd(root_attrs: &str, set_attrs: &str, body: &str) -> String {
    format!(
        "<MPD{root_attrs}><Period><AdaptationSet{set_attrs}>{body}</AdaptationSet></Period></MPD>"
    )
}

#[test]
fn table_plan_dash() {
    // 原 dash_finite_timeline_and_segment_list 的清单
    const TIMELINE: &str = r#"<MPD type="static"><BaseURL>media/</BaseURL><Period><AdaptationSet mimeType="video/mp4"><SegmentTemplate initialization="$RepresentationID$-init.mp4" media="$RepresentationID$-$Number%03d$-$Time$.m4s" startNumber="7"><SegmentTimeline><S t="20" d="5" r="1"/></SegmentTimeline></SegmentTemplate><Representation id="v1" bandwidth="100"/></AdaptationSet></Period></MPD>"#;
    const TIMELINE_OK: &str = "v1 video/mp4: init https://example.test/root/media/v1-init.mp4 | https://example.test/root/media/v1-007-20.m4s | https://example.test/root/media/v1-008-25.m4s";
    const LIST: &str = r#"<MPD><Period><AdaptationSet><Representation id="a"><SegmentList><Initialization sourceURL="init.mp4" range="0-9"/><SegmentURL media="a.m4s" mediaRange="10-19"/></SegmentList></Representation></AdaptationSet></Period></MPD>"#;
    let dur = r#" mediaPresentationDuration="PT10S""#;
    let rep = r#"<Representation id="a" bandwidth="64000"/>"#;
    let cases: Vec<(String, &str)> = vec![
        // ---- 原有断言 ----
        (TIMELINE.into(), TIMELINE_OK),
        (TIMELINE.replace("<Period>", "<Period start='PT0.0S'>"), TIMELINE_OK),
        (
            LIST.into(),
            "a application/octet-stream: init https://example.test/root/init.mp4[0-9] | https://example.test/root/a.m4s[10-19]",
        ),
        (
            TIMELINE.replace("type=\"static\"", "type=\"dynamic\""),
            "Err: only static DASH MPD is supported",
        ),
        (
            TIMELINE.replace("r=\"1\"", "r=\"-1\""),
            "Err: unbounded or excessive DASH repetition",
        ),
        (
            TIMELINE.replace(
                "<Representation id=\"v1\" bandwidth=\"100\"/>",
                "<ContentProtection/><Representation id=\"v1\" bandwidth=\"100\"/>",
            ),
            "Err: encrypted/index-addressed DASH unsupported",
        ),
        // ---- SegmentTemplate + duration：分片数 = ceil(时长 × timescale / duration) ----
        (
            mpd(
                dur,
                r#" mimeType="audio/mp4""#,
                &format!(r#"<SegmentTemplate timescale="1000" duration="4000" media="$Bandwidth$/$Number%05d$.m4s"/>{rep}"#),
            ),
            "a audio/mp4: https://example.test/root/64000/00001.m4s | https://example.test/root/64000/00002.m4s | https://example.test/root/64000/00003.m4s",
        ),
        // Period 的 duration 优先于 mediaPresentationDuration
        (
            r#"<MPD mediaPresentationDuration="PT100S"><Period duration="PT8S"><AdaptationSet><SegmentTemplate duration="4" media="s$Number$.m4s"/><Representation id="a"/></AdaptationSet></Period></MPD>"#.into(),
            "a application/octet-stream: https://example.test/root/s1.m4s | https://example.test/root/s2.m4s",
        ),
        // startNumber、initialization 里的 $Number$ 取 startNumber
        (
            mpd(
                dur,
                "",
                &format!(r#"<SegmentTemplate duration="5" startNumber="0" initialization="i$Number$.mp4" media="s$Number$-$Time$.m4s"/>{rep}"#),
            ),
            "a application/octet-stream: init https://example.test/root/i0.mp4 | https://example.test/root/s0-0.m4s | https://example.test/root/s1-5.m4s",
        ),
        (
            mpd("", "", &format!(r#"<SegmentTemplate duration="4" media="s$Number$.m4s"/>{rep}"#)),
            "Err: DASH numbered template needs finite period duration",
        ),
        (
            mpd(dur, "", &format!(r#"<SegmentTemplate media="s$Number$.m4s"/>{rep}"#)),
            "Err: DASH template needs duration or timeline",
        ),
        (
            mpd(dur, "", &format!(r#"<SegmentTemplate duration="0" media="s.m4s"/>{rep}"#)),
            "Err: DASH duration must be positive",
        ),
        (
            mpd(dur, "", &format!(r#"<SegmentTemplate timescale="0" duration="1" media="s.m4s"/>{rep}"#)),
            "Err: DASH timescale must be positive",
        ),
        (
            mpd(dur, "", &format!(r#"<SegmentTemplate duration="1" presentationTimeOffset="5" media="s.m4s"/>{rep}"#)),
            "Err: numbered DASH timing offset unsupported",
        ),
        (
            mpd(
                r#" mediaPresentationDuration="PT100000S""#,
                "",
                &format!(r#"<SegmentTemplate duration="1" media="s$Number$.m4s"/>{rep}"#),
            ),
            "Err: DASH duration segment budget exceeded",
        ),
        (
            mpd(dur, "", &format!(r#"<SegmentTemplate duration="1" eptDelta="1" media="s.m4s"/>{rep}"#)),
            "Err: DASH timing/index extension unsupported",
        ),
        (
            mpd(dur, "", &format!(r#"<SegmentTemplate duration="1"/>{rep}"#)),
            "Err: DASH media template missing",
        ),
        (
            mpd(dur, "", &format!(r#"<SegmentTemplate duration="5" startNumber="x" media="s.m4s"/>{rep}"#)),
            "Err: invalid digit found in string",
        ),
        (
            mpd(dur, "", r#"<SegmentTemplate duration="5" media="$Bandwidth$.m4s"/><Representation id="a"/>"#),
            "Err: DASH bandwidth missing",
        ),
        // ---- SegmentTimeline：t 跳变、r 重复、$Time$ ----
        (
            mpd(
                "",
                "",
                &format!(r#"<SegmentTemplate timescale="10" media="t$Time$.m4s"><SegmentTimeline><S t="100" d="50" r="1"/><S d="60"/><S t="500" d="10"/></SegmentTimeline></SegmentTemplate>{rep}"#),
            ),
            "a application/octet-stream: https://example.test/root/t100.m4s | https://example.test/root/t150.m4s | https://example.test/root/t200.m4s | https://example.test/root/t500.m4s",
        ),
        (
            mpd("", "", &format!(r#"<SegmentTemplate media="s.m4s"><SegmentTimeline><S d="0"/></SegmentTimeline></SegmentTemplate>{rep}"#)),
            "Err: empty DASH duration",
        ),
        (
            mpd("", "", &format!(r#"<SegmentTemplate media="s.m4s"><SegmentTimeline><S t="1"/></SegmentTimeline></SegmentTemplate>{rep}"#)),
            "Err: DASH timeline duration missing",
        ),
        (
            mpd("", "", &format!(r#"<SegmentTemplate media="s.m4s"><SegmentTimeline><S d="1" r="5000"/></SegmentTimeline></SegmentTemplate>{rep}"#)),
            "Err: unbounded or excessive DASH repetition",
        ),
        // ---- AdaptationSet 级模板与 Representation 级模板合并：属性和时间线从上级补 ----
        (
            mpd(
                "",
                "",
                r#"<SegmentTemplate timescale="10" initialization="i-$RepresentationID$.mp4"><SegmentTimeline><S d="20" r="1"/></SegmentTimeline></SegmentTemplate><Representation id="v"><SegmentTemplate media="$RepresentationID$-$Time$.m4s"/></Representation>"#,
            ),
            "v application/octet-stream: init https://example.test/root/i-v.mp4 | https://example.test/root/v-0.m4s | https://example.test/root/v-20.m4s",
        ),
        // 一个 AdaptationSet 级模板给多个 Representation；Representation 的 mimeType 优先
        (
            mpd(
                dur,
                r#" mimeType="video/mp4""#,
                r#"<SegmentTemplate duration="10" media="$RepresentationID$.m4s"/><Representation id="lo"/><Representation id="hi" mimeType="video/webm"/>"#,
            ),
            "lo video/mp4: https://example.test/root/lo.m4s\nhi video/webm: https://example.test/root/hi.m4s",
        ),
        // ---- SegmentList ----
        (
            mpd(
                "",
                "",
                r#"<Representation id="a"><BaseURL>v.mp4</BaseURL><SegmentList><Initialization range="0-99"/><SegmentURL media="v.mp4" mediaRange="100-199"/><SegmentURL media="v.mp4" mediaRange="200-299"/></SegmentList></Representation>"#,
            ),
            "a application/octet-stream: init https://example.test/root/v.mp4[0-99] | https://example.test/root/v.mp4[100-199] | https://example.test/root/v.mp4[200-299]",
        ),
        // AdaptationSet 级的 SegmentList 给下面的 Representation 用
        (
            mpd("", "", r#"<SegmentList><SegmentURL media="x.m4s"/></SegmentList><Representation id="a"/>"#),
            "a application/octet-stream: https://example.test/root/x.m4s",
        ),
        (
            mpd("", "", r#"<Representation id="a"><SegmentList><SegmentURL media="a.m4s" indexRange="0-9"/></SegmentList></Representation>"#),
            "Err: DASH segment indexes unsupported",
        ),
        (
            mpd("", "", r#"<Representation id="a"><SegmentList><SegmentURL/></SegmentList></Representation>"#),
            "Err: DASH media missing",
        ),
        (
            mpd("", "", r#"<Representation id="a"><SegmentList><SegmentURL media=""/></SegmentList></Representation>"#),
            "Err: invalid segment URL",
        ),
        (
            mpd("", "", r#"<Representation id="a"><SegmentList><SegmentURL media="a.m4s" mediaRange="19-10"/></SegmentList></Representation>"#),
            "Err: inverted DASH range",
        ),
        (
            mpd("", "", r#"<Representation id="a"><SegmentList><Initialization sourceURL="i.mp4"/></SegmentList></Representation>"#),
            "Err: empty DASH representation",
        ),
        // ---- SegmentBase 不支持 ----
        (
            mpd("", "", r#"<Representation id="a"><BaseURL>v.mp4</BaseURL><SegmentBase indexRange="0-99"/></Representation>"#),
            "Err: encrypted/index-addressed DASH unsupported",
        ),
        // ---- BaseURL 逐级拼接 ----
        (
            r#"<MPD><BaseURL>https://cdn.test/a/</BaseURL><Period><BaseURL>p/</BaseURL><AdaptationSet><BaseURL>s/</BaseURL><Representation id="a"><BaseURL>r/</BaseURL><SegmentList><SegmentURL media="1.m4s"/></SegmentList></Representation></AdaptationSet></Period></MPD>"#.into(),
            "a application/octet-stream: https://cdn.test/a/p/s/r/1.m4s",
        ),
        (
            mpd("", "", r#"<Representation id="a"><BaseURL>https://other.test/x/</BaseURL><SegmentList><SegmentURL media="1.m4s"/></SegmentList></Representation>"#),
            "a application/octet-stream: https://other.test/x/1.m4s",
        ),
        // 分片地址本身是绝对地址时不再拼 BaseURL；片段标识去掉
        (
            r#"<MPD><BaseURL>media/</BaseURL><Period><AdaptationSet><Representation id="a"><SegmentList><SegmentURL media="https://other.test/1.m4s#t=1"/><SegmentURL media="/abs.m4s"/></SegmentList></Representation></AdaptationSet></Period></MPD>"#.into(),
            "a application/octet-stream: https://other.test/1.m4s | https://example.test/abs.m4s",
        ),
        (
            r#"<MPD><BaseURL>a/</BaseURL><BaseURL>b/</BaseURL><Period><AdaptationSet><Representation id="a"><SegmentList><SegmentURL media="1.m4s"/></SegmentList></Representation></AdaptationSet></Period></MPD>"#.into(),
            "Err: multiple DASH BaseURL alternatives unsupported",
        ),
        (
            r#"<MPD><BaseURL>https://u:p@cdn.test/</BaseURL><Period><AdaptationSet><Representation id="a"><SegmentList><SegmentURL media="1.m4s"/></SegmentList></Representation></AdaptationSet></Period></MPD>"#.into(),
            "Err: segments require credential-free HTTP(S)",
        ),
        (
            r#"<MPD><BaseURL>ftp://cdn.test/</BaseURL><Period><AdaptationSet><Representation id="a"><SegmentList><SegmentURL media="1.m4s"/></SegmentList></Representation></AdaptationSet></Period></MPD>"#.into(),
            "Err: segments require credential-free HTTP(S)",
        ),
        // ---- 命名空间前缀 ----
        (
            r#"<mpd:MPD xmlns:mpd="urn:mpeg:dash:schema:mpd:2011" type="static"><mpd:Period><mpd:AdaptationSet><mpd:Representation id="a"><mpd:SegmentList><mpd:SegmentURL media="1.m4s"/></mpd:SegmentList></mpd:Representation></mpd:AdaptationSet></mpd:Period></mpd:MPD>"#.into(),
            "a application/octet-stream: https://example.test/root/1.m4s",
        ),
        // ---- Period ----
        (
            r#"<MPD><Period/><Period/></MPD>"#.into(),
            "Err: DASH requires one explicit period",
        ),
        (r#"<MPD></MPD>"#.into(), "Err: DASH requires one explicit period"),
        (
            LIST.replace("<Period>", r#"<Period start="PT5S">"#),
            "Err: nonzero DASH period start unsupported",
        ),
        (
            LIST.replace("<Period>", r#"<Period start="P1D">"#),
            "Err: only DASH PT hour/minute/second durations are supported",
        ),
        // ---- Representation ----
        (
            mpd("", "", r#"<Representation><SegmentList><SegmentURL media="1.m4s"/></SegmentList></Representation>"#),
            "Err: DASH representation id missing",
        ),
        (
            mpd("", "", r#"<Representation id=""><SegmentList><SegmentURL media="1.m4s"/></SegmentList></Representation>"#),
            "Err: invalid DASH representation id",
        ),
        (
            mpd("", "", r#"<Representation id="a"/>"#),
            "Err: DASH representation lacks supported segments",
        ),
        (mpd("", "", ""), "Err: DASH has no supported representations"),
        // ---- 根元素、XML ----
        (r#"<foo/>"#.into(), "Err: only static DASH MPD is supported"),
        (
            LIST.replace("<Period>", r#"<Period xlink:href="https://evil.test/p.xml">"#),
            "Err: external DASH XML references unsupported",
        ),
        (
            format!("<!DOCTYPE MPD>{LIST}"),
            "Err: DASH DTD unsupported",
        ),
        (r#"<MPD><Period></MPD>"#.into(), "Err"),
    ];
    check("plan_stream(DASH)", &cases, |s| plan(s), same);
}
