use std::collections::BTreeMap;

use anyhow::{Context, Result, bail, ensure};
use url::Url;

use super::xml::{Xml, xml};
use super::{MAX_SEGMENTS, Segment, StreamPlan, Track, target};

pub(super) fn dash(source: &str, base: &Url) -> Result<StreamPlan> {
    let root = xml(source)?;
    ensure!(
        root.name == "MPD" && root.attr("type").unwrap_or("static") == "static",
        "only static DASH MPD is supported"
    );
    let mut pending = vec![&root];
    while let Some(n) = pending.pop() {
        ensure!(
            n.name != "ContentProtection" && n.name != "SegmentBase",
            "encrypted/index-addressed DASH unsupported"
        );
        pending.extend(&n.children);
    }
    let periods: Vec<_> = root
        .children
        .iter()
        .filter(|n| n.name == "Period")
        .collect();
    ensure!(periods.len() == 1, "DASH requires one explicit period");
    let period = periods[0];
    ensure!(
        period
            .attr("start")
            .map(iso_duration)
            .transpose()?
            .is_none_or(|s| s == 0.0),
        "nonzero DASH period start unsupported"
    );
    let presentation_duration = period
        .attr("duration")
        .or_else(|| root.attr("mediaPresentationDuration"))
        .map(iso_duration)
        .transpose()?;
    let base = period.base(&root.base(base)?)?;
    let mut tracks = Vec::new();
    for set in period.children.iter().filter(|n| n.name == "AdaptationSet") {
        let set_base = set.base(&base)?;
        for rep in set.children.iter().filter(|n| n.name == "Representation") {
            ensure!(tracks.len() < 32, "too many DASH representations");
            let rep_base = rep.base(&set_base)?;
            let id = rep.attr("id").context("DASH representation id missing")?;
            ensure!(
                !id.is_empty() && id.len() <= 128,
                "invalid DASH representation id"
            );
            let mime = rep
                .attr("mimeType")
                .or_else(|| set.attr("mimeType"))
                .unwrap_or("application/octet-stream")
                .to_owned();
            let segments = if let Some(list) = rep
                .child("SegmentList")
                .or_else(|| set.child("SegmentList"))
            {
                segment_list(list, &rep_base)?
            } else if let Some(template) = rep
                .child("SegmentTemplate")
                .or_else(|| set.child("SegmentTemplate"))
            {
                let mut template = template.clone();
                if let Some(parent) = set.child("SegmentTemplate") {
                    for (key, value) in &parent.attrs {
                        template
                            .attrs
                            .entry(key.clone())
                            .or_insert_with(|| value.clone());
                    }
                    if template.child("SegmentTimeline").is_none()
                        && let Some(timeline) = parent.child("SegmentTimeline")
                    {
                        template.children.push(timeline.clone());
                    }
                }
                template_segments(
                    &template,
                    &rep_base,
                    id,
                    rep.attr("bandwidth"),
                    presentation_duration,
                )?
            } else {
                bail!("DASH representation lacks supported segments");
            };
            ensure!(
                segments.iter().any(|s| !s.initialization),
                "empty DASH representation"
            );
            tracks.push(Track {
                id: id.into(),
                mime,
                segments,
            });
        }
    }
    ensure!(!tracks.is_empty(), "DASH has no supported representations");
    ensure!(
        tracks.iter().map(|t| t.segments.len()).sum::<usize>() <= MAX_SEGMENTS,
        "DASH total segment budget exceeded"
    );
    Ok(StreamPlan::Vod {
        tracks,
        manifest: Some(source.to_owned()),
    })
}
pub(super) fn dash_range(v: &str) -> Result<(u64, u64)> {
    let (start, end) = v.split_once('-').context("invalid DASH byte range")?;
    let (start, end) = (start.parse()?, end.parse()?);
    ensure!(start <= end, "inverted DASH range");
    Ok((start, end))
}
pub(super) fn segment_list(list: &Xml, base: &Url) -> Result<Vec<Segment>> {
    let mut segments = Vec::new();
    if let Some(init) = list.child("Initialization") {
        segments.push(Segment {
            url: target(base, init.attr("sourceURL").unwrap_or(base.as_str()))?,
            range: init.attr("range").map(dash_range).transpose()?,
            initialization: true,
        });
    }
    for n in list.children.iter().filter(|n| n.name == "SegmentURL") {
        ensure!(
            segments.len() < MAX_SEGMENTS,
            "DASH segment budget exceeded"
        );
        ensure!(
            n.attr("index").is_none() && n.attr("indexRange").is_none(),
            "DASH segment indexes unsupported"
        );
        segments.push(Segment {
            url: target(base, n.attr("media").context("DASH media missing")?)?,
            range: n.attr("mediaRange").map(dash_range).transpose()?,
            initialization: false,
        });
    }
    Ok(segments)
}
pub(super) fn template_segments(
    t: &Xml,
    base: &Url,
    id: &str,
    bandwidth: Option<&str>,
    duration: Option<f64>,
) -> Result<Vec<Segment>> {
    ensure!(
        t.attr("eptDelta").is_none() && t.attr("index").is_none(),
        "DASH timing/index extension unsupported"
    );
    let media = t.attr("media").context("DASH media template missing")?;
    let timescale: u64 = t.attr("timescale").unwrap_or("1").parse()?;
    ensure!(timescale > 0, "DASH timescale must be positive");
    let mut number: u64 = t.attr("startNumber").unwrap_or("1").parse()?;
    let mut time = 0_u64;
    let mut segments = Vec::new();
    if let Some(init) = t.attr("initialization") {
        segments.push(Segment {
            url: target(base, &expand(init, id, bandwidth, number, time)?)?,
            range: None,
            initialization: true,
        });
    }
    let generated;
    let entries = if let Some(timeline) = t.child("SegmentTimeline") {
        timeline
            .children
            .iter()
            .filter(|n| n.name == "S")
            .collect::<Vec<_>>()
    } else {
        let d: u64 = t
            .attr("duration")
            .context("DASH template needs duration or timeline")?
            .parse()?;
        ensure!(d > 0, "DASH duration must be positive");
        let count = (duration.context("DASH numbered template needs finite period duration")?
            * timescale as f64
            / d as f64)
            .ceil();
        ensure!(
            count.is_finite() && count > 0.0 && count <= MAX_SEGMENTS as f64,
            "DASH duration segment budget exceeded"
        );
        ensure!(
            t.attr("presentationTimeOffset").is_none_or(|s| s == "0"),
            "numbered DASH timing offset unsupported"
        );
        generated = Xml {
            attrs: BTreeMap::from([
                ("t".into(), "0".into()),
                ("d".into(), d.to_string()),
                ("r".into(), ((count as u64) - 1).to_string()),
            ]),
            ..Default::default()
        };
        vec![&generated]
    };
    for s in entries {
        if let Some(value) = s.attr("t") {
            time = value.parse()?;
        }
        let d: u64 = s
            .attr("d")
            .context("DASH timeline duration missing")?
            .parse()?;
        ensure!(d > 0, "empty DASH duration");
        let repeat: i64 = s.attr("r").unwrap_or("0").parse()?;
        ensure!(
            (0..MAX_SEGMENTS as i64).contains(&repeat),
            "unbounded or excessive DASH repetition"
        );
        for _ in 0..=repeat {
            ensure!(
                segments.len() < MAX_SEGMENTS,
                "DASH segment budget exceeded"
            );
            segments.push(Segment {
                url: target(base, &expand(media, id, bandwidth, number, time)?)?,
                range: None,
                initialization: false,
            });
            number = number.checked_add(1).context("DASH number overflow")?;
            time = time.checked_add(d).context("DASH time overflow")?;
        }
    }
    Ok(segments)
}
pub(super) fn iso_duration(value: &str) -> Result<f64> {
    let mut rest = value
        .strip_prefix("PT")
        .context("only DASH PT hour/minute/second durations are supported")?;
    let mut total = 0.0;
    let mut previous = 0;
    while !rest.is_empty() {
        let end = rest
            .find(|c: char| c.is_ascii_alphabetic())
            .context("invalid DASH duration")?;
        let n: f64 = rest[..end].parse()?;
        ensure!(n.is_finite() && n >= 0.0, "invalid DASH duration number");
        let (order, multiplier) = match rest.as_bytes()[end] {
            b'H' => (1, 3600.0),
            b'M' => (2, 60.0),
            b'S' => (3, 1.0),
            _ => bail!("unsupported DASH duration unit"),
        };
        ensure!(order > previous, "DASH duration units out of order");
        previous = order;
        total += n * multiplier;
        rest = &rest[end + 1..];
    }
    ensure!(
        previous > 0 && total.is_finite() && total >= 0.0,
        "empty DASH duration"
    );
    Ok(total)
}
pub(super) fn expand(
    template: &str,
    id: &str,
    bandwidth: Option<&str>,
    number: u64,
    time: u64,
) -> Result<String> {
    let mut out = String::new();
    let mut rest = template;
    while let Some((prefix, tail)) = rest.split_once('$') {
        out.push_str(prefix);
        if let Some(next) = tail.strip_prefix('$') {
            out.push('$');
            rest = next;
            continue;
        }
        let (key, next) = tail.split_once('$').context("unclosed DASH template")?;
        let (key, width) = match key.split_once("%0") {
            Some((k, w)) => (
                k,
                Some(
                    w.strip_suffix('d')
                        .context("invalid DASH format")?
                        .parse::<usize>()?,
                ),
            ),
            None => (key, None),
        };
        ensure!(
            width.is_none_or(|w| (1..=16).contains(&w)),
            "DASH template width exceeds budget"
        );
        let value = match key {
            "RepresentationID" => {
                ensure!(width.is_none(), "representation formatting unsupported");
                id.to_owned()
            }
            "Bandwidth" => bandwidth.context("DASH bandwidth missing")?.into(),
            "Number" => number.to_string(),
            "Time" => time.to_string(),
            _ => bail!("unknown DASH placeholder"),
        };
        if let Some(w) = width {
            out.push_str(&format!("{value:0>width$}", width = w));
        } else {
            out.push_str(&value);
        }
        rest = next;
        ensure!(out.len() <= 8192, "DASH expanded URL exceeds budget");
    }
    out.push_str(rest);
    Ok(out)
}
