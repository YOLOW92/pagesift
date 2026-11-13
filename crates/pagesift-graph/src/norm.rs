//! Deterministic typed value normalization for cross-source comparison (preserves `raw`).

use serde::{Deserialize, Serialize};
use serde_json::Value;

/// Billing or recurrence period extracted alongside monetary values.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum MoneyPeriod {
    OneTime,
    Monthly,
    Yearly,
    Weekly,
    Daily,
    Hourly,
    Unknown,
}

/// Precision level of a normalized ISO date.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum DatePrecision {
    Year,
    YearMonth,
    YearMonthDay,
}

/// Typed normalized representation paired with the raw source string.
#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "camelCase")]
pub enum NormalizedValue {
    Money {
        /// Fixed-point in 1/10,000 currency units (avoids float rounding errors).
        amount_e4: i64,
        currency: String,
        period: MoneyPeriod,
        raw: String,
    },
    Date {
        iso: String,
        precision: DatePrecision,
        raw: String,
    },
    Number {
        value_e4: i64,
        #[serde(skip_serializing_if = "Option::is_none")]
        unit: Option<String>,
        raw: String,
    },
    Boolean {
        value: bool,
        raw: String,
    },
    Url {
        normalized: String,
        raw: String,
    },
    Identifier {
        canonical: String,
        raw: String,
    },
    Text {
        canonical: String,
        raw: String,
    },
}

impl NormalizedValue {
    /// Canonical bucket key excluding `raw` formatting differences (used to group identical claims).
    pub fn bucket_key(&self) -> String {
        match self {
            Self::Money {
                amount_e4,
                currency,
                period,
                ..
            } => format!("money:{amount_e4}:{currency}:{period:?}"),
            Self::Date { iso, .. } => format!("date:{iso}"),
            Self::Number { value_e4, unit, .. } => {
                format!("num:{value_e4}:{}", unit.as_deref().unwrap_or(""))
            }
            Self::Boolean { value, .. } => format!("bool:{value}"),
            Self::Url { normalized, .. } => format!("url:{normalized}"),
            Self::Identifier { canonical, .. } => format!("id:{canonical}"),
            Self::Text { canonical, .. } => format!("txt:{canonical}"),
        }
    }

    pub fn raw(&self) -> &str {
        match self {
            Self::Money { raw, .. }
            | Self::Date { raw, .. }
            | Self::Number { raw, .. }
            | Self::Boolean { raw, .. }
            | Self::Url { raw, .. }
            | Self::Identifier { raw, .. }
            | Self::Text { raw, .. } => raw,
        }
    }
}

/// Pairwise comparison result between two normalized claim values.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ValueComparison {
    Equal,
    UnitOrCurrencyMismatch,
    ValueMismatch,
}

/// Fold camelCase/snake_case/whitespace field names to a stable lowercase snake key.
pub fn normalize_field_key(name: &str) -> String {
    let mut out = String::with_capacity(name.len());
    let mut prev_lower = false;
    for ch in name.trim().chars() {
        if ch.is_ascii_alphanumeric() {
            if ch.is_ascii_uppercase() && prev_lower && !out.ends_with('_') {
                out.push('_');
            }
            out.push(ch.to_ascii_lowercase());
            prev_lower = ch.is_ascii_lowercase() || ch.is_ascii_digit();
        } else if !out.is_empty() && !out.ends_with('_') {
            out.push('_');
            prev_lower = false;
        }
    }
    out.trim_matches('_').to_owned()
}

/// Normalize a field value into a typed representation while keeping `raw`.
pub fn normalize_field_value(field_name: &str, value: &Value) -> NormalizedValue {
    match value {
        Value::Bool(b) => NormalizedValue::Boolean {
            value: *b,
            raw: b.to_string(),
        },
        Value::Number(n) => {
            let raw = n.to_string();
            if is_date_hint(field_name)
                && let Some((iso, precision)) = parse_date_str(&raw, true)
            {
                return NormalizedValue::Date {
                    iso,
                    precision,
                    raw,
                };
            }
            if let Some(amount_e4) = parse_scaled_e4(&raw, 1) {
                NormalizedValue::Number {
                    value_e4: amount_e4,
                    unit: None,
                    raw,
                }
            } else {
                NormalizedValue::Identifier {
                    canonical: raw.clone(),
                    raw,
                }
            }
        }
        Value::String(s) => normalize_string_value(field_name, s),
        other => {
            let raw = other.to_string();
            NormalizedValue::Text {
                canonical: raw.to_ascii_lowercase(),
                raw,
            }
        }
    }
}

fn normalize_string_value(field_name: &str, raw: &str) -> NormalizedValue {
    let trimmed = raw.trim();
    if matches!(trimmed.to_ascii_lowercase().as_str(), "true" | "false") {
        return NormalizedValue::Boolean {
            value: trimmed.eq_ignore_ascii_case("true"),
            raw: trimmed.to_owned(),
        };
    }
    if (trimmed.starts_with("http://") || trimmed.starts_with("https://"))
        && let Ok(parsed) = url::Url::parse(trimmed)
    {
        return NormalizedValue::Url {
            normalized: crate::hash::normalize_url(parsed.as_str()),
            raw: trimmed.to_owned(),
        };
    }
    if let Some((amount_e4, currency, period)) = parse_money(field_name, trimmed) {
        return NormalizedValue::Money {
            amount_e4,
            currency,
            period,
            raw: trimmed.to_owned(),
        };
    }
    if let Some((iso, precision)) = parse_date_str(trimmed, is_date_hint(field_name)) {
        return NormalizedValue::Date {
            iso,
            precision,
            raw: trimmed.to_owned(),
        };
    }
    if let Some((value_e4, unit)) = parse_number_with_unit(trimmed) {
        return NormalizedValue::Number {
            value_e4,
            unit,
            raw: trimmed.to_owned(),
        };
    }
    if is_identifier_field(field_name) {
        return NormalizedValue::Identifier {
            canonical: trimmed.to_ascii_uppercase(),
            raw: trimmed.to_owned(),
        };
    }
    NormalizedValue::Text {
        canonical: collapse_lower(trimmed),
        raw: trimmed.to_owned(),
    }
}

/// Compare two normalized values and flag unit/currency/period discrepancies distinctly.
pub fn compare_normalized(a: &NormalizedValue, b: &NormalizedValue) -> ValueComparison {
    match (a, b) {
        (
            NormalizedValue::Money {
                amount_e4: a1,
                currency: c1,
                period: p1,
                ..
            },
            NormalizedValue::Money {
                amount_e4: a2,
                currency: c2,
                period: p2,
                ..
            },
        ) => {
            if c1 != c2 || p1 != p2 {
                ValueComparison::UnitOrCurrencyMismatch
            } else if a1 == a2 {
                ValueComparison::Equal
            } else {
                ValueComparison::ValueMismatch
            }
        }
        (
            NormalizedValue::Number {
                value_e4: v1,
                unit: u1,
                ..
            },
            NormalizedValue::Number {
                value_e4: v2,
                unit: u2,
                ..
            },
        ) => {
            if u1 != u2 {
                ValueComparison::UnitOrCurrencyMismatch
            } else if v1 == v2 {
                ValueComparison::Equal
            } else {
                ValueComparison::ValueMismatch
            }
        }
        (NormalizedValue::Money { .. }, NormalizedValue::Number { .. })
        | (NormalizedValue::Number { .. }, NormalizedValue::Money { .. }) => {
            ValueComparison::UnitOrCurrencyMismatch
        }
        _ => {
            if a.bucket_key() == b.bucket_key() {
                ValueComparison::Equal
            } else {
                ValueComparison::ValueMismatch
            }
        }
    }
}

fn is_date_hint(field_name: &str) -> bool {
    let k = normalize_field_key(field_name);
    k.contains("date")
        || k.contains("time")
        || k.contains("year")
        || k == "founded"
        || k == "published"
}

fn is_identifier_field(field_name: &str) -> bool {
    let k = normalize_field_key(field_name);
    matches!(
        k.as_str(),
        "sku" | "mpn" | "gtin" | "gtin13" | "isbn" | "doi" | "id" | "lei_code" | "ticker"
    )
}

fn is_money_hint(field_name: &str) -> bool {
    let k = normalize_field_key(field_name);
    k.contains("price")
        || k.contains("cost")
        || k.contains("fee")
        || k.contains("revenue")
        || k.contains("salary")
}

fn parse_money(field_name: &str, s: &str) -> Option<(i64, String, MoneyPeriod)> {
    let lower = s.to_ascii_lowercase();
    let (period, body) = extract_period(&lower, s);
    let mut currency = detect_currency(body);
    if currency.is_none() && is_money_hint(field_name) {
        currency = Some("USD".to_owned());
    }
    let currency = currency?;
    let numeric_part = strip_currency_tokens(body.trim());
    let (num_str, mult) = extract_magnitude_suffix(&numeric_part);
    let amount_e4 = parse_scaled_e4(num_str, mult)?;
    Some((amount_e4, currency, period))
}

fn extract_period<'a>(lower: &str, orig: &'a str) -> (MoneyPeriod, &'a str) {
    let patterns = [
        ("/month", MoneyPeriod::Monthly),
        ("/mo", MoneyPeriod::Monthly),
        ("per month", MoneyPeriod::Monthly),
        ("monthly", MoneyPeriod::Monthly),
        ("每月", MoneyPeriod::Monthly),
        ("/year", MoneyPeriod::Yearly),
        ("/yr", MoneyPeriod::Yearly),
        ("per year", MoneyPeriod::Yearly),
        ("annually", MoneyPeriod::Yearly),
        ("annual", MoneyPeriod::Yearly),
        ("yearly", MoneyPeriod::Yearly),
        ("每年", MoneyPeriod::Yearly),
        ("/week", MoneyPeriod::Weekly),
        ("/wk", MoneyPeriod::Weekly),
        ("/day", MoneyPeriod::Daily),
        ("/hr", MoneyPeriod::Hourly),
    ];
    for (pat, period) in patterns {
        if let Some(pos) = lower.rfind(pat) {
            return (period, orig[..pos].trim());
        }
    }
    (MoneyPeriod::Unknown, orig)
}

fn detect_currency(s: &str) -> Option<String> {
    let upper = s.to_ascii_uppercase();
    for code in [
        "USD", "EUR", "GBP", "JPY", "CNY", "RMB", "CHF", "CAD", "AUD", "KRW",
    ] {
        if upper.contains(code) {
            return Some(if code == "RMB" { "CNY" } else { code }.to_owned());
        }
    }
    if s.contains('$') {
        Some("USD".into())
    } else if s.contains('€') {
        Some("EUR".into())
    } else if s.contains('£') {
        Some("GBP".into())
    } else if s.contains('¥') || s.contains('￥') {
        Some("CNY".into())
    } else {
        None
    }
}

fn strip_currency_tokens(s: &str) -> String {
    let mut out = s.to_owned();
    for tok in [
        "US$", "USD", "EUR", "GBP", "JPY", "CNY", "RMB", "CHF", "CAD", "AUD", "KRW", "$", "€", "£",
        "¥", "￥", "元",
    ] {
        out = out.replace(tok, "");
    }
    out.trim().to_owned()
}

fn extract_magnitude_suffix(s: &str) -> (&str, i64) {
    let t = s.trim();
    if let Some(rest) = t.strip_suffix(['k', 'K']) {
        return (rest.trim(), 1_000);
    }
    if let Some(rest) = t.strip_suffix(['m', 'M']) {
        return (rest.trim(), 1_000_000);
    }
    if let Some(rest) = t.strip_suffix(['b', 'B']) {
        return (rest.trim(), 1_000_000_000);
    }
    (t, 1)
}

use crate::norm_parse::{collapse_lower, parse_date_str, parse_number_with_unit, parse_scaled_e4};

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn distinguishes_monthly_and_yearly_pricing_and_dates() {
        let m = normalize_field_value("price", &json!("$49/mo"));
        let y = normalize_field_value("price", &json!("$490/yr"));
        assert_eq!(
            compare_normalized(&m, &y),
            ValueComparison::UnitOrCurrencyMismatch
        );

        let same_m = normalize_field_value("price", &json!("USD 49.00 per month"));
        assert_eq!(compare_normalized(&m, &same_m), ValueComparison::Equal);

        let d1 = normalize_field_value("published_time", &json!("Oct 21, 2026"));
        let d2 = normalize_field_value("publishedTime", &json!("2026年10月21日"));
        assert_eq!(compare_normalized(&d1, &d2), ValueComparison::Equal);
        assert_eq!(normalize_field_key("unitPrice"), "unit_price");
    }
}
