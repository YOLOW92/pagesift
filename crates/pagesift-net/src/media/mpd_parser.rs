//! Pure Safe Rust MPEG-DASH MPD Manifest Parser.
//! Parses AdaptationSets, Representations, SegmentTemplates, and Timelines.

use quick_xml::events::Event;
use quick_xml::reader::Reader;
use url::Url;

/// Segment in a DASH representation.
#[derive(Clone, Debug, PartialEq)]
pub struct DashSegment {
    pub uri: String,
    pub duration_s: f64,
    pub number: u64,
    pub is_init: bool,
}

/// A specific quality representation in DASH.
#[derive(Clone, Debug, PartialEq)]
pub struct DashRepresentation {
    pub id: String,
    pub bandwidth: u64,
    pub width: Option<u32>,
    pub height: Option<u32>,
    pub codecs: Option<String>,
    pub segments: Vec<DashSegment>,
}

/// Adaptation set containing multiple representations (e.g. video, audio).
#[derive(Clone, Debug, PartialEq)]
pub struct DashAdaptationSet {
    pub id: Option<String>,
    pub content_type: Option<String>,
    pub mime_type: Option<String>,
    pub representations: Vec<DashRepresentation>,
}

/// Parsed DASH manifest representation.
#[derive(Clone, Debug, PartialEq)]
pub struct DashManifest {
    pub min_buffer_time_s: f64,
    pub media_presentation_duration_s: Option<f64>,
    pub adaptation_sets: Vec<DashAdaptationSet>,
}

#[derive(Debug, thiserror::Error)]
pub enum DashError {
    #[error("XML parsing error: {0}")]
    Xml(String),
    #[error("relative URI resolution error: {0}")]
    UriResolution(String),
    #[error("invalid duration or number in manifest: {0}")]
    InvalidValue(String),
}

#[derive(Clone, Default)]
struct SegmentTemplateInfo {
    timescale: u64,
    initialization: Option<String>,
    media: Option<String>,
    start_number: u64,
    duration: Option<u64>,
    timeline: Vec<(u64, u64, u64)>, // (time, duration, repeat)
}

fn replace_template(
    template: &str,
    rep_id: &str,
    number: u64,
    bandwidth: u64,
    time: u64,
) -> String {
    let mut result = template
        .replace("$RepresentationID$", rep_id)
        .replace("$Bandwidth$", &bandwidth.to_string());

    if let Some(idx) = result.find("$Number") {
        if let Some(end) = result[idx + 1..].find('$') {
            let tag = result[idx..=idx + 1 + end].to_string();
            let formatted = if tag.contains("%0") {
                if let Some(width_str) = tag
                    .strip_prefix("$Number%0")
                    .and_then(|s| s.strip_suffix("d$"))
                {
                    if let Ok(width) = width_str.parse::<usize>() {
                        format!("{:0width$}", number, width = width)
                    } else {
                        number.to_string()
                    }
                } else {
                    number.to_string()
                }
            } else {
                number.to_string()
            };
            result = result.replace(&tag, &formatted);
        }
    }

    result = result.replace("$Time$", &time.to_string());
    result
}

fn parse_iso8601_duration(s: &str) -> Option<f64> {
    let s = s.strip_prefix("PT")?;
    let mut total = 0.0;
    let mut cur = String::new();

    for c in s.chars() {
        if c.is_ascii_digit() || c == '.' {
            cur.push(c);
        } else {
            let val: f64 = cur.parse().ok()?;
            cur.clear();
            match c {
                'H' => total += val * 3600.0,
                'M' => total += val * 60.0,
                'S' => total += val,
                _ => {}
            }
        }
    }
    Some(total)
}

fn finalize_representation(
    mut rep: DashRepresentation,
    cur_template: &SegmentTemplateInfo,
    global_base_url: Option<&str>,
    base_url: Option<&Url>,
    media_presentation_duration_s: Option<f64>,
) -> DashRepresentation {
    let timescale = cur_template.timescale.max(1) as f64;
    let effective_base = global_base_url.unwrap_or("");
    let resolve = |raw: &str| -> String {
        let merged = if effective_base.is_empty() {
            raw.to_string()
        } else {
            format!(
                "{}/{}",
                effective_base.trim_end_matches('/'),
                raw.trim_start_matches('/')
            )
        };
        match base_url {
            Some(b) => b.join(&merged).map(|u| u.to_string()).unwrap_or(merged),
            None => merged,
        }
    };

    if let Some(ref init_pattern) = cur_template.initialization {
        let init_uri = replace_template(init_pattern, &rep.id, 0, rep.bandwidth, 0);
        rep.segments.push(DashSegment {
            uri: resolve(&init_uri),
            duration_s: 0.0,
            number: 0,
            is_init: true,
        });
    }

    if let Some(ref media_pattern) = cur_template.media {
        if !cur_template.timeline.is_empty() {
            let mut seg_num = cur_template.start_number;
            let mut cur_time = 0u64;
            for (t, d, r) in &cur_template.timeline {
                if *t > 0 {
                    cur_time = *t;
                }
                let count = r + 1;
                for _ in 0..count {
                    let seg_uri =
                        replace_template(media_pattern, &rep.id, seg_num, rep.bandwidth, cur_time);
                    rep.segments.push(DashSegment {
                        uri: resolve(&seg_uri),
                        duration_s: *d as f64 / timescale,
                        number: seg_num,
                        is_init: false,
                    });
                    cur_time += d;
                    seg_num += 1;
                }
            }
        } else if let Some(dur) = cur_template.duration {
            let total_duration = media_presentation_duration_s.unwrap_or(60.0);
            let seg_dur_s = dur as f64 / timescale;
            let num_segs = if seg_dur_s > 0.0 {
                (total_duration / seg_dur_s).ceil() as u64
            } else {
                1
            };
            for i in 0..num_segs {
                let seg_num = cur_template.start_number + i;
                let cur_time = i * dur;
                let seg_uri =
                    replace_template(media_pattern, &rep.id, seg_num, rep.bandwidth, cur_time);
                rep.segments.push(DashSegment {
                    uri: resolve(&seg_uri),
                    duration_s: seg_dur_s,
                    number: seg_num,
                    is_init: false,
                });
            }
        }
    }

    rep
}

fn parse_rep_attrs(e: &quick_xml::events::BytesStart<'_>) -> DashRepresentation {
    let mut rep = DashRepresentation {
        id: String::new(),
        bandwidth: 0,
        width: None,
        height: None,
        codecs: None,
        segments: Vec::new(),
    };
    for attr in e.attributes().flatten() {
        let k = attr.key.as_ref();
        let v = attr.value.as_ref();
        if k == "id" {
            rep.id = v.to_string();
        } else if k == "bandwidth" {
            rep.bandwidth = v.parse().unwrap_or(0);
        } else if k == "width" {
            rep.width = v.parse().ok();
        } else if k == "height" {
            rep.height = v.parse().ok();
        } else if k == "codecs" {
            rep.codecs = Some(v.to_string());
        }
    }
    rep
}

/// Pure Safe Rust streaming parser for MPEG-DASH MPD manifest.
pub fn parse_mpd(xml_content: &str, base_url: Option<&Url>) -> Result<DashManifest, DashError> {
    let mut reader = Reader::from_str(xml_content);
    reader.config_mut().trim_text(true);

    let mut adaptation_sets = Vec::new();
    let mut cur_adapt: Option<DashAdaptationSet> = None;
    let mut cur_rep: Option<DashRepresentation> = None;
    let mut cur_template = SegmentTemplateInfo::default();
    let mut global_base_url: Option<String> = None;
    let mut in_base_url = false;
    let mut in_timeline = false;
    let mut min_buffer_time_s = 0.0;
    let mut media_presentation_duration_s = None;

    loop {
        match reader.read_event() {
            Ok(Event::Start(ref e)) => {
                let name = e.local_name();
                match name.as_ref() {
                    "MPD" => {
                        for attr in e.attributes().flatten() {
                            let key = attr.key.as_ref();
                            let val = attr.value.as_ref();
                            if key == "minBufferTime" {
                                min_buffer_time_s = parse_iso8601_duration(val).unwrap_or(0.0);
                            } else if key == "mediaPresentationDuration" {
                                media_presentation_duration_s = parse_iso8601_duration(val);
                            }
                        }
                    }
                    "BaseURL" => in_base_url = true,
                    "AdaptationSet" => {
                        let mut adapt = DashAdaptationSet {
                            id: None,
                            content_type: None,
                            mime_type: None,
                            representations: Vec::new(),
                        };
                        for attr in e.attributes().flatten() {
                            let k = attr.key.as_ref();
                            let v = attr.value.as_ref().to_string();
                            if k == "id" {
                                adapt.id = Some(v);
                            } else if k == "contentType" {
                                adapt.content_type = Some(v);
                            } else if k == "mimeType" {
                                adapt.mime_type = Some(v);
                            }
                        }
                        cur_adapt = Some(adapt);
                    }
                    "Representation" => {
                        cur_rep = Some(parse_rep_attrs(e));
                    }
                    "SegmentTemplate" => {
                        let mut tmpl = SegmentTemplateInfo {
                            timescale: 1,
                            start_number: 1,
                            ..Default::default()
                        };
                        for attr in e.attributes().flatten() {
                            let k = attr.key.as_ref();
                            let v = attr.value.as_ref();
                            if k == "timescale" {
                                tmpl.timescale = v.parse().unwrap_or(1);
                            } else if k == "initialization" {
                                tmpl.initialization = Some(v.to_string());
                            } else if k == "media" {
                                tmpl.media = Some(v.to_string());
                            } else if k == "startNumber" {
                                tmpl.start_number = v.parse().unwrap_or(1);
                            } else if k == "duration" {
                                tmpl.duration = v.parse().ok();
                            }
                        }
                        cur_template = tmpl;
                    }
                    "SegmentTimeline" => in_timeline = true,
                    _ => {}
                }
            }
            Ok(Event::Empty(ref e)) => {
                let name = e.local_name();
                if name.as_ref() == "S" && in_timeline {
                    let mut t = 0u64;
                    let mut d = 0u64;
                    let mut r = 0u64;
                    for attr in e.attributes().flatten() {
                        let k = attr.key.as_ref();
                        let v = attr.value.as_ref();
                        if k == "t" {
                            t = v.parse().unwrap_or(0);
                        } else if k == "d" {
                            d = v.parse().unwrap_or(0);
                        } else if k == "r" {
                            r = v.parse().unwrap_or(0);
                        }
                    }
                    cur_template.timeline.push((t, d, r));
                } else if name.as_ref() == "Representation" {
                    let rep = parse_rep_attrs(e);
                    let finalized = finalize_representation(
                        rep,
                        &cur_template,
                        global_base_url.as_deref(),
                        base_url,
                        media_presentation_duration_s,
                    );
                    if let Some(ref mut adapt) = cur_adapt {
                        adapt.representations.push(finalized);
                    }
                }
            }
            Ok(Event::Text(ref e)) => {
                if in_base_url {
                    global_base_url = Some(e.as_ref().to_string());
                }
            }
            Ok(Event::End(ref e)) => {
                let name = e.local_name();
                match name.as_ref() {
                    "BaseURL" => in_base_url = false,
                    "SegmentTimeline" => in_timeline = false,
                    "Representation" => {
                        if let Some(rep) = cur_rep.take() {
                            let finalized = finalize_representation(
                                rep,
                                &cur_template,
                                global_base_url.as_deref(),
                                base_url,
                                media_presentation_duration_s,
                            );
                            if let Some(ref mut adapt) = cur_adapt {
                                adapt.representations.push(finalized);
                            }
                        }
                    }
                    "AdaptationSet" => {
                        if let Some(adapt) = cur_adapt.take() {
                            adaptation_sets.push(adapt);
                        }
                    }
                    _ => {}
                }
            }
            Ok(Event::Eof) => break,
            Err(e) => return Err(DashError::Xml(e.to_string())),
            _ => {}
        }
    }

    Ok(DashManifest {
        min_buffer_time_s,
        media_presentation_duration_s,
        adaptation_sets,
    })
}
