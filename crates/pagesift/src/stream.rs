//! Native bounded HLS/DASH VOD planning. Unsupported live/encrypted/index-based
//! formats fail explicitly. Planning does not execute a player or a helper tool.
use std::collections::BTreeMap;

use anyhow::{Context, Result, bail, ensure};
use quick_xml::events::Event;
use serde::Serialize;
use url::Url;

const MAX_SEGMENTS: usize = 4096;
#[derive(Clone, Debug, Serialize)]
pub struct Segment {
    pub url: String,
    pub range: Option<(u64, u64)>,
    pub initialization: bool,
}
#[derive(Clone, Debug, Serialize)]
pub struct Track {
    pub id: String,
    pub mime: String,
    pub segments: Vec<Segment>,
}
#[derive(Clone, Debug, Serialize)]
pub struct Variant {
    pub url: String,
    pub bandwidth: u64,
    pub codecs: Option<String>,
}
#[derive(Clone, Debug, Serialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum StreamPlan {
    Master {
        variants: Vec<Variant>,
    },
    Vod {
        tracks: Vec<Track>,
        /// Original DASH timing, retained for an offline MPD. Raw per-track
        /// concatenation alone cannot preserve presentation boundaries.
        manifest: Option<String>,
    },
}

/// Download a finite, unencrypted VOD into separate track files. Completed
/// segments reuse the validated archive; an interrupted segment uses its ETag
/// checkpoint. This concatenates each track, and never claims A/V remuxing.
pub async fn archive_stream(
    policy: &pagesift_policy::Policy,
    plan: &StreamPlan,
    source: &Url,
    directory: &std::path::Path,
    max_bytes: u64,
) -> Result<Vec<crate::postprocess::PublishedMedia>> {
    use crate::download::{Archive, DownloadRequest, DownloadResult};
    use crate::media::MediaItem;
    use std::fs::{self, File, OpenOptions};
    use std::io::{Read, Write};
    ensure!(
        (1..=2 * 1024 * 1024 * 1024).contains(&max_bytes),
        "stream byte budget must be 1..=2 GiB"
    );
    let StreamPlan::Vod { tracks, manifest } = plan else {
        bail!("choose an explicit media variant before downloading");
    };
    ensure!(
        !tracks.is_empty() && tracks.len() <= 32,
        "invalid stream tracks"
    );
    ensure!(
        tracks.iter().map(|t| t.segments.len()).sum::<usize>() <= MAX_SEGMENTS,
        "too many stream segments"
    );
    ensure!(
        tracks.iter().all(|t| !t.segments.is_empty()),
        "empty stream track"
    );
    let encoded = serde_json::to_vec(plan)?;
    let revision = blake3::hash(&encoded).to_hex().to_string();
    fs::create_dir_all(directory)?;
    let mut archive = Archive::open(directory.join("segments"))?;
    let mut total = 0_u64;
    let mut downloaded = Vec::new();
    for (ti, track) in tracks.iter().enumerate() {
        let mut files = Vec::new();
        for (si, segment) in track.segments.iter().enumerate() {
            ensure!(total < max_bytes, "stream total byte budget exhausted");
            let url = Url::parse(&target(source, &segment.url)?)?;
            let request = DownloadRequest {
                source: source.to_string(),
                id: format!("track:{ti}:segment:{si}"),
                revision: revision.clone(),
                url,
                max_bytes: (max_bytes - total).min(64 * 1024 * 1024),
                expected_blake3: None,
                restart: false,
            };
            let file = if let Some((start, end)) = segment.range {
                archive.download_range(policy, &request, start, end).await?
            } else {
                archive.download(policy, &request).await?
            };
            ensure!(file.bytes > 0, "empty stream segment");
            total = total
                .checked_add(file.bytes)
                .context("stream byte count overflow")?;
            files.push(file);
        }
        downloaded.push(files);
    }
    let offline = manifest
        .as_ref()
        .map(|source| offline_dash(source, tracks, &downloaded, directory, &revision))
        .transpose()?;
    let mut published = Vec::new();
    // DASH playback uses the offline MPD and shared cached assets. Avoid extra
    // whole-track copies, whose timestamps alone do not express the MPD window.
    if offline.is_none() {
        for (ti, (track, files)) in tracks.iter().zip(downloaded).enumerate() {
            let joined = directory.join(format!("track-{revision}-{ti}.part"));
            if let Ok(meta) = fs::symlink_metadata(&joined) {
                ensure!(
                    meta.file_type().is_file(),
                    "track staging path must be a regular file"
                );
            }
            let mut options = OpenOptions::new();
            options.create(true).truncate(true).write(true);
            #[cfg(unix)]
            {
                use std::os::unix::fs::OpenOptionsExt;
                options.mode(0o600);
            }
            let mut output = options.open(&joined)?;
            let mut hash = blake3::Hasher::new();
            let mut bytes = 0_u64;
            for segment in files {
                let mut input = File::open(&segment.path)?;
                let mut buffer = [0; 64 * 1024];
                let mut checked = blake3::Hasher::new();
                let mut count = 0_u64;
                loop {
                    let n = input.read(&mut buffer)?;
                    if n == 0 {
                        break;
                    }
                    count += n as u64;
                    ensure!(count <= segment.bytes, "segment grew during assembly");
                    checked.update(&buffer[..n]);
                    hash.update(&buffer[..n]);
                    output.write_all(&buffer[..n])?;
                }
                ensure!(
                    count == segment.bytes
                        && checked.finalize().to_hex().as_str() == segment.blake3,
                    "segment changed during assembly"
                );
                bytes += count;
            }
            output.sync_all()?;
            drop(output);
            let result = DownloadResult {
                path: joined.clone(),
                bytes,
                blake3: hash.finalize().to_hex().to_string(),
                resumed_bytes: 0,
                cached: false,
            };
            let extension = match track.mime.as_str() {
                "video/mp2t" => "ts",
                "video/mp4" | "audio/mp4" => "mp4",
                _ => "bin",
            };
            let item = MediaItem {
                source: source.to_string(),
                id: format!("track-{ti}"),
                revision: revision.clone(),
                url: source.to_string(),
                title: track.id.clone(),
                extension: extension.into(),
                metadata: serde_json::json!({"mime":track.mime,"segments":track.segments.len(),"manifest_blake3":revision,"assembly":"per-track concatenation; not A/V mux"}),
            };
            published.push(crate::postprocess::publish_media(
                &item,
                &result,
                &directory.join("tracks"),
                "{id}-{hash}.{ext}",
            )?);
            fs::remove_file(joined)?;
        }
    }
    if let Some(manifest) = offline {
        // Publish the MPD only after all referenced assets are durable.
        let path = directory.join(format!("manifest-{revision}.part"));
        if let Ok(m) = fs::symlink_metadata(&path) {
            ensure!(
                m.file_type().is_file(),
                "manifest staging path must be regular"
            );
        }
        let mut options = OpenOptions::new();
        options.create(true).truncate(true).write(true);
        #[cfg(unix)]
        {
            use std::os::unix::fs::OpenOptionsExt;
            options.mode(0o600);
        }
        let mut file = options.open(&path)?;
        file.write_all(manifest.as_bytes())?;
        file.sync_all()?;
        drop(file);
        let result = DownloadResult {
            path: path.clone(),
            bytes: manifest.len() as u64,
            blake3: blake3::hash(manifest.as_bytes()).to_hex().to_string(),
            resumed_bytes: 0,
            cached: false,
        };
        let item = MediaItem {
            source: source.to_string(),
            id: "presentation".into(),
            revision: revision.clone(),
            url: source.to_string(),
            title: "DASH offline presentation".into(),
            extension: "mpd".into(),
            metadata: serde_json::json!({"tracks":tracks.len(),"timing":"preserved original MPD","assets":"validated local hard links","manifest_blake3":revision}),
        };
        let presentation = crate::postprocess::publish_media(
            &item,
            &result,
            directory,
            "presentation-{hash}.{ext}",
        )?;
        fs::remove_file(path)?;
        return Ok(vec![presentation]);
    }
    Ok(published)
}

pub fn plan_stream(source: &str, base: &Url) -> Result<StreamPlan> {
    ensure!(source.len() <= 1024 * 1024, "stream manifest exceeds 1 MiB");
    if source.starts_with("#EXTM3U") {
        hls(source, base)
    } else {
        dash(source, base)
    }
}
fn target(base: &Url, value: &str) -> Result<String> {
    ensure!(
        !value.trim().is_empty() && value.len() <= 8192,
        "invalid segment URL"
    );
    let mut u = base.join(value)?;
    ensure!(
        matches!(u.scheme(), "http" | "https") && u.username().is_empty() && u.password().is_none(),
        "segments require credential-free HTTP(S)"
    );
    u.set_fragment(None);
    Ok(u.to_string())
}
fn attrs(value: &str) -> Result<BTreeMap<String, String>> {
    let mut out = BTreeMap::new();
    let mut rest = value;
    while !rest.is_empty() {
        let (name, tail) = rest.split_once('=').context("invalid HLS attribute")?;
        ensure!(!name.is_empty(), "empty HLS attribute");
        let (v, next) = if let Some(quoted) = tail.strip_prefix('"') {
            let (v, next) = quoted.split_once('"').context("unclosed HLS attribute")?;
            ensure!(
                next.is_empty() || next.starts_with(','),
                "invalid HLS attribute separator"
            );
            (v, next.strip_prefix(',').unwrap_or(next))
        } else {
            tail.split_once(',').unwrap_or((tail, ""))
        };
        ensure!(
            out.insert(name.into(), v.into()).is_none(),
            "duplicate HLS attribute"
        );
        rest = next;
    }
    Ok(out)
}
fn range(value: &str, previous: Option<u64>) -> Result<(u64, u64)> {
    let (len, start) = value
        .split_once('@')
        .map_or((value, None), |(l, s)| (l, Some(s)));
    let len: u64 = len.parse()?;
    ensure!(len > 0, "empty byte range");
    let start = match start {
        Some(s) => s.parse()?,
        None => previous.context("implicit range lacks previous same-resource range")?,
    };
    let end = start.checked_add(len - 1).context("byte range overflow")?;
    Ok((start, end))
}
fn hls(source: &str, base: &Url) -> Result<StreamPlan> {
    let mut segments = Vec::new();
    let mut variants = Vec::new();
    let mut variant = None;
    let mut byte_range = None;
    let mut previous: Option<(String, u64)> = None;
    let (mut duration, mut end, mut discontinuities) = (false, false, 0);
    for (index, line) in source.lines().enumerate() {
        ensure!(index <= 32_768, "too many HLS lines");
        let line = line.trim_end_matches('\r');
        if line.is_empty() || line == "#EXTM3U" {
            continue;
        }
        ensure!(!end, "HLS contains data after ENDLIST");
        if let Some(v) = line.strip_prefix("#EXT-X-STREAM-INF:") {
            ensure!(
                variant.is_none() && segments.is_empty(),
                "mixed or dangling HLS variant"
            );
            variant = Some(attrs(v)?);
        } else if let Some(v) = line.strip_prefix("#EXTINF:") {
            ensure!(!duration, "duplicate EXTINF");
            let d: f64 = v.split(',').next().unwrap_or("").parse()?;
            ensure!(d.is_finite() && d > 0.0, "invalid HLS duration");
            duration = true;
        } else if let Some(v) = line.strip_prefix("#EXT-X-BYTERANGE:") {
            ensure!(byte_range.is_none(), "duplicate byte range");
            byte_range = Some(v.to_owned());
        } else if let Some(v) = line.strip_prefix("#EXT-X-MAP:") {
            ensure!(
                segments.is_empty(),
                "changing/repeated HLS initialization maps require a remuxer"
            );
            let a = attrs(v)?;
            let url = target(base, a.get("URI").context("MAP URI missing")?)?;
            let range = a.get("BYTERANGE").map(|r| range(r, None)).transpose()?;
            segments.push(Segment {
                url,
                range,
                initialization: true,
            });
        } else if let Some(v) = line.strip_prefix("#EXT-X-KEY:") {
            ensure!(
                attrs(v)?.get("METHOD").map(String::as_str) == Some("NONE"),
                "encrypted HLS is unsupported"
            );
        } else if line == "#EXT-X-ENDLIST" {
            end = true;
        } else if line == "#EXT-X-DISCONTINUITY" {
            discontinuities += 1;
        } else if line.starts_with("#EXT-X-PART")
            || line.starts_with("#EXT-X-DEFINE")
            || line.starts_with("#EXT-X-PRELOAD")
            || line.starts_with("#EXT-X-SKIP")
            || line.starts_with("#EXT-X-GAP")
            || line.starts_with("#EXT-X-SESSION-KEY")
            || line.starts_with("#EXT-X-MEDIA:")
            || line.starts_with("#EXT-X-I-FRAMES-ONLY")
        {
            bail!("unsupported HLS rendition/live/gap feature");
        } else if line.starts_with('#') {
            continue;
        } else if let Some(a) = variant.take() {
            ensure!(variants.len() < 128, "too many HLS variants");
            variants.push(Variant {
                url: target(base, line)?,
                bandwidth: a
                    .get("BANDWIDTH")
                    .context("variant bandwidth missing")?
                    .parse()?,
                codecs: a.get("CODECS").cloned(),
            });
        } else {
            ensure!(
                variants.is_empty() && duration,
                "mixed HLS playlist or segment without EXTINF"
            );
            let url = target(base, line)?;
            let previous_end = previous
                .as_ref()
                .filter(|(u, _)| u == &url)
                .and_then(|(_, n)| n.checked_add(1));
            let r = byte_range
                .take()
                .map(|r| range(&r, previous_end))
                .transpose()?;
            previous = r.map(|(_, end)| (url.clone(), end));
            segments.push(Segment {
                url,
                range: r,
                initialization: false,
            });
            duration = false;
        }
        ensure!(
            segments.len() <= MAX_SEGMENTS,
            "HLS segment budget exceeded"
        );
    }
    ensure!(
        variant.is_none() && !duration && byte_range.is_none(),
        "incomplete HLS manifest"
    );
    if !variants.is_empty() {
        return Ok(StreamPlan::Master { variants });
    }
    ensure!(
        end && segments.iter().any(|s| !s.initialization),
        "only complete HLS VOD is supported"
    );
    ensure!(
        discontinuities == 0,
        "HLS discontinuity requires an explicit remuxer"
    );
    Ok(StreamPlan::Vod {
        manifest: None,
        tracks: vec![Track {
            id: "hls".into(),
            mime: if segments.iter().any(|s| s.initialization) {
                "video/mp4"
            } else {
                "video/mp2t"
            }
            .into(),
            segments,
        }],
    })
}

#[derive(Default, Debug, Clone)]
struct Xml {
    name: String,
    attrs: BTreeMap<String, String>,
    children: Vec<Xml>,
    text: String,
}
impl Xml {
    fn child(&self, name: &str) -> Option<&Xml> {
        self.children.iter().find(|n| n.name == name)
    }
    fn attr(&self, name: &str) -> Option<&str> {
        self.attrs.get(name).map(String::as_str)
    }
    fn base(&self, base: &Url) -> Result<Url> {
        ensure!(
            self.children.iter().filter(|n| n.name == "BaseURL").count() <= 1,
            "multiple DASH BaseURL alternatives unsupported"
        );
        match self.child("BaseURL") {
            Some(n) => Ok(Url::parse(&target(base, n.text.trim())?)?),
            None => Ok(base.clone()),
        }
    }
}
fn xml(source: &str) -> Result<Xml> {
    let mut reader = quick_xml::Reader::from_str(source);
    reader.config_mut().trim_text(true);
    let mut stack = vec![Xml::default()];
    let mut count = 0;
    loop {
        match reader.read_event()? {
            Event::Start(e) | Event::Empty(e) => {
                count += 1;
                ensure!(
                    count <= 10_000 && stack.len() <= 32,
                    "DASH XML budget exceeded"
                );
                let mut n = Xml {
                    name: e.local_name().as_ref().to_owned(),
                    ..Default::default()
                };
                for a in e.attributes() {
                    let a = a?;
                    let key = a.key.as_ref().to_owned();
                    ensure!(
                        !key.ends_with(":href"),
                        "external DASH XML references unsupported"
                    );
                    n.attrs.insert(
                        key,
                        a.normalized_value(quick_xml::XmlVersion::Implicit1_0)?
                            .into_owned(),
                    );
                }
                // Empty events and start events share payload but not variant; the
                // source byte just consumed ends in '/>' only for an empty element.
                let end = reader.buffer_position() as usize;
                if source.as_bytes().get(end.saturating_sub(2)..end) == Some(b"/>") {
                    stack.last_mut().unwrap().children.push(n);
                } else {
                    stack.push(n);
                }
            }
            Event::End(_) => {
                ensure!(stack.len() > 1, "unexpected DASH close tag");
                let n = stack.pop().unwrap();
                stack.last_mut().unwrap().children.push(n);
            }
            Event::Text(e) => stack.last_mut().unwrap().text.push_str(&e.xml10_content()),
            Event::GeneralRef(e) => {
                let value = quick_xml::escape::unescape(&format!("&{};", &*e))?.into_owned();
                stack.last_mut().unwrap().text.push_str(&value);
            }
            Event::DocType(_) => bail!("DASH DTD unsupported"),
            Event::Eof => break,
            Event::CData(_) => bail!("DASH CDATA unsupported"),
            _ => {}
        }
    }
    ensure!(
        stack.len() == 1 && stack[0].children.len() == 1,
        "invalid DASH document"
    );
    Ok(stack.pop().unwrap().children.remove(0))
}

/// Keep MPD timing and codec metadata, replace addressing with validated local
/// assets, and remove remote base/clock/location hints. No A/V remux is needed
/// to preserve the presentation window in a local DASH manifest.
fn offline_dash(
    source: &str,
    tracks: &[Track],
    files: &[Vec<crate::download::DownloadResult>],
    directory: &std::path::Path,
    revision: &str,
) -> Result<String> {
    use std::fs::{self, File};
    use std::io::Read;
    let mut root = xml(source)?;
    // Xml stores local element names. Supply the DASH default namespace even
    // when the source used a prefix rather than xmlns on the root element.
    root.attrs
        .insert("xmlns".into(), "urn:mpeg:dash:schema:mpd:2011".into());
    let asset_name = format!("assets-{revision}");
    let assets = directory.join(&asset_name);
    match fs::symlink_metadata(&assets) {
        Ok(m) => ensure!(
            m.file_type().is_dir(),
            "DASH assets path must be a directory, not a symlink"
        ),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => fs::create_dir(&assets)?,
        Err(e) => return Err(e.into()),
    }
    let mut index = 0;
    for period in root.children.iter_mut().filter(|n| n.name == "Period") {
        for set in period
            .children
            .iter_mut()
            .filter(|n| n.name == "AdaptationSet")
        {
            let parent_list = set.child("SegmentList").cloned();
            let parent_template = set.child("SegmentTemplate").cloned();
            for rep in set
                .children
                .iter_mut()
                .filter(|n| n.name == "Representation")
            {
                let track = tracks.get(index).context("DASH track mapping mismatch")?;
                let blobs = files.get(index).context("DASH download mapping mismatch")?;
                ensure!(
                    blobs.len() == track.segments.len(),
                    "DASH segment mapping mismatch"
                );
                let mut addressing = rep
                    .child("SegmentList")
                    .or(parent_list.as_ref())
                    .or_else(|| rep.child("SegmentTemplate"))
                    .or(parent_template.as_ref())
                    .context("DASH addressing missing")?
                    .clone();
                if addressing.name == "SegmentTemplate"
                    && let Some(parent) = &parent_template
                {
                    for (key, value) in &parent.attrs {
                        addressing
                            .attrs
                            .entry(key.clone())
                            .or_insert_with(|| value.clone());
                    }
                    if addressing.child("SegmentTimeline").is_none()
                        && let Some(timeline) = parent.child("SegmentTimeline")
                    {
                        addressing.children.push(timeline.clone());
                    }
                }
                let template_mode = addressing.name == "SegmentTemplate";
                let mut number: u64 = addressing.attr("startNumber").unwrap_or("1").parse()?;
                let mut list = Xml {
                    name: if template_mode {
                        "SegmentTemplate"
                    } else {
                        "SegmentList"
                    }
                    .into(),
                    attrs: addressing
                        .attrs
                        .into_iter()
                        .filter(|(k, _)| {
                            matches!(
                                k.as_str(),
                                "timescale" | "duration" | "presentationTimeOffset" | "startNumber"
                            )
                        })
                        .collect(),
                    children: addressing
                        .children
                        .into_iter()
                        .filter(|n| n.name == "SegmentTimeline")
                        .collect(),
                    ..Default::default()
                };
                for (si, (segment, blob)) in track.segments.iter().zip(blobs).enumerate() {
                    let ext = if segment.initialization {
                        "mp4"
                    } else if track.mime.ends_with("webm") {
                        "webm"
                    } else {
                        "m4s"
                    };
                    let filename = if template_mode && !segment.initialization {
                        let name = format!("t{index}-n{number}.{ext}");
                        number = number
                            .checked_add(1)
                            .context("local DASH number overflow")?;
                        list.attrs.insert(
                            "media".into(),
                            format!("{asset_name}/t{index}-n$Number$.{ext}"),
                        );
                        name
                    } else {
                        format!("t{index}-s{si}.{ext}")
                    };
                    let path = assets.join(&filename);
                    match fs::hard_link(&blob.path, &path) {
                        Ok(()) => {}
                        Err(e) if e.kind() == std::io::ErrorKind::AlreadyExists => {
                            let m = fs::symlink_metadata(&path)?;
                            ensure!(
                                m.file_type().is_file() && m.len() == blob.bytes,
                                "DASH asset collision"
                            );
                            let mut file = File::open(&path)?;
                            let mut hash = blake3::Hasher::new();
                            let mut buffer = [0; 64 * 1024];
                            let mut count = 0;
                            loop {
                                let n = file.read(&mut buffer)?;
                                if n == 0 {
                                    break;
                                }
                                count += n as u64;
                                ensure!(count <= blob.bytes, "DASH asset grew during verification");
                                hash.update(&buffer[..n]);
                            }
                            ensure!(
                                count == blob.bytes
                                    && hash.finalize().to_hex().as_str() == blob.blake3,
                                "DASH asset checksum mismatch"
                            );
                        }
                        Err(e) => return Err(e.into()),
                    }
                    if template_mode {
                        if segment.initialization {
                            list.attrs.insert(
                                "initialization".into(),
                                format!("{asset_name}/{filename}"),
                            );
                        }
                    } else {
                        list.children.push(Xml {
                            name: if segment.initialization {
                                "Initialization"
                            } else {
                                "SegmentURL"
                            }
                            .into(),
                            attrs: BTreeMap::from([(
                                if segment.initialization {
                                    "sourceURL"
                                } else {
                                    "media"
                                }
                                .into(),
                                format!("{asset_name}/{filename}"),
                            )]),
                            ..Default::default()
                        });
                    }
                }
                list.children.sort_by_key(|n| match n.name.as_str() {
                    "Initialization" => 0,
                    "SegmentTimeline" => 1,
                    _ => 2,
                });
                rep.children
                    .retain(|n| !matches!(n.name.as_str(), "SegmentList" | "SegmentTemplate"));
                rep.children.push(list);
                index += 1;
            }
            set.children
                .retain(|n| !matches!(n.name.as_str(), "SegmentList" | "SegmentTemplate"));
        }
    }
    ensure!(index == tracks.len(), "DASH representation count mismatch");
    #[cfg(unix)]
    File::open(&assets)?.sync_all()?;
    strip_remote_hints(&mut root);
    let mut result = String::from("<?xml version=\"1.0\" encoding=\"UTF-8\"?>\n");
    write_xml(&root, &mut result)?;
    Ok(result)
}
fn strip_remote_hints(node: &mut Xml) {
    node.children.retain(|n| {
        !matches!(
            n.name.as_str(),
            "BaseURL" | "Location" | "PatchLocation" | "UTCTiming"
        )
    });
    for n in &mut node.children {
        strip_remote_hints(n);
    }
}
fn write_xml(node: &Xml, out: &mut String) -> Result<()> {
    out.push('<');
    out.push_str(&node.name);
    for (k, v) in &node.attrs {
        out.push(' ');
        out.push_str(k);
        out.push_str("=\"");
        out.push_str(&quick_xml::escape::escape(v));
        out.push('"');
    }
    if node.children.is_empty() && node.text.is_empty() {
        out.push_str("/>");
    } else {
        out.push('>');
        out.push_str(&quick_xml::escape::escape(&node.text));
        for n in &node.children {
            write_xml(n, out)?;
        }
        out.push_str("</");
        out.push_str(&node.name);
        out.push('>');
    }
    ensure!(
        out.len() <= 2 * 1024 * 1024,
        "local DASH manifest exceeds 2 MiB"
    );
    Ok(())
}
fn dash(source: &str, base: &Url) -> Result<StreamPlan> {
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
fn dash_range(v: &str) -> Result<(u64, u64)> {
    let (start, end) = v.split_once('-').context("invalid DASH byte range")?;
    let (start, end) = (start.parse()?, end.parse()?);
    ensure!(start <= end, "inverted DASH range");
    Ok((start, end))
}
fn segment_list(list: &Xml, base: &Url) -> Result<Vec<Segment>> {
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
fn template_segments(
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
fn iso_duration(value: &str) -> Result<f64> {
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
fn expand(
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

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn offline_dash_preserves_timing_and_publishes_only_local_verified_assets() {
        let base = Url::parse("https://example.test/remote/index.mpd").unwrap();
        let source = r#"<MPD type="static" mediaPresentationDuration="PT2.0S"><BaseURL>https://cdn.test/</BaseURL><Location>https://remote.test/new.mpd</Location><Period start="PT0.0S"><AdaptationSet mimeType="audio/mp4"><Representation id="a"><SegmentTemplate timescale="44100" presentationTimeOffset="1024" initialization="init.mp4" media="$Time$.m4s"><SegmentTimeline><S t="0" d="44100" r="1"/></SegmentTimeline></SegmentTemplate></Representation></AdaptationSet></Period></MPD>"#;
        let StreamPlan::Vod { tracks, .. } = plan_stream(source, &base).unwrap() else {
            panic!()
        };
        let root = tempfile::tempdir().unwrap();
        let blob = root.path().join("blob");
        std::fs::write(&blob, b"media").unwrap();
        let result = crate::download::DownloadResult {
            path: blob,
            bytes: 5,
            blake3: blake3::hash(b"media").to_hex().to_string(),
            resumed_bytes: 0,
            cached: false,
        };
        let files = vec![vec![result.clone(); 3]];
        let local = offline_dash(source, &tracks, &files, root.path(), "frozen").unwrap();
        assert!(local.contains("mediaPresentationDuration=\"PT2.0S\""));
        assert!(local.contains("presentationTimeOffset=\"1024\""));
        assert!(local.contains("<SegmentTimeline>"));
        assert!(local.contains("xmlns=\"urn:mpeg:dash:schema:mpd:2011\""));
        assert!(!local.contains("cdn.test"));
        assert!(!local.contains("remote.test"));
        assert!(local.contains("assets-frozen/t0-s0.mp4"));
        assert!(local.contains("assets-frozen/t0-n$Number$.m4s"));
        assert_eq!(
            offline_dash(source, &tracks, &files, root.path(), "frozen").unwrap(),
            local
        );
        std::fs::write(root.path().join("assets-frozen/t0-n1.m4s"), b"wrong").unwrap();
        assert!(offline_dash(source, &tracks, &files, root.path(), "frozen").is_err());
    }
    #[test]
    fn hls_variants_maps_and_range_offsets() {
        let base = Url::parse("https://example.test/video/index.m3u8").unwrap();
        let StreamPlan::Master { variants } = plan_stream(
            "#EXTM3U\n#EXT-X-STREAM-INF:BANDWIDTH=100,CODECS=\"avc1,mp4a\"\nlow.m3u8\n",
            &base,
        )
        .unwrap() else {
            panic!()
        };
        assert_eq!(variants[0].url, "https://example.test/video/low.m3u8");
        let source = "#EXTM3U\n#EXT-X-MAP:URI=\"init.mp4\"\n#EXTINF:2,\n#EXT-X-BYTERANGE:10@0\nall.mp4\n#EXTINF:2,\n#EXT-X-BYTERANGE:5\nall.mp4\n#EXT-X-ENDLIST\n";
        let StreamPlan::Vod { tracks, .. } = plan_stream(source, &base).unwrap() else {
            panic!()
        };
        assert_eq!(tracks[0].segments[2].range, Some((10, 14)));
        for bad in [
            source.replace("all.mp4\n#EXT-X-ENDLIST", "other.mp4\n#EXT-X-ENDLIST"),
            source.replace("#EXT-X-ENDLIST", ""),
            source.replace("#EXT-X-MAP:", "#EXT-X-KEY:METHOD=AES-128,URI="),
            source.replace("#EXTINF:2,", "#EXTINF:NaN,"),
        ] {
            assert!(plan_stream(&bad, &base).is_err());
        }
    }
    #[test]
    fn dash_finite_timeline_and_segment_list() {
        let base = Url::parse("https://example.test/root/manifest.mpd").unwrap();
        let mpd = r#"<MPD type="static"><BaseURL>media/</BaseURL><Period><AdaptationSet mimeType="video/mp4"><SegmentTemplate initialization="$RepresentationID$-init.mp4" media="$RepresentationID$-$Number%03d$-$Time$.m4s" startNumber="7"><SegmentTimeline><S t="20" d="5" r="1"/></SegmentTimeline></SegmentTemplate><Representation id="v1" bandwidth="100"/></AdaptationSet></Period></MPD>"#;
        assert!(plan_stream(&mpd.replace("<Period>", "<Period start='PT0.0S'>"), &base).is_ok());
        let StreamPlan::Vod { tracks, .. } = plan_stream(mpd, &base).unwrap() else {
            panic!()
        };
        assert_eq!(
            tracks[0].segments[2].url,
            "https://example.test/root/media/v1-008-25.m4s"
        );
        let list = r#"<MPD><Period><AdaptationSet><Representation id="a"><SegmentList><Initialization sourceURL="init.mp4" range="0-9"/><SegmentURL media="a.m4s" mediaRange="10-19"/></SegmentList></Representation></AdaptationSet></Period></MPD>"#;
        assert!(matches!(
            plan_stream(list, &base).unwrap(),
            StreamPlan::Vod { .. }
        ));
        for bad in [
            mpd.replace("type=\"static\"", "type=\"dynamic\""),
            mpd.replace("r=\"1\"", "r=\"-1\""),
            mpd.replace(
                "<Representation id=\"v1\" bandwidth=\"100\"/>",
                "<ContentProtection/><Representation id=\"v1\" bandwidth=\"100\"/>",
            ),
        ] {
            assert!(plan_stream(&bad, &base).is_err());
        }
    }
}
