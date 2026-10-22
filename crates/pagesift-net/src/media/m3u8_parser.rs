//! Pure Safe Rust HLS M3U8 Manifest Parser (Master & Media Playlists).
//! Supports bandwidth, resolution, codecs, byte-ranges, and initialization maps.

use std::collections::BTreeMap;
use url::Url;

/// Representation of an HLS variant stream in a Master Playlist.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct HlsVariant {
    pub uri: String,
    pub bandwidth: u64,
    pub resolution: Option<(u32, u32)>,
    pub codecs: Option<String>,
}

/// Representation of a single media segment in a Media Playlist.
#[derive(Clone, Debug, PartialEq)]
pub struct HlsSegment {
    pub uri: String,
    pub duration_s: f64,
    pub byte_range: Option<(u64, u64)>, // (start, length)
    pub is_init: bool,
}

/// Master Playlist containing multiple variant streams.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct HlsMasterPlaylist {
    pub variants: Vec<HlsVariant>,
}

/// Media Playlist containing sequence of playable segments.
#[derive(Clone, Debug, PartialEq)]
pub struct HlsMediaPlaylist {
    pub target_duration_s: f64,
    pub media_sequence: u64,
    pub is_vod: bool,
    pub segments: Vec<HlsSegment>,
}

/// Discriminated union of parsed HLS playlists.
#[derive(Clone, Debug, PartialEq)]
pub enum HlsPlaylist {
    Master(HlsMasterPlaylist),
    Media(HlsMediaPlaylist),
}

#[derive(Debug, thiserror::Error)]
pub enum M3u8Error {
    #[error("missing #EXTM3U header")]
    MissingHeader,
    #[error("empty playlist")]
    Empty,
    #[error("invalid attribute list: {0}")]
    InvalidAttrs(String),
    #[error("invalid duration: {0}")]
    InvalidDuration(String),
    #[error("invalid byte range: {0}")]
    InvalidByteRange(String),
    #[error("encrypted HLS stream is not supported")]
    EncryptedUnsupported,
    #[error("relative URI resolution error: {0}")]
    UriResolution(String),
    #[error("malformed syntax: {0}")]
    Malformed(String),
}

/// Parse attributes in key=value or key="value" format.
fn parse_tag_attributes(attr_str: &str) -> Result<BTreeMap<String, String>, M3u8Error> {
    let mut map = BTreeMap::new();
    let chars: Vec<char> = attr_str.chars().collect();
    let mut i = 0;
    let len = chars.len();

    while i < len {
        // Skip whitespace and commas
        while i < len && (chars[i].is_whitespace() || chars[i] == ',') {
            i += 1;
        }
        if i >= len {
            break;
        }

        // Parse key
        let key_start = i;
        while i < len && chars[i] != '=' && chars[i] != ',' && !chars[i].is_whitespace() {
            i += 1;
        }
        let key: String = chars[key_start..i].iter().collect();

        while i < len && chars[i].is_whitespace() {
            i += 1;
        }
        if i >= len || chars[i] != '=' {
            map.insert(key, String::new());
            continue;
        }
        i += 1; // skip '='

        while i < len && chars[i].is_whitespace() {
            i += 1;
        }
        if i >= len {
            map.insert(key, String::new());
            break;
        }

        // Parse value
        let value = if chars[i] == '"' {
            i += 1;
            let val_start = i;
            while i < len && chars[i] != '"' {
                i += 1;
            }
            let val: String = chars[val_start..i].iter().collect();
            if i < len && chars[i] == '"' {
                i += 1;
            }
            val
        } else {
            let val_start = i;
            while i < len && chars[i] != ',' && !chars[i].is_whitespace() {
                i += 1;
            }
            chars[val_start..i].iter().collect()
        };

        map.insert(key, value);
    }
    Ok(map)
}

fn resolve_url(base: Option<&Url>, raw: &str) -> Result<String, M3u8Error> {
    match base {
        Some(b) => b
            .join(raw)
            .map(|u| u.to_string())
            .map_err(|e| M3u8Error::UriResolution(e.to_string())),
        None => Ok(raw.to_string()),
    }
}

/// Parse byte range string like "length[@offset]"
fn parse_byte_range(s: &str, prev_end: Option<u64>) -> Result<(u64, u64), M3u8Error> {
    let parts: Vec<&str> = s.split('@').collect();
    let length: u64 = parts[0]
        .parse()
        .map_err(|_| M3u8Error::InvalidByteRange(s.into()))?;
    let start = if parts.len() > 1 {
        parts[1]
            .parse()
            .map_err(|_| M3u8Error::InvalidByteRange(s.into()))?
    } else {
        prev_end.unwrap_or(0)
    };
    Ok((start, length))
}

/// Pure Safe Rust streaming parser for M3U8 content.
pub fn parse_m3u8(content: &str, base_url: Option<&Url>) -> Result<HlsPlaylist, M3u8Error> {
    let mut lines = content.lines().map(str::trim).filter(|l| !l.is_empty());

    let first = lines.next().ok_or(M3u8Error::Empty)?;
    if first != "#EXTM3U" {
        return Err(M3u8Error::MissingHeader);
    }

    let mut is_master = false;
    let mut variants = Vec::new();
    let mut segments = Vec::new();

    let mut pending_variant_attrs = None;
    let mut pending_duration = None;
    let mut pending_byte_range = None;
    let mut prev_range_end = None;
    let mut target_duration = 0.0;
    let mut media_sequence = 0;
    let mut is_vod = false;

    for line in lines {
        if let Some(stream_inf) = line.strip_prefix("#EXT-X-STREAM-INF:") {
            is_master = true;
            let attrs = parse_tag_attributes(stream_inf)?;
            pending_variant_attrs = Some(attrs);
        } else if let Some(extinf) = line.strip_prefix("#EXTINF:") {
            let dur_str = extinf.split(',').next().unwrap_or("").trim();
            let dur: f64 = dur_str
                .parse()
                .map_err(|_| M3u8Error::InvalidDuration(dur_str.into()))?;
            if !dur.is_finite() || dur <= 0.0 {
                return Err(M3u8Error::InvalidDuration(dur_str.into()));
            }
            pending_duration = Some(dur);
        } else if let Some(br) = line.strip_prefix("#EXT-X-BYTERANGE:") {
            let range = parse_byte_range(br.trim(), prev_range_end)?;
            prev_range_end = Some(range.0 + range.1);
            pending_byte_range = Some(range);
        } else if let Some(map_tag) = line.strip_prefix("#EXT-X-MAP:") {
            let attrs = parse_tag_attributes(map_tag)?;
            let uri_raw = attrs
                .get("URI")
                .ok_or_else(|| M3u8Error::Malformed("EXT-X-MAP missing URI".into()))?;
            let uri = resolve_url(base_url, uri_raw)?;
            let byte_range = if let Some(br) = attrs.get("BYTERANGE") {
                Some(parse_byte_range(br, None)?)
            } else {
                None
            };
            segments.push(HlsSegment {
                uri,
                duration_s: 0.0,
                byte_range,
                is_init: true,
            });
        } else if let Some(td) = line.strip_prefix("#EXT-X-TARGETDURATION:") {
            target_duration = td
                .trim()
                .parse::<f64>()
                .map_err(|_| M3u8Error::InvalidDuration(td.into()))?;
        } else if let Some(seq) = line.strip_prefix("#EXT-X-MEDIA-SEQUENCE:") {
            media_sequence = seq
                .trim()
                .parse::<u64>()
                .map_err(|_| M3u8Error::Malformed("invalid media sequence".into()))?;
        } else if line == "#EXT-X-ENDLIST" {
            is_vod = true;
        } else if let Some(key) = line.strip_prefix("#EXT-X-KEY:") {
            let attrs = parse_tag_attributes(key)?;
            if attrs.get("METHOD").map(String::as_str) != Some("NONE") {
                return Err(M3u8Error::EncryptedUnsupported);
            }
        } else if line.starts_with('#') {
            // Other optional comments or tags ignored safely
            continue;
        } else {
            // Non-tag line: URI
            if let Some(attrs) = pending_variant_attrs.take() {
                let bandwidth = attrs
                    .get("BANDWIDTH")
                    .and_then(|b| b.parse::<u64>().ok())
                    .unwrap_or(0);
                let resolution = attrs.get("RESOLUTION").and_then(|res| {
                    let mut parts = res.split('x');
                    let w = parts.next()?.parse::<u32>().ok()?;
                    let h = parts.next()?.parse::<u32>().ok()?;
                    Some((w, h))
                });
                let codecs = attrs.get("CODECS").cloned();
                let uri = resolve_url(base_url, line)?;
                variants.push(HlsVariant {
                    uri,
                    bandwidth,
                    resolution,
                    codecs,
                });
            } else if let Some(dur) = pending_duration.take() {
                let uri = resolve_url(base_url, line)?;
                let byte_range = pending_byte_range.take();
                segments.push(HlsSegment {
                    uri,
                    duration_s: dur,
                    byte_range,
                    is_init: false,
                });
            }
        }
    }

    if is_master {
        Ok(HlsPlaylist::Master(HlsMasterPlaylist { variants }))
    } else {
        Ok(HlsPlaylist::Media(HlsMediaPlaylist {
            target_duration_s: target_duration,
            media_sequence,
            is_vod,
            segments,
        }))
    }
}
