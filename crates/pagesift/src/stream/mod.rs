//! Native bounded HLS/DASH VOD planning. Unsupported live/encrypted/index-based
//! formats fail explicitly. Planning does not execute a player or a helper tool.
use std::collections::BTreeMap;

use anyhow::{Context, Result, bail, ensure};
use serde::Serialize;
use url::Url;

mod dash;
mod xml;

use dash::dash;
use xml::offline_dash;

pub(super) const MAX_SEGMENTS: usize = 4096;
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
pub(super) fn target(base: &Url, value: &str) -> Result<String> {
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
