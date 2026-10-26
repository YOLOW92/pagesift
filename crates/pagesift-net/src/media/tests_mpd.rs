//! MPD 解析器的表驱动测试：每个函数一张用例表，一次跑完全部用例，最后统一报告失败的条目。
//!
//! 期望值按当前实现写；标 `// BUG?` 的是"当前行为看起来不对"的用例，留待以后决定要不要改生产代码。

use std::fmt::{Debug, Write as _};

use url::Url;

use super::*;

/// 跑一张表：逐条比较，收集所有失败再统一 panic（打印序号、输入、期望、实际）。
fn check<I: Debug, E: Debug, A: Debug>(
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

// ---------------------------------------------------------------------------
// ISO 8601 时长（只认 PT 开头的时/分/秒）

#[test]
fn table_parse_iso8601_duration() {
    const CASES: &[(&str, Option<f64>)] = &[
        ("PT1H2M3.5S", Some(3723.5)),
        ("PT0S", Some(0.0)),
        ("PT1.5S", Some(1.5)),
        ("PT0.040S", Some(0.04)),
        ("PT10M", Some(600.0)),
        ("PT2H", Some(7200.0)),
        ("PT1M30S", Some(90.0)),
        ("PT1H30S", Some(3630.0)),
        ("PT634.566S", Some(634.566)),
        ("PT1M1H", Some(3660.0)), // 单位顺序不检查（宽松）
        // 带日期部分的不支持
        ("P1D", None),
        ("P1DT2H", None),
        ("P0Y0M0DT0H3M30.000S", None),
        // 不合法
        ("PT-1S", None),
        ("PT1.5.5S", None),
        ("PTS", None),
        ("1H", None),
        ("pt1s", None),
        ("", None),
        ("PT", Some(0.0)),    // BUG? 空时长当成 0 秒，应为 None
        ("PT5", Some(0.0)),   // BUG? 最后的数字没有单位，被悄悄丢掉
        ("PT1X", Some(0.0)),  // BUG? 未知单位被忽略，应为 None
        ("PT1S5", Some(1.0)), // BUG? 同上，结尾没单位的数字丢掉
    ];
    check(
        "parse_iso8601_duration",
        CASES,
        |s| parse_iso8601_duration(s),
        |a, e| a == e,
    );
}

// ---------------------------------------------------------------------------
// SegmentTemplate 的占位符

#[test]
fn table_replace_template() {
    // ((模板, RepresentationID, Number, Bandwidth, Time), 期望)
    const CASES: &[((&str, &str, u64, u64, u64), &str)] = &[
        (
            ("$RepresentationID$/$Number$.m4s", "v1", 7, 100, 0),
            "v1/7.m4s",
        ),
        (
            ("chunk-$Number%05d$.m4s", "v1", 7, 100, 0),
            "chunk-00007.m4s",
        ),
        (("$Number%010d$", "v1", 7, 100, 0), "0000000007"),
        (("$Number%02d$", "v1", 12345, 100, 0), "12345"), // 超过宽度不截断
        (("$Bandwidth$/$Number$", "v1", 7, 500000, 0), "500000/7"),
        (("t-$Time$.m4s", "v1", 7, 100, 90000), "t-90000.m4s"),
        (
            ("$RepresentationID$-$RepresentationID$", "a", 1, 1, 0),
            "a-a",
        ),
        (("$Number$-$Number$", "v1", 7, 100, 0), "7-7"), // 同样写法的占位符全部替换
        (("init.mp4", "v1", 7, 100, 0), "init.mp4"),
        (("$Unknown$.m4s", "v1", 7, 100, 0), "$Unknown$.m4s"), // 未知占位符原样保留
        (("$Number%5d$", "v1", 7, 100, 0), "7"),               // 没有 0 的宽度写法不补零
        (("$Time%08d$.m4s", "v1", 7, 100, 90), "$Time%08d$.m4s"), // BUG? $Time$ 的宽度格式不支持，原样留下
        (("a$$b-$Number$", "v1", 7, 100, 0), "a$$b-7"),           // BUG? "$$" 应还原成 "$"
        (("$Number%05d$-$Number$", "v1", 7, 100, 0), "00007-$Number$"), // BUG? 只处理第一种 $Number 写法
        (("$Bandwidth%08d$", "v1", 7, 100, 0), "$Bandwidth%08d$"), // BUG? $Bandwidth$ 的宽度格式不支持
    ];
    check(
        "replace_template",
        CASES,
        |&(t, id, n, bw, time)| replace_template(t, id, n, bw, time),
        |a, e| a == e,
    );
}

// ---------------------------------------------------------------------------
// 整个清单

/// 把解析结果压成一段文字：清单级时长，每个 AdaptationSet、Representation 一行，分片列在后面。
/// 分片写成 `地址#序号@秒`，初始化分片写成 `地址#init`；缺省的属性写 `-`。
fn summary(r: Result<DashManifest, DashError>) -> String {
    let m = match r {
        Ok(m) => m,
        Err(e) => return format!("Err({})", e.to_string().split(':').next().unwrap_or("")),
    };
    let opt = |v: &Option<String>| v.clone().unwrap_or_else(|| "-".into());
    let num = |v: Option<u32>| v.map_or("-".into(), |x| x.to_string());
    let mut s = format!(
        "buf={} dur={}",
        m.min_buffer_time_s,
        m.media_presentation_duration_s
            .map_or("-".into(), |d| d.to_string())
    );
    for a in &m.adaptation_sets {
        write!(
            s,
            "\nset {} {} {}",
            opt(&a.id),
            opt(&a.content_type),
            opt(&a.mime_type)
        )
        .unwrap();
        for r in &a.representations {
            write!(
                s,
                "\n {} {} {}x{} {}:",
                r.id,
                r.bandwidth,
                num(r.width),
                num(r.height),
                opt(&r.codecs)
            )
            .unwrap();
            for seg in &r.segments {
                if seg.is_init {
                    write!(s, " {}#init", seg.uri).unwrap();
                } else {
                    write!(s, " {}#{}@{}", seg.uri, seg.number, seg.duration_s).unwrap();
                }
            }
        }
    }
    s
}

#[test]
fn table_parse_mpd() {
    const BASE: &str = "https://example.test/m/manifest.mpd";
    // ((MPD, 是否传入清单地址), 期望摘要)
    const CASES: &[((&str, bool), &str)] = &[
        // 原 test_mpd_segment_template_parsing：SegmentTimeline、$RepresentationID$、$Number%05d$
        (
            (
                r#"<?xml version="1.0" encoding="UTF-8"?>
<MPD xmlns="urn:mpeg:dash:schema:mpd:2011" minBufferTime="PT1.5S" mediaPresentationDuration="PT5S">
  <Period id="0">
    <AdaptationSet id="0" contentType="video" mimeType="video/mp4">
      <SegmentTemplate timescale="1000" initialization="init-$RepresentationID$.mp4" media="chunk-$RepresentationID$-$Number%05d$.m4s" startNumber="1">
        <SegmentTimeline>
          <S t="0" d="1000" r="4"/>
        </SegmentTimeline>
      </SegmentTemplate>
      <Representation id="1080p" bandwidth="5000000" width="1920" height="1080" codecs="avc1.640028" />
    </AdaptationSet>
  </Period>
</MPD>"#,
                false,
            ),
            "buf=1.5 dur=5\nset 0 video video/mp4\n 1080p 5000000 1920x1080 avc1.640028: init-1080p.mp4#init chunk-1080p-00001.m4s#1@1 chunk-1080p-00002.m4s#2@1 chunk-1080p-00003.m4s#3@1 chunk-1080p-00004.m4s#4@1 chunk-1080p-00005.m4s#5@1",
        ),
        // 时长写法：时、分、小数秒
        (
            (
                r#"<MPD minBufferTime="PT2S" mediaPresentationDuration="PT1H2M3.5S"/>"#,
                false,
            ),
            "buf=0 dur=-", // BUG? 自闭合的 <MPD/> 是 Empty 事件，属性没读
        ),
        (
            (
                r#"<MPD minBufferTime="PT2S" mediaPresentationDuration="PT1H2M3.5S"></MPD>"#,
                false,
            ),
            "buf=2 dur=3723.5",
        ),
        (
            (
                r#"<MPD minBufferTime="P1D" mediaPresentationDuration="bogus"></MPD>"#,
                false,
            ),
            "buf=0 dur=-",
        ),
        // 带命名空间前缀的元素按本地名认
        (
            (
                r#"<mpd:MPD xmlns:mpd="urn:mpeg:dash:schema:mpd:2011" mediaPresentationDuration="PT2S"><mpd:Period><mpd:AdaptationSet mimeType="audio/mp4"><mpd:SegmentTemplate media="a-$Number$.m4s" duration="1"></mpd:SegmentTemplate><mpd:Representation id="a" bandwidth="64000"/></mpd:AdaptationSet></mpd:Period></mpd:MPD>"#,
                false,
            ),
            "buf=0 dur=2\nset - - audio/mp4\n a 64000 -x- -: a-1.m4s#1@1 a-2.m4s#2@1",
        ),
        // 按 duration 编号：分片数 = ceil(总时长 / 分片时长)，startNumber 可以从 0 开始
        (
            (
                r#"<MPD mediaPresentationDuration="PT10S"><Period><AdaptationSet><SegmentTemplate timescale="1000" duration="4000" startNumber="0" media="s-$Number$-$Time$.m4s"></SegmentTemplate><Representation id="a" bandwidth="1"/></AdaptationSet></Period></MPD>"#,
                false,
            ),
            "buf=0 dur=10\nset - - -\n a 1 -x- -: s-0-0.m4s#0@4 s-1-4000.m4s#1@4 s-2-8000.m4s#2@4",
        ),
        // 没有总时长时按 60 秒猜
        (
            (
                r#"<MPD><Period><AdaptationSet><SegmentTemplate duration="20" media="s-$Number$.m4s"></SegmentTemplate><Representation id="a" bandwidth="1"/></AdaptationSet></Period></MPD>"#,
                false,
            ),
            "buf=0 dur=-\nset - - -\n a 1 -x- -: s-1.m4s#1@20 s-2.m4s#2@20 s-3.m4s#3@20", // BUG? 不知道总时长时编造 60 秒，应报错或不生成
        ),
        // 自闭合的 SegmentTemplate（按 duration 编号时最常见的写法）
        (
            (
                r#"<MPD mediaPresentationDuration="PT10S"><Period><AdaptationSet><SegmentTemplate timescale="1" duration="5" media="s-$Number$.m4s"/><Representation id="a" bandwidth="1"/></AdaptationSet></Period></MPD>"#,
                false,
            ),
            "buf=0 dur=10\nset - - -\n a 1 -x- -:", // BUG? 自闭合的 SegmentTemplate 是 Empty 事件，被整个忽略
        ),
        // SegmentTimeline：t 跳变、r 重复、$Time$
        (
            (
                r#"<MPD><Period><AdaptationSet><SegmentTemplate timescale="10" media="t-$Time$.m4s"><SegmentTimeline><S t="100" d="50" r="1"/><S d="60"/><S t="500" d="10"/></SegmentTimeline></SegmentTemplate><Representation id="a" bandwidth="1"/></AdaptationSet></Period></MPD>"#,
                false,
            ),
            "buf=0 dur=-\nset - - -\n a 1 -x- -: t-100.m4s#1@5 t-150.m4s#2@5 t-200.m4s#3@6 t-500.m4s#4@1",
        ),
        // Representation 里的 SegmentTemplate；$Bandwidth$；宽度、高度、编码
        (
            (
                r#"<MPD mediaPresentationDuration="PT4S"><Period><AdaptationSet contentType="video"><Representation id="hd" bandwidth="3000000" width="1280" height="720" codecs="avc1.4d401f"><SegmentTemplate duration="2" initialization="$Bandwidth$/init.mp4" media="$Bandwidth$/$Number%03d$.m4s"></SegmentTemplate></Representation></AdaptationSet></Period></MPD>"#,
                false,
            ),
            "buf=0 dur=4\nset - video -\n hd 3000000 1280x720 avc1.4d401f: 3000000/init.mp4#init 3000000/001.m4s#1@2 3000000/002.m4s#2@2",
        ),
        // 一个 AdaptationSet 级模板给多个 Representation 用；属性写错时取缺省值
        (
            (
                r#"<MPD mediaPresentationDuration="PT2S"><Period><AdaptationSet><SegmentTemplate duration="2" media="$RepresentationID$.m4s"></SegmentTemplate><Representation id="lo" bandwidth="abc" width="x"/><Representation id="hi" bandwidth="9"/></AdaptationSet></Period></MPD>"#,
                false,
            ),
            "buf=0 dur=2\nset - - -\n lo 0 -x- -: lo.m4s#1@2\n hi 9 -x- -: hi.m4s#1@2",
        ),
        // BaseURL 与清单地址拼接
        (
            (
                r#"<MPD mediaPresentationDuration="PT1S"><Period><AdaptationSet><SegmentTemplate duration="1" media="seg-$Number$.m4s"></SegmentTemplate><Representation id="a" bandwidth="1"/></AdaptationSet></Period></MPD>"#,
                true,
            ),
            "buf=0 dur=1\nset - - -\n a 1 -x- -: https://example.test/m/seg-1.m4s#1@1",
        ),
        (
            (
                r#"<MPD mediaPresentationDuration="PT1S"><BaseURL>media/</BaseURL><Period><AdaptationSet><SegmentTemplate duration="1" media="seg-$Number$.m4s"></SegmentTemplate><Representation id="a" bandwidth="1"/></AdaptationSet></Period></MPD>"#,
                true,
            ),
            "buf=0 dur=1\nset - - -\n a 1 -x- -: https://example.test/m/media/seg-1.m4s#1@1",
        ),
        (
            (
                r#"<MPD mediaPresentationDuration="PT1S"><BaseURL>https://cdn.test/v/</BaseURL><Period><AdaptationSet><SegmentTemplate duration="1" media="/seg-$Number$.m4s"></SegmentTemplate><Representation id="a" bandwidth="1"/></AdaptationSet></Period></MPD>"#,
                true,
            ),
            "buf=0 dur=1\nset - - -\n a 1 -x- -: https://cdn.test/v/seg-1.m4s#1@1",
        ),
        (
            (
                r#"<MPD mediaPresentationDuration="PT1S"><BaseURL>media/</BaseURL><Period><AdaptationSet><SegmentTemplate duration="1" media="seg-$Number$.m4s"></SegmentTemplate><Representation id="a" bandwidth="1"/></AdaptationSet></Period></MPD>"#,
                false,
            ),
            "buf=0 dur=1\nset - - -\n a 1 -x- -: media/seg-1.m4s#1@1",
        ),
        // 多级 BaseURL：后出现的直接覆盖前面的，而不是逐级拼接
        (
            (
                r#"<MPD mediaPresentationDuration="PT1S"><BaseURL>https://cdn.test/a/</BaseURL><Period><BaseURL>p/</BaseURL><AdaptationSet><SegmentTemplate duration="1" media="seg.m4s"></SegmentTemplate><Representation id="a" bandwidth="1"/></AdaptationSet></Period></MPD>"#,
                true,
            ),
            "buf=0 dur=1\nset - - -\n a 1 -x- -: https://example.test/m/p/seg.m4s#1@1", // BUG? 应为 https://cdn.test/a/p/seg.m4s
        ),
        // 模板本身是绝对地址时仍被拼到 BaseURL 后面
        (
            (
                r#"<MPD mediaPresentationDuration="PT1S"><BaseURL>media/</BaseURL><Period><AdaptationSet><SegmentTemplate duration="1" media="https://other.test/x.m4s"></SegmentTemplate><Representation id="a" bandwidth="1"/></AdaptationSet></Period></MPD>"#,
                true,
            ),
            "buf=0 dur=1\nset - - -\n a 1 -x- -: https://example.test/m/media/https://other.test/x.m4s#1@1", // BUG? 应为 https://other.test/x.m4s
        ),
        // SegmentList、SegmentBase 不支持：Representation 有，分片为空
        (
            (
                r#"<MPD><Period><AdaptationSet><Representation id="a" bandwidth="1"><SegmentList duration="2"><Initialization sourceURL="init.mp4"/><SegmentURL media="a1.m4s"/></SegmentList></Representation></AdaptationSet></Period></MPD>"#,
                false,
            ),
            "buf=0 dur=-\nset - - -\n a 1 -x- -:",
        ),
        (
            (
                r#"<MPD><Period><AdaptationSet><Representation id="a" bandwidth="1"><SegmentBase indexRange="0-99"><Initialization range="0-99"/></SegmentBase></Representation></AdaptationSet></Period></MPD>"#,
                false,
            ),
            "buf=0 dur=-\nset - - -\n a 1 -x- -:",
        ),
        // 上一个 AdaptationSet 的模板会被下一个没有模板的 AdaptationSet 沿用
        (
            (
                r#"<MPD mediaPresentationDuration="PT1S"><Period><AdaptationSet contentType="video"><SegmentTemplate duration="1" media="v-$Number$.m4s"></SegmentTemplate><Representation id="v" bandwidth="1"/></AdaptationSet><AdaptationSet contentType="text"><Representation id="t" bandwidth="1"/></AdaptationSet></Period></MPD>"#,
                false,
            ),
            "buf=0 dur=1\nset - video -\n v 1 -x- -: v-1.m4s#1@1\nset - text -\n t 1 -x- -: v-1.m4s#1@1", // BUG? 第二个 AdaptationSet 不该有分片
        ),
        // 没有 AdaptationSet、不是 MPD
        ((r#"<MPD><Period/></MPD>"#, false), "buf=0 dur=-"),
        ((r#"<html><body/></html>"#, false), "buf=0 dur=-"),
        // XML 写错
        ((r#"<MPD><Period></MPD>"#, false), "Err(XML parsing error)"),
        (
            (r#"<MPD><Period a="1></Period></MPD>"#, false),
            "Err(XML parsing error)",
        ),
    ];
    let base = Url::parse(BASE).unwrap();
    check(
        "parse_mpd",
        CASES,
        |&(xml, with_base)| summary(parse_mpd(xml, with_base.then_some(&base))),
        |a, e| a == e,
    );
}
