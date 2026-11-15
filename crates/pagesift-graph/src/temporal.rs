//! Multi-dimensional temporal semantics for documents, observations, and claims.
//!
//! Distinguishes PageSift observation time (`observed_at_ms`) from source-declared publication
//! time (`published_at`), source modification time (`modified_at`), and claim validity intervals
//! (`valid_from` / `valid_until`). Unknown temporal dimensions remain `None` rather than guessed.

use serde::{Deserialize, Serialize};
use serde_json::Value;

/// Distinct temporal dimensions associated with a document representation or structured claim.
#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct TemporalSemantics {
    /// Timestamp (Unix ms) when PageSift observed or verified this representation.
    pub observed_at_ms: u64,
    /// Source-declared publication timestamp (ISO-8601 / RFC-3339 string, if present).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub published_at: Option<String>,
    /// Parsed publication timestamp in Unix ms when deterministically parseable.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub published_at_ms: Option<u64>,
    /// Source-declared content modification timestamp (meta / JSON-LD / HTTP Last-Modified).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub modified_at: Option<String>,
    /// Parsed modification timestamp in Unix ms when deterministically parseable.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub modified_at_ms: Option<u64>,
    /// Start of the validity window described by the claim itself (e.g. `validFrom`, `effectiveDate`).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub valid_from: Option<String>,
    /// End of the validity window described by the claim itself (e.g. `validThrough`, `priceValidUntil`).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub valid_until: Option<String>,
}

impl TemporalSemantics {
    /// Construct temporal semantics with only the confirmed PageSift observation timestamp.
    pub fn observed_only(observed_at_ms: u64) -> Self {
        Self {
            observed_at_ms,
            published_at: None,
            published_at_ms: None,
            modified_at: None,
            modified_at_ms: None,
            valid_from: None,
            valid_until: None,
        }
    }

    /// Build document-level temporal metadata from explicit metadata fields, JSON-LD, and HTTP headers.
    pub fn from_document_sources(
        observed_at_ms: u64,
        meta_published: Option<&str>,
        meta_modified: Option<&str>,
        http_last_modified: Option<&str>,
        json_ld: &[Value],
    ) -> Self {
        let ld_pub = find_json_ld_str(json_ld, &["datePublished", "dateCreated", "uploadDate"]);
        let ld_mod = find_json_ld_str(json_ld, &["dateModified"]);
        let ld_from = find_json_ld_str(json_ld, &["validFrom", "effectiveDate", "startDate"]);
        let ld_until = find_json_ld_str(
            json_ld,
            &["validThrough", "priceValidUntil", "expires", "endDate"],
        );

        let published_at = clean_opt(meta_published).or(ld_pub);
        let modified_at = clean_opt(meta_modified)
            .or(ld_mod)
            .or_else(|| clean_opt(http_last_modified));

        let published_at_ms = published_at.as_deref().and_then(parse_timestamp_ms);
        let modified_at_ms = modified_at.as_deref().and_then(parse_timestamp_ms);

        Self {
            observed_at_ms,
            published_at,
            published_at_ms,
            modified_at,
            modified_at_ms,
            valid_from: ld_from,
            valid_until: ld_until,
        }
    }

    /// Returns true if this representation was already observed at or before `as_of_ms`.
    pub fn is_observed_as_of(&self, as_of_ms: u64) -> bool {
        self.observed_at_ms <= as_of_ms
    }
}

fn clean_opt(raw: Option<&str>) -> Option<String> {
    let trimmed = raw?.trim();
    (!trimmed.is_empty()).then(|| trimmed.to_owned())
}

fn find_json_ld_str(items: &[Value], keys: &[&str]) -> Option<String> {
    for item in items {
        if let Some(val) = search_keys(item, keys) {
            return Some(val);
        }
    }
    None
}

fn search_keys(val: &Value, keys: &[&str]) -> Option<String> {
    match val {
        Value::Object(map) => {
            for &k in keys {
                if let Some(Value::String(s)) = map.get(k)
                    && !s.trim().is_empty()
                {
                    return Some(s.trim().to_owned());
                }
            }
            for v in map.values() {
                if let Some(found) = search_keys(v, keys) {
                    return Some(found);
                }
            }
            None
        }
        Value::Array(arr) => arr.iter().find_map(|v| search_keys(v, keys)),
        _ => None,
    }
}

/// Deterministically parse common ISO-8601 / RFC-3339 (`YYYY-MM-DD` or `YYYY-MM-DDTHH:MM:SSZ`)
/// or RFC-9110 HTTP-date (`Sun, 06 Nov 1994 08:49:37 GMT`) into Unix milliseconds.
pub fn parse_timestamp_ms(raw: &str) -> Option<u64> {
    let s = raw.trim();
    if s.is_empty() {
        return None;
    }
    if let Some(ms) = parse_iso8601_ms(s) {
        return Some(ms);
    }
    parse_http_date_ms(s)
}

fn parse_iso8601_ms(s: &str) -> Option<u64> {
    let b = s.as_bytes();
    if b.len() < 10 || b[4] != b'-' || b[7] != b'-' {
        return None;
    }
    let year: i64 = s.get(0..4)?.parse().ok()?;
    let month: u32 = s.get(5..7)?.parse().ok()?;
    let day: u32 = s.get(8..10)?.parse().ok()?;
    let days = days_from_civil(year, month, day)?;
    if b.len() == 10 {
        return u64::try_from(days * 86_400_000).ok();
    }
    let sep = b[10];
    if sep != b'T' && sep != b't' && sep != b' ' {
        return None;
    }
    if b.len() < 16 || b[13] != b':' {
        return None;
    }
    let hour: i64 = s.get(11..13)?.parse().ok()?;
    let min: i64 = s.get(14..16)?.parse().ok()?;
    if !(0..24).contains(&hour) || !(0..60).contains(&min) {
        return None;
    }
    let (sec, mut pos) = if b.get(16) == Some(&b':') && b.len() >= 19 {
        let sc: i64 = s.get(17..19)?.parse().ok()?;
        if !(0..=60).contains(&sc) {
            return None;
        }
        (sc.min(59), 19usize)
    } else {
        (0, 16usize)
    };
    let mut millis = 0i64;
    if b.get(pos) == Some(&b'.') {
        pos += 1;
        let start = pos;
        while pos < b.len() && b[pos].is_ascii_digit() {
            pos += 1;
        }
        let frac = &s[start..pos];
        if !frac.is_empty() {
            let mut scale = 100i64;
            for ch in frac.chars().take(3) {
                millis += i64::from(ch.to_digit(10)?) * scale;
                scale /= 10;
            }
        }
    }
    let tz_rest = s.get(pos..)?.trim();
    let offset_secs: i64 = if tz_rest.is_empty() || tz_rest.eq_ignore_ascii_case("Z") {
        0
    } else if tz_rest.starts_with('+') || tz_rest.starts_with('-') {
        let sign = if tz_rest.as_bytes()[0] == b'-' {
            -1i64
        } else {
            1i64
        };
        let tz_digits = &tz_rest[1..];
        let (tz_h, tz_m) = if let Some((h, m)) = tz_digits.split_once(':') {
            (h.parse::<i64>().ok()?, m.parse::<i64>().ok()?)
        } else if tz_digits.len() == 4 {
            (
                tz_digits[0..2].parse::<i64>().ok()?,
                tz_digits[2..4].parse::<i64>().ok()?,
            )
        } else if tz_digits.len() == 2 {
            (tz_digits.parse::<i64>().ok()?, 0)
        } else {
            return None;
        };
        if !(0..=23).contains(&tz_h) || !(0..=59).contains(&tz_m) {
            return None;
        }
        sign * (tz_h * 3600 + tz_m * 60)
    } else {
        return None;
    };

    let total_secs = days * 86_400 + hour * 3600 + min * 60 + sec - offset_secs;
    u64::try_from(total_secs * 1000 + millis).ok()
}

fn parse_http_date_ms(s: &str) -> Option<u64> {
    // RFC 9110 IMF-fixdate: "Sun, 06 Nov 1994 08:49:37 GMT"
    let (_, rest) = s.split_once(',')?;
    let parts: Vec<&str> = rest.split_whitespace().collect();
    if parts.len() != 5 || !parts[4].eq_ignore_ascii_case("GMT") {
        return None;
    }
    let day: u32 = parts[0].parse().ok()?;
    let month: u32 = match parts[1] {
        "Jan" => 1,
        "Feb" => 2,
        "Mar" => 3,
        "Apr" => 4,
        "May" => 5,
        "Jun" => 6,
        "Jul" => 7,
        "Aug" => 8,
        "Sep" => 9,
        "Oct" => 10,
        "Nov" => 11,
        "Dec" => 12,
        _ => return None,
    };
    let year: i64 = parts[2].parse().ok()?;
    let mut hms = parts[3].split(':');
    let hour: i64 = hms.next()?.parse().ok()?;
    let min: i64 = hms.next()?.parse().ok()?;
    let sec: i64 = hms.next()?.parse().ok()?;
    if hms.next().is_some()
        || !(0..24).contains(&hour)
        || !(0..60).contains(&min)
        || !(0..=60).contains(&sec)
    {
        return None;
    }
    let days = days_from_civil(year, month, day)?;
    let total_secs = days * 86_400 + hour * 3600 + min * 60 + sec.min(59);
    u64::try_from(total_secs * 1000).ok()
}

/// Howard Hinnant civil-date to Unix epoch day algorithm (`days_from_civil`).
fn days_from_civil(year: i64, month: u32, day: u32) -> Option<i64> {
    if !(1970..=9999).contains(&year) || !(1..=12).contains(&month) || !(1..=31).contains(&day) {
        return None;
    }
    let y = if month <= 2 { year - 1 } else { year };
    let m = i64::from(month);
    let d = i64::from(day);
    let era = y.div_euclid(400);
    let yoe = y - era * 400;
    let doy = (153 * ((m + 9) % 12) + 2) / 5 + d - 1;
    let doe = yoe * 365 + yoe / 4 - yoe / 100 + doy;
    Some(era * 146_097 + doe - 719_468)
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn parses_iso8601_and_http_dates_without_guessing_unknowns() {
        assert_eq!(parse_timestamp_ms("2026-11-15"), Some(1_794_700_800_000));
        assert_eq!(
            parse_timestamp_ms("2026-11-15T12:00:00Z"),
            Some(1_794_744_000_000)
        );
        assert_eq!(
            parse_timestamp_ms("2026-11-15T21:00:00+09:00"),
            Some(1_794_744_000_000)
        );
        assert_eq!(
            parse_timestamp_ms("Sun, 15 Nov 2026 12:00:00 GMT"),
            Some(1_794_744_000_000)
        );
        assert_eq!(parse_timestamp_ms("not a timestamp"), None);

        let sem = TemporalSemantics::from_document_sources(
            1_794_750_000_000,
            None,
            None,
            Some("Sun, 15 Nov 2026 12:00:00 GMT"),
            &[json!({
                "@type": "Product",
                "datePublished": "2026-11-01",
                "offers": { "priceValidUntil": "2026-12-31" }
            })],
        );
        assert_eq!(sem.published_at.as_deref(), Some("2026-11-01"));
        assert!(sem.published_at_ms.is_some());
        assert_eq!(sem.valid_until.as_deref(), Some("2026-12-31"));
        assert!(sem.valid_from.is_none());
    }
}
