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

const VALID_FROM_KEYS: &[&str] = &["validFrom", "effectiveDate", "startDate"];
const VALID_UNTIL_KEYS: &[&str] = &["validThrough", "priceValidUntil", "expires", "endDate"];

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
    ///
    /// Nested JSON-LD validity intervals (`validFrom`/`validThrough`) are only promoted to
    /// document-level when a unique, unambiguous value appears across the document's JSON-LD graph;
    /// multi-item offers/events are resolved per claim via [`Self::for_claim`].
    pub fn from_document_sources(
        observed_at_ms: u64,
        meta_published: Option<&str>,
        meta_modified: Option<&str>,
        http_last_modified: Option<&str>,
        json_ld: &[Value],
    ) -> Self {
        let ld_pub = find_unique_json_ld_str(
            json_ld,
            &["datePublished", "dateCreated", "uploadDate"],
            true,
        );
        let ld_mod = find_unique_json_ld_str(json_ld, &["dateModified"], true);
        let ld_from = find_unique_json_ld_str(json_ld, VALID_FROM_KEYS, false);
        let ld_until = find_unique_json_ld_str(json_ld, VALID_UNTIL_KEYS, false);

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

    /// Resolve per-claim temporal semantics from `doc_temporal` and JSON-LD object proximity.
    pub fn for_claim(
        doc_temporal: &Self,
        json_ld: &[Value],
        field_name: &str,
        field_value: &Value,
    ) -> Self {
        let mut out = doc_temporal.clone();
        if let Some((claim_from, claim_until)) =
            find_claim_json_ld_validity(json_ld, field_name, field_value)
        {
            if claim_from.is_some() {
                out.valid_from = claim_from;
            }
            if claim_until.is_some() {
                out.valid_until = claim_until;
            }
        }
        out
    }

    /// Returns true if this representation was already observed at or before `as_of_ms`.
    pub fn is_observed_as_of(&self, as_of_ms: u64) -> bool {
        self.observed_at_ms <= as_of_ms
    }

    pub fn has_rich_metadata(&self) -> bool {
        self.published_at_ms.is_some()
            || self.modified_at_ms.is_some()
            || self.published_at.is_some()
            || self.modified_at.is_some()
            || self.valid_from.is_some()
            || self.valid_until.is_some()
    }
}

fn clean_opt(raw: Option<&str>) -> Option<String> {
    let trimmed = raw?.trim();
    (!trimmed.is_empty()).then(|| trimmed.to_owned())
}

fn collect_key_values(val: &Value, keys: &[&str], out: &mut Vec<String>) {
    match val {
        Value::Object(map) => {
            for &k in keys {
                if let Some(Value::String(s)) = map.get(k) {
                    let t = s.trim();
                    if !t.is_empty() && !out.iter().any(|existing| existing == t) {
                        out.push(t.to_owned());
                    }
                }
            }
            for v in map.values() {
                collect_key_values(v, keys, out);
            }
        }
        Value::Array(arr) => {
            for v in arr {
                collect_key_values(v, keys, out);
            }
        }
        _ => {}
    }
}

fn top_level_key_str(items: &[Value], keys: &[&str]) -> Option<String> {
    let mut top = Vec::new();
    for item in items {
        if let Value::Object(map) = item {
            for &k in keys {
                if let Some(Value::String(s)) = map.get(k) {
                    let t = s.trim();
                    if !t.is_empty() && !top.iter().any(|x| x == t) {
                        top.push(t.to_owned());
                    }
                }
            }
        }
    }
    (top.len() == 1).then(|| top.remove(0))
}

fn find_unique_json_ld_str(
    items: &[Value],
    keys: &[&str],
    prefer_top_level: bool,
) -> Option<String> {
    if prefer_top_level && let Some(top) = top_level_key_str(items, keys) {
        return Some(top);
    }
    let mut vals = Vec::new();
    for item in items {
        collect_key_values(item, keys, &mut vals);
        if vals.len() > 1 {
            return None;
        }
    }
    (vals.len() == 1).then(|| vals.remove(0))
}

fn first_direct_key(map: &serde_json::Map<String, Value>, keys: &[&str]) -> Option<String> {
    for &k in keys {
        if let Some(Value::String(s)) = map.get(k) {
            let t = s.trim();
            if !t.is_empty() {
                return Some(t.to_owned());
            }
        }
    }
    None
}

fn value_matches_loose(candidate: &Value, target: &Value) -> bool {
    if candidate == target {
        return true;
    }
    match (candidate, target) {
        (Value::String(a), Value::String(b)) => a.trim() == b.trim(),
        (Value::Number(n), Value::String(b)) => b.trim().contains(&n.to_string()),
        (Value::String(a), Value::Number(n)) => a.trim().contains(&n.to_string()),
        _ => false,
    }
}

fn search_matching_offer_validity(
    val: &Value,
    field_name: &str,
    field_value: &Value,
) -> Option<(Option<String>, Option<String>)> {
    match val {
        Value::Object(map) => {
            for v in map.values() {
                if let Some(found) = search_matching_offer_validity(v, field_name, field_value) {
                    return Some(found);
                }
            }
            let matches_field = map.iter().any(|(k, v)| {
                (k.eq_ignore_ascii_case(field_name) || field_name.is_empty())
                    && value_matches_loose(v, field_value)
            });
            if matches_field {
                let vf = first_direct_key(map, VALID_FROM_KEYS);
                let vu = first_direct_key(map, VALID_UNTIL_KEYS);
                if vf.is_some() || vu.is_some() {
                    return Some((vf, vu));
                }
            }
            None
        }
        Value::Array(arr) => arr
            .iter()
            .find_map(|item| search_matching_offer_validity(item, field_name, field_value)),
        _ => None,
    }
}

fn find_claim_json_ld_validity(
    items: &[Value],
    field_name: &str,
    field_value: &Value,
) -> Option<(Option<String>, Option<String>)> {
    for item in items {
        if let Some(found) = search_matching_offer_validity(item, field_name, field_value) {
            return Some(found);
        }
    }
    None
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
    if !s.is_ascii() {
        return None;
    }
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
        if frac.is_empty() {
            return None;
        }
        let mut scale = 100i64;
        for ch in frac.chars().take(3) {
            millis += i64::from(ch.to_digit(10)?) * scale;
            scale /= 10;
        }
    }
    let tz_rest = s.get(pos..)?.trim();
    // Datetime strings without an explicit UTC or numeric offset are ambiguous across zones.
    let offset_secs: i64 = if tz_rest.eq_ignore_ascii_case("Z") {
        0
    } else if tz_rest.starts_with('+') || tz_rest.starts_with('-') {
        let sign = if tz_rest.as_bytes()[0] == b'-' {
            -1i64
        } else {
            1i64
        };
        let tz_digits = &tz_rest[1..];
        let (tz_h, tz_m) = if let Some((h, m)) = tz_digits.split_once(':') {
            if h.len() != 2 || m.len() != 2 {
                return None;
            }
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

fn days_in_month(year: i64, month: u32) -> Option<u32> {
    let leap = (year % 4 == 0 && year % 100 != 0) || year % 400 == 0;
    match month {
        1 | 3 | 5 | 7 | 8 | 10 | 12 => Some(31),
        4 | 6 | 9 | 11 => Some(30),
        2 => Some(if leap { 29 } else { 28 }),
        _ => None,
    }
}

/// Howard Hinnant civil-date to Unix epoch day algorithm (`days_from_civil`).
fn days_from_civil(year: i64, month: u32, day: u32) -> Option<i64> {
    if !(1970..=9999).contains(&year) {
        return None;
    }
    let max_day = days_in_month(year, month)?;
    if !(1..=max_day).contains(&day) {
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
        assert_eq!(parse_timestamp_ms("2025-02-29"), None);
        assert_eq!(parse_timestamp_ms("2024-02-29"), Some(1_709_164_800_000));
        assert_eq!(parse_timestamp_ms("2026-04-31"), None);
        assert_eq!(parse_timestamp_ms("2026-11-15T12:00:00"), None);

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

        let multi = [json!({
            "@type": "Product",
            "datePublished": "2026-11-01",
            "offers": [
                { "price": "19", "priceValidUntil": "2026-12-01" },
                { "price": "29", "priceValidUntil": "2027-01-15" }
            ]
        })];
        let doc_multi =
            TemporalSemantics::from_document_sources(1_794_750_000_000, None, None, None, &multi);
        assert_eq!(doc_multi.valid_until, None);
        let offer_claim = TemporalSemantics::for_claim(&doc_multi, &multi, "price", &json!("29"));
        assert_eq!(offer_claim.valid_until.as_deref(), Some("2027-01-15"));
        let offer_19 = TemporalSemantics::for_claim(&doc_multi, &multi, "price", &json!("19"));
        assert_eq!(offer_19.valid_until.as_deref(), Some("2026-12-01"));
        let unrelated = TemporalSemantics::for_claim(&doc_multi, &multi, "brand", &json!("Acme"));
        assert_eq!(
            unrelated.valid_until, None,
            "multi-offer validity must not bleed into unrelated claims on the page (T17)"
        );
    }
}
