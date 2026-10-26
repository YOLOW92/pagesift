//! Small DASH helpers shared by the MPD parser: `xs:duration` values,
//! SegmentTemplate identifiers and hierarchical BaseURL resolution
//! (ISO/IEC 23009-1 §5.3.9.4.4 and §5.6, RFC 3986 §5.2).

use url::Url;

/// Parses an `xs:duration` (`PnYnMnDTnHnMnS`) into seconds.
///
/// Components must appear in order and carry a unit; at least one is required,
/// and a `T` must be followed by a time component. Years and months count as
/// 365 and 30 days, as dash.js and Shaka do. Negative durations are rejected.
pub(super) fn parse_iso8601_duration(s: &str) -> Option<f64> {
    const DATE: &[(char, f64)] = &[
        ('Y', 365.0 * 86400.0),
        ('M', 30.0 * 86400.0),
        ('D', 86400.0),
    ];
    const TIME: &[(char, f64)] = &[('H', 3600.0), ('M', 60.0), ('S', 1.0)];
    let rest = s.strip_prefix('P')?;
    let (date, time) = match rest.split_once('T') {
        Some((d, t)) => (d, Some(t)),
        None => (rest, None),
    };
    if time == Some("") {
        return None;
    }
    let mut total = 0.0;
    let mut any = false;
    for (mut part, units) in [(date, DATE), (time.unwrap_or(""), TIME)] {
        let mut next = 0;
        while !part.is_empty() {
            // A trailing number without a unit makes `find` fail.
            let end = part.find(|c: char| !(c.is_ascii_digit() || c == '.'))?;
            if end == 0 {
                return None;
            }
            let unit = part[end..].chars().next()?;
            let i = next + units[next..].iter().position(|&(u, _)| u == unit)?;
            let value: f64 = part[..end].parse().ok()?;
            total += value * units[i].1;
            any = true;
            next = i + 1;
            part = &part[end + unit.len_utf8()..];
        }
    }
    any.then_some(total)
}

/// Widest `%0<width>d` format tag honoured; wider tags are left untouched.
const MAX_WIDTH: usize = 64;

/// Expands the SegmentTemplate identifiers of §5.3.9.4.4 in one left-to-right
/// pass: `$$`, `$RepresentationID$`, and `$Number$`, `$Bandwidth$`, `$Time$`
/// with an optional `%0<width>d` format tag. Unknown identifiers stay as they are.
pub(super) fn replace_template(
    template: &str,
    rep_id: &str,
    number: u64,
    bandwidth: u64,
    time: u64,
) -> String {
    let mut out = String::with_capacity(template.len() + 16);
    let mut rest = template;
    while let Some(start) = rest.find('$') {
        out.push_str(&rest[..start]);
        let after = &rest[start + 1..];
        let Some(end) = after.find('$') else {
            out.push_str(&rest[start..]);
            return out;
        };
        let token = &after[..end];
        match expand(token, rep_id, number, bandwidth, time) {
            Some(value) => out.push_str(&value),
            None => {
                out.push('$');
                out.push_str(token);
                out.push('$');
            }
        }
        rest = &after[end + 1..];
    }
    out.push_str(rest);
    out
}

fn expand(token: &str, rep_id: &str, number: u64, bandwidth: u64, time: u64) -> Option<String> {
    if token.is_empty() {
        return Some("$".into());
    }
    let (name, format) = match token.find('%') {
        Some(i) => (&token[..i], Some(&token[i + 1..])),
        None => (token, None),
    };
    let value = match name {
        // The format tag is not allowed on RepresentationID.
        "RepresentationID" => return format.is_none().then(|| rep_id.to_owned()),
        "Number" => number,
        "Bandwidth" => bandwidth,
        "Time" => time,
        _ => return None,
    };
    let Some(format) = format else {
        return Some(value.to_string());
    };
    let digits = format.strip_suffix('d')?;
    if digits.is_empty() || !digits.bytes().all(|b| b.is_ascii_digit()) {
        return None;
    }
    match digits.strip_prefix('0') {
        Some(width) if !width.is_empty() => {
            let width: usize = width.parse().ok().filter(|&w| w <= MAX_WIDTH)?;
            Some(format!("{value:0width$}"))
        }
        // `%5d` has no zero flag: printf pads with spaces, players use the plain value.
        _ => Some(value.to_string()),
    }
}

/// Resolves `reference` against `base` (RFC 3986 §5.2). Without an absolute base
/// both stay relative: the reference replaces the base's last path segment.
pub(super) fn join(base: Option<&str>, reference: &str) -> String {
    let Some(base) = base else {
        return reference.to_owned();
    };
    if Url::parse(reference).is_ok() {
        return reference.to_owned();
    }
    if let Ok(b) = Url::parse(base) {
        return b
            .join(reference)
            .map(String::from)
            .unwrap_or_else(|_| reference.to_owned());
    }
    if reference.starts_with('/') || reference.is_empty() && base.is_empty() {
        return reference.to_owned();
    }
    let dir = &base[..base.rfind('/').map_or(0, |i| i + 1)];
    format!("{dir}{reference}")
}
