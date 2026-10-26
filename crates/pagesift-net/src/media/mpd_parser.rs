//! Pure Safe Rust MPEG-DASH MPD Manifest Parser.
//! Parses AdaptationSets, Representations, SegmentTemplates, and Timelines.

use quick_xml::events::{BytesStart, Event};
use quick_xml::reader::Reader;
use url::Url;

use super::mpd_template::join;
pub(super) use super::mpd_template::{parse_iso8601_duration, replace_template};

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

/// Most segments generated for one representation; more is treated as a
/// malformed or hostile manifest rather than allocated.
const MAX_SEGMENTS: u64 = 200_000;

/// One `<S>` entry of a SegmentTimeline (`r < 0` repeats up to the next `t`).
#[derive(Clone, Copy)]
struct TimelineEntry {
    t: Option<u64>,
    d: u64,
    r: i64,
}

/// SegmentTemplate attributes declared at one level. Lower levels inherit the
/// attributes and the timeline they omit (ISO/IEC 23009-1 §5.3.9.1).
#[derive(Clone, Default)]
struct Template {
    timescale: Option<u64>,
    initialization: Option<String>,
    media: Option<String>,
    start_number: Option<u64>,
    duration: Option<u64>,
    presentation_time_offset: Option<u64>,
    timeline: Option<Vec<TimelineEntry>>,
}

impl Template {
    fn parse(e: &BytesStart<'_>) -> Self {
        let mut t = Template::default();
        for (k, v) in attrs(e) {
            match k.as_str() {
                "timescale" => t.timescale = v.parse().ok(),
                "initialization" => t.initialization = Some(v),
                "media" => t.media = Some(v),
                "startNumber" => t.start_number = v.parse().ok(),
                "duration" => t.duration = v.parse().ok(),
                "presentationTimeOffset" => t.presentation_time_offset = v.parse().ok(),
                _ => {}
            }
        }
        t
    }

    fn inherit(mut self, parent: Option<&Template>) -> Self {
        let Some(p) = parent else { return self };
        self.timescale = self.timescale.or(p.timescale);
        self.initialization = self.initialization.or_else(|| p.initialization.clone());
        self.media = self.media.or_else(|| p.media.clone());
        self.start_number = self.start_number.or(p.start_number);
        self.duration = self.duration.or(p.duration);
        self.presentation_time_offset =
            self.presentation_time_offset.or(p.presentation_time_offset);
        self.timeline = self.timeline.or_else(|| p.timeline.clone());
        self
    }
}

/// Element levels that can carry a BaseURL or a SegmentTemplate.
const MPD: usize = 0;
const PERIOD: usize = 1;
const SET: usize = 2;
const REP: usize = 3;

#[derive(Clone, Copy, Default)]
struct PeriodInfo {
    start: Option<f64>,
    duration: Option<f64>,
}

/// A representation whose segments are listed once all Period timing is known.
struct Pending {
    set: usize,
    rep: usize,
    period: Option<usize>,
    template: Option<Template>,
    base: Option<String>,
}

fn attrs<'a>(e: &'a BytesStart<'_>) -> impl Iterator<Item = (String, String)> + 'a {
    e.attributes().flatten().map(|a| {
        let value = a
            .normalized_value(quick_xml::XmlVersion::Implicit1_0)
            .map(|v| v.into_owned())
            .unwrap_or_else(|_| a.value.as_ref().to_owned());
        (a.key.local_name().as_ref().to_owned(), value)
    })
}

fn parse_rep_attrs(e: &BytesStart<'_>) -> DashRepresentation {
    let mut rep = DashRepresentation {
        id: String::new(),
        bandwidth: 0,
        width: None,
        height: None,
        codecs: None,
        segments: Vec::new(),
    };
    for (k, v) in attrs(e) {
        match k.as_str() {
            "id" => rep.id = v,
            "bandwidth" => rep.bandwidth = v.parse().unwrap_or(0),
            "width" => rep.width = v.parse().ok(),
            "height" => rep.height = v.parse().ok(),
            "codecs" => rep.codecs = Some(v),
            _ => {}
        }
    }
    rep
}

/// Lists a representation's segments. Number-based templates need the Period
/// duration; without one (e.g. a live MPD) only the initialization segment is
/// listed instead of inventing a segment count.
fn segments(
    rep: &DashRepresentation,
    template: &Template,
    base: Option<&str>,
    period_duration: Option<f64>,
) -> Result<Vec<DashSegment>, DashError> {
    let timescale = template.timescale.unwrap_or(1).max(1);
    let start_number = template.start_number.unwrap_or(1);
    let resolve = |raw: &str| join(base, raw);
    let mut out = Vec::new();
    if let Some(init) = &template.initialization {
        out.push(DashSegment {
            uri: resolve(&replace_template(init, &rep.id, 0, rep.bandwidth, 0)),
            duration_s: 0.0,
            number: 0,
            is_init: true,
        });
    }
    let Some(media) = &template.media else {
        return Ok(out);
    };
    let too_many = || DashError::InvalidValue(format!("more than {MAX_SEGMENTS} segments"));
    let push = |out: &mut Vec<DashSegment>, number: u64, time: u64, duration_s: f64| {
        out.push(DashSegment {
            uri: resolve(&replace_template(
                media,
                &rep.id,
                number,
                rep.bandwidth,
                time,
            )),
            duration_s,
            number,
            is_init: false,
        });
    };
    if let Some(timeline) = &template.timeline {
        let pto = template.presentation_time_offset.unwrap_or(0);
        let period_end =
            period_duration.map(|d| pto.saturating_add((d * timescale as f64).round() as u64));
        let mut number = start_number;
        let mut time = 0u64;
        let mut listed = 0u64;
        for (i, s) in timeline.iter().enumerate() {
            if let Some(t) = s.t {
                time = t;
            }
            let count = if s.r >= 0 {
                s.r as u64 + 1
            } else {
                let end = timeline.get(i + 1).and_then(|n| n.t).or(period_end);
                match end {
                    Some(end) if end > time && s.d > 0 => (end - time).div_ceil(s.d),
                    _ => 1,
                }
            };
            listed = listed.saturating_add(count);
            if listed > MAX_SEGMENTS {
                return Err(too_many());
            }
            for _ in 0..count {
                push(&mut out, number, time, s.d as f64 / timescale as f64);
                time = time.saturating_add(s.d);
                number += 1;
            }
        }
    } else if let (Some(dur), Some(total)) = (template.duration.filter(|&d| d > 0), period_duration)
    {
        let seg_s = dur as f64 / timescale as f64;
        // The tolerance keeps 6.000000001 segments from becoming 7.
        let count = (total / seg_s - 1e-9).ceil().max(0.0);
        if count > MAX_SEGMENTS as f64 {
            return Err(too_many());
        }
        for i in 0..count as u64 {
            push(&mut out, start_number + i, i * dur, seg_s);
        }
    }
    Ok(out)
}

/// Start, duration of each Period (§5.3.2.1): a missing start follows the previous
/// Period, a missing duration runs to the next start or the presentation end.
fn period_durations(periods: &[PeriodInfo], total: Option<f64>) -> Vec<Option<f64>> {
    let mut out = Vec::with_capacity(periods.len());
    let mut prev_end = Some(0.0);
    for (i, p) in periods.iter().enumerate() {
        let start = p.start.or(prev_end);
        let end = match periods.get(i + 1) {
            Some(next) => next.start,
            None => total,
        };
        let duration = p.duration.or_else(|| {
            let (s, e) = (start?, end?);
            (e >= s).then_some(e - s)
        });
        prev_end = start.zip(duration).map(|(s, d)| s + d);
        out.push(duration);
    }
    out
}

/// Parser state while walking the MPD.
#[derive(Default)]
struct State {
    /// The manifest's own address, the base of the outermost BaseURL.
    root: Option<String>,
    level: usize,
    /// Resolved BaseURL per level (only the first BaseURL of a level is used).
    bases: [Option<String>; 4],
    templates: [Option<Template>; 4],
    template_level: Option<usize>,
    base_text: Option<String>,
    min_buffer_time_s: f64,
    duration_s: Option<f64>,
    periods: Vec<PeriodInfo>,
    sets: Vec<DashAdaptationSet>,
    cur_set: Option<DashAdaptationSet>,
    cur_rep: Option<DashRepresentation>,
    pending: Vec<Pending>,
}

impl State {
    fn enter(&mut self, level: usize) {
        self.level = level;
        for l in level..=REP {
            self.bases[l] = None;
            self.templates[l] = None;
        }
    }

    fn open(&mut self, e: &BytesStart<'_>) {
        match e.local_name().as_ref() {
            "MPD" => {
                self.enter(MPD);
                for (k, v) in attrs(e) {
                    match k.as_str() {
                        "minBufferTime" => {
                            self.min_buffer_time_s = parse_iso8601_duration(&v).unwrap_or(0.0)
                        }
                        "mediaPresentationDuration" => self.duration_s = parse_iso8601_duration(&v),
                        _ => {}
                    }
                }
            }
            "Period" => {
                self.enter(PERIOD);
                let mut p = PeriodInfo::default();
                for (k, v) in attrs(e) {
                    match k.as_str() {
                        "start" => p.start = parse_iso8601_duration(&v),
                        "duration" => p.duration = parse_iso8601_duration(&v),
                        _ => {}
                    }
                }
                self.periods.push(p);
            }
            "AdaptationSet" => {
                self.enter(SET);
                let mut set = DashAdaptationSet {
                    id: None,
                    content_type: None,
                    mime_type: None,
                    representations: Vec::new(),
                };
                for (k, v) in attrs(e) {
                    match k.as_str() {
                        "id" => set.id = Some(v),
                        "contentType" => set.content_type = Some(v),
                        "mimeType" => set.mime_type = Some(v),
                        _ => {}
                    }
                }
                self.cur_set = Some(set);
            }
            "Representation" => {
                self.enter(REP);
                self.cur_rep = Some(parse_rep_attrs(e));
            }
            "BaseURL" => self.base_text = Some(String::new()),
            "SegmentTemplate" => {
                self.templates[self.level] = Some(Template::parse(e));
                self.template_level = Some(self.level);
            }
            "SegmentTimeline" => {
                if let Some(t) = self.open_template() {
                    t.timeline = Some(Vec::new());
                }
            }
            "S" => {
                let mut s = TimelineEntry {
                    t: None,
                    d: 0,
                    r: 0,
                };
                for (k, v) in attrs(e) {
                    match k.as_str() {
                        "t" => s.t = v.parse().ok(),
                        "d" => s.d = v.parse().unwrap_or(0),
                        "r" => s.r = v.parse().unwrap_or(0),
                        _ => {}
                    }
                }
                if let Some(timeline) = self.open_template().and_then(|t| t.timeline.as_mut()) {
                    timeline.push(s);
                }
            }
            _ => {}
        }
    }

    fn open_template(&mut self) -> Option<&mut Template> {
        self.templates[self.template_level?].as_mut()
    }

    fn close(&mut self, name: &str) {
        match name {
            "BaseURL" => {
                if let Some(text) = self.base_text.take()
                    && self.bases[self.level].is_none()
                {
                    let parent = self.base_above(self.level);
                    self.bases[self.level] = Some(join(parent, text.trim()));
                }
            }
            "SegmentTemplate" => self.template_level = None,
            "Representation" => {
                self.finish_representation();
                self.level = SET;
            }
            "AdaptationSet" => {
                if let Some(set) = self.cur_set.take() {
                    self.sets.push(set);
                }
                self.level = PERIOD;
            }
            "Period" => self.level = MPD,
            _ => {}
        }
    }

    /// The nearest resolved BaseURL above `level`, else the manifest's address.
    fn base_above(&self, level: usize) -> Option<&str> {
        self.bases[..level]
            .iter()
            .rev()
            .flatten()
            .next()
            .or(self.root.as_ref())
            .map(String::as_str)
    }

    fn finish_representation(&mut self) {
        let base = self.base_above(REP + 1).map(str::to_owned);
        let (Some(rep), Some(set)) = (self.cur_rep.take(), self.cur_set.as_mut()) else {
            return;
        };
        let template = self.templates[PERIOD..=REP]
            .iter()
            .flatten()
            .fold(None, |parent: Option<Template>, t| {
                Some(t.clone().inherit(parent.as_ref()))
            });
        self.pending.push(Pending {
            set: self.sets.len(),
            rep: set.representations.len(),
            period: self.periods.len().checked_sub(1),
            template,
            base,
        });
        set.representations.push(rep);
    }
}

/// Pure Safe Rust streaming parser for MPEG-DASH MPD manifest.
pub fn parse_mpd(xml_content: &str, base_url: Option<&Url>) -> Result<DashManifest, DashError> {
    let mut reader = Reader::from_str(xml_content);
    let mut st = State {
        root: base_url.map(Url::to_string),
        ..State::default()
    };

    loop {
        match reader.read_event() {
            Ok(Event::Start(ref e)) => st.open(e),
            Ok(Event::Empty(ref e)) => {
                st.open(e);
                st.close(e.local_name().as_ref());
            }
            Ok(Event::End(ref e)) => st.close(e.local_name().as_ref()),
            Ok(Event::Text(ref e)) => {
                if let Some(text) = st.base_text.as_mut() {
                    text.push_str(&e.xml10_content());
                }
            }
            Ok(Event::GeneralRef(ref e)) => {
                if let Some(text) = st.base_text.as_mut() {
                    let entity = format!("&{};", &**e);
                    let value = quick_xml::escape::unescape(&entity)
                        .map_err(|e| DashError::Xml(e.to_string()))?;
                    text.push_str(&value);
                }
            }
            Ok(Event::Eof) => break,
            Err(e) => return Err(DashError::Xml(e.to_string())),
            _ => {}
        }
    }

    let durations = period_durations(&st.periods, st.duration_s);
    for p in &st.pending {
        let Some(template) = &p.template else {
            continue;
        };
        let period_duration = match p.period {
            Some(i) => durations[i],
            None => st.duration_s,
        };
        // An unclosed AdaptationSet at end of input never reached `sets`.
        let Some(rep) = st
            .sets
            .get_mut(p.set)
            .map(|s| &mut s.representations[p.rep])
        else {
            continue;
        };
        rep.segments = segments(rep, template, p.base.as_deref(), period_duration)?;
    }

    Ok(DashManifest {
        min_buffer_time_s: st.min_buffer_time_s,
        media_presentation_duration_s: st.duration_s,
        adaptation_sets: st.sets,
    })
}
#[cfg(test)]
#[path = "tests_mpd.rs"]
mod tests;
