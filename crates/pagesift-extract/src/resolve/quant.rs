//! Deterministic quantitative, currency, billing-period, and unit extraction + comparison
//! for 1.7 Resolve Check (small-scale verification only; reuses `pagesift_graph::norm`).

use pagesift_graph::norm::{
    MoneyPeriod, NormalizedValue, ValueComparison, canonical_currency_code, compare_normalized,
};
use pagesift_graph::resolve::{QuantitativeCheck, QuantitativeMismatchKind};

// [协议] 固定点标度因子 10_000（与 `pagesift_graph::norm::NormalizedValue` 的 `amount_e4`/`value_e4` 一致）
const SCALE_E4: f64 = 10_000.0;

/// Extracted typed quantity from natural language or a structured table cell.
#[derive(Debug, Clone, PartialEq)]
pub struct ExtractedQuantity {
    pub value: NormalizedValue,
    pub raw_span: String,
    pub is_money: bool,
    pub canonical_unit_dim: Option<UnitDim>,
    /// Value normalized to base unit within `canonical_unit_dim` (e.g. bytes, milliseconds) when comparable.
    pub base_scalar: Option<f64>,
}

/// Physical/computing unit dimension for safe cross-unit comparison (never confuses MB vs GB or ms vs s).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum UnitDim {
    BytesBinary,
    BytesDecimal,
    DurationMs,
    Percent,
    RatePerSec,
}

/// Extract all monetary amounts and unit-bearing scalar quantities from `text`.
pub fn extract_quantities(text: &str) -> Vec<ExtractedQuantity> {
    let mut out = Vec::new();
    let lower = text.to_ascii_lowercase();
    let chars: Vec<char> = text.chars().collect();
    let n = chars.len();
    let mut i = 0usize;

    while i < n {
        // 1. Prefix currency symbol (`$99/mo`, `€49.50 per year`, `¥299/年`, `£20`)
        if let Some(sym_cur) = prefix_currency_symbol(chars[i]) {
            let start = i;
            i += 1;
            while i < n && chars[i].is_whitespace() {
                i += 1;
            }
            if let Some((num_val, end_num)) = parse_number_at(&chars, i) {
                let (period, end_full) = scan_trailing_period(&chars, end_num);
                let raw_span: String = chars[start..end_full].iter().collect();
                let amount_e4 = (num_val * SCALE_E4).round() as i64;
                out.push(ExtractedQuantity {
                    value: NormalizedValue::Money {
                        amount_e4,
                        currency: sym_cur.to_owned(),
                        period,
                        raw: raw_span.clone(),
                    },
                    raw_span,
                    is_money: true,
                    canonical_unit_dim: None,
                    base_scalar: Some(num_val),
                });
                i = end_full;
                continue;
            }
        }

        // 2. Number followed by currency/unit/period (`99 USD/mo`, `99 美元/月`, `64 MiB`, `100 ms`, `365 days`, `99.9%`)
        if chars[i].is_ascii_digit()
            && (i == 0 || !chars[i - 1].is_alphanumeric())
            && let Some((num_val, end_num)) = parse_number_at(&chars, i)
        {
            // Skip bare 4-digit calendar years (1970..=2099) unless followed by a currency/unit.
            let raw_num: String = chars[i..end_num].iter().collect();
            // [协议] 公历 4 位业务年份范围 1970..=2099，无紧随币种/单位时不视作普通度量值
            let looks_like_year = raw_num.len() == 4
                && raw_num.bytes().all(|b| b.is_ascii_digit())
                && (1970.0..=2099.0).contains(&num_val);

            let mut j = end_num;
            while j < n && chars[j] == ' ' {
                j += 1;
            }

            // Check ISO currency prefix immediately before the number (`USD 99/mo`, `EUR 49`)
            let prev_cur = lookbehind_currency(&chars, i);
            let (trail_cur, after_cur) = scan_trailing_currency(&chars, j);

            if let Some(cur) = trail_cur.or(prev_cur) {
                let period_start = if after_cur > j { after_cur } else { end_num };
                let (period, end_full) = scan_trailing_period(&chars, period_start);
                let raw_span: String = chars[i..end_full].iter().collect();
                let amount_e4 = (num_val * SCALE_E4).round() as i64;
                out.push(ExtractedQuantity {
                    value: NormalizedValue::Money {
                        amount_e4,
                        currency: cur,
                        period,
                        raw: raw_span.clone(),
                    },
                    raw_span,
                    is_money: true,
                    canonical_unit_dim: None,
                    base_scalar: Some(num_val),
                });
                i = end_full;
                continue;
            }

            // Check trailing physical/computing unit (`%`, `MiB`, `GB`, `ms`, `s`, `days`, `rps`, ...)
            if let Some((unit_canon, dim, mult, end_u)) = scan_trailing_unit(&chars, end_num) {
                let raw_span: String = chars[i..end_u].iter().collect();
                let value_e4 = (num_val * SCALE_E4).round() as i64;
                out.push(ExtractedQuantity {
                    value: NormalizedValue::Number {
                        value_e4,
                        unit: Some(unit_canon),
                        raw: raw_span.clone(),
                    },
                    raw_span,
                    is_money: false,
                    canonical_unit_dim: dim,
                    base_scalar: Some(num_val * mult),
                });
                i = end_u;
                continue;
            }

            // Standalone price-like number with explicit period (`99/mo`, `99 per year`) -> UNKNOWN currency
            let (period, end_p) = scan_trailing_period(&chars, end_num);
            if period != MoneyPeriod::Unknown {
                let raw_span: String = chars[i..end_p].iter().collect();
                let amount_e4 = (num_val * SCALE_E4).round() as i64;
                out.push(ExtractedQuantity {
                    value: NormalizedValue::Money {
                        amount_e4,
                        currency: "UNKNOWN".to_owned(),
                        period,
                        raw: raw_span.clone(),
                    },
                    raw_span,
                    is_money: true,
                    canonical_unit_dim: None,
                    base_scalar: Some(num_val),
                });
                i = end_p;
                continue;
            }

            // Bare number in a clearly monetary context (`price: 99`, `costs 99`) -> UNKNOWN currency/period
            if !looks_like_year
                && !is_citation_number(&chars, i, end_num)
                && has_nearby_money_keyword(&lower, text, i, &chars)
            {
                let amount_e4 = (num_val * SCALE_E4).round() as i64;
                out.push(ExtractedQuantity {
                    value: NormalizedValue::Money {
                        amount_e4,
                        currency: "UNKNOWN".to_owned(),
                        period: MoneyPeriod::Unknown,
                        raw: raw_num.clone(),
                    },
                    raw_span: raw_num,
                    is_money: true,
                    canonical_unit_dim: None,
                    base_scalar: Some(num_val),
                });
                i = end_num;
                continue;
            }
            i = end_num;
            continue;
        }

        i += 1;
    }

    out
}

/// Compare two [`ExtractedQuantity`] instances strictly separating value match, billing-period
/// conflict, currency/unit conflict, and unspecified unit/currency ambiguity.
pub fn compare_quantities(
    claimed: &ExtractedQuantity,
    observed: &ExtractedQuantity,
) -> QuantitativeCheck {
    // 1. Both monetary: check currency, billing period, and amount.
    if let (
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
    ) = (&claimed.value, &observed.value)
    {
        let cur_conflict = c1 != "UNKNOWN" && c2 != "UNKNOWN" && c1 != c2;
        let period_conflict =
            *p1 != MoneyPeriod::Unknown && *p2 != MoneyPeriod::Unknown && p1 != p2;
        if cur_conflict {
            return QuantitativeCheck {
                kind: QuantitativeMismatchKind::UnitOrCurrencyMismatch,
                claimed_value: Some(claimed.value.clone()),
                observed_value: Some(observed.value.clone()),
                detail: format!("currency mismatch: claimed {c1} vs observed {c2}"),
            };
        }
        if period_conflict {
            return QuantitativeCheck {
                kind: QuantitativeMismatchKind::BillingPeriodMismatch,
                claimed_value: Some(claimed.value.clone()),
                observed_value: Some(observed.value.clone()),
                detail: format!("billing period mismatch: claimed {p1:?} vs observed {p2:?}"),
            };
        }
        if c1 != c2 || p1 != p2 {
            // If amounts already disagree sharply, report ValueMismatch; otherwise flag unspecified unit/period.
            if a1 != a2 {
                return QuantitativeCheck {
                    kind: QuantitativeMismatchKind::ValueMismatch,
                    claimed_value: Some(claimed.value.clone()),
                    observed_value: Some(observed.value.clone()),
                    detail: format!(
                        "amount mismatch ({} vs {}) with partially unspecified currency/period",
                        claimed.raw_span, observed.raw_span
                    ),
                };
            }
            return QuantitativeCheck {
                kind: QuantitativeMismatchKind::UnitOrPeriodUnspecified,
                claimed_value: Some(claimed.value.clone()),
                observed_value: Some(observed.value.clone()),
                detail: format!(
                    "unspecified currency or billing period between '{}' and '{}'",
                    claimed.raw_span, observed.raw_span
                ),
            };
        }
        if a1 == a2 {
            return QuantitativeCheck {
                kind: QuantitativeMismatchKind::Matched,
                claimed_value: Some(claimed.value.clone()),
                observed_value: Some(observed.value.clone()),
                detail: format!("matched monetary value '{}'", claimed.raw_span),
            };
        }
        return QuantitativeCheck {
            kind: QuantitativeMismatchKind::ValueMismatch,
            claimed_value: Some(claimed.value.clone()),
            observed_value: Some(observed.value.clone()),
            detail: format!(
                "monetary amount mismatch: claimed '{}' vs observed '{}'",
                claimed.raw_span, observed.raw_span
            ),
        };
    }

    // 2. Same physical dimension (e.g. `1000 ms` vs `1 s`, `1024 MiB` vs `1 GiB`): compare normalized base scalars!
    if let (Some(d1), Some(d2), Some(s1), Some(s2)) = (
        claimed.canonical_unit_dim,
        observed.canonical_unit_dim,
        claimed.base_scalar,
        observed.base_scalar,
    ) {
        if d1 != d2 {
            return QuantitativeCheck {
                kind: QuantitativeMismatchKind::UnitOrCurrencyMismatch,
                claimed_value: Some(claimed.value.clone()),
                observed_value: Some(observed.value.clone()),
                detail: format!(
                    "unit dimension mismatch: '{}' ({d1:?}) vs '{}' ({d2:?})",
                    claimed.raw_span, observed.raw_span
                ),
            };
        }
        // [推导] 物理同量纲换算允许 1e-4 相对浮点容差（对应 1/10_000 固定点精度）
        let tol = s1.abs().max(s2.abs()).max(1.0) * 1e-4;
        if (s1 - s2).abs() <= tol {
            return QuantitativeCheck {
                kind: QuantitativeMismatchKind::Matched,
                claimed_value: Some(claimed.value.clone()),
                observed_value: Some(observed.value.clone()),
                detail: format!(
                    "matched quantity '{}' == '{}'",
                    claimed.raw_span, observed.raw_span
                ),
            };
        }
        return QuantitativeCheck {
            kind: QuantitativeMismatchKind::ValueMismatch,
            claimed_value: Some(claimed.value.clone()),
            observed_value: Some(observed.value.clone()),
            detail: format!(
                "quantity value mismatch: claimed '{}' vs observed '{}'",
                claimed.raw_span, observed.raw_span
            ),
        };
    }

    // 3. Fallback to `pagesift_graph::norm::compare_normalized`.
    match compare_normalized(&claimed.value, &observed.value) {
        ValueComparison::Equal => QuantitativeCheck {
            kind: QuantitativeMismatchKind::Matched,
            claimed_value: Some(claimed.value.clone()),
            observed_value: Some(observed.value.clone()),
            detail: format!("matched '{}'", claimed.raw_span),
        },
        ValueComparison::UnitOrCurrencyMismatch => QuantitativeCheck {
            kind: QuantitativeMismatchKind::UnitOrCurrencyMismatch,
            claimed_value: Some(claimed.value.clone()),
            observed_value: Some(observed.value.clone()),
            detail: format!(
                "unit/currency mismatch: claimed '{}' vs observed '{}'",
                claimed.raw_span, observed.raw_span
            ),
        },
        ValueComparison::UnitOrCurrencyUnspecified => QuantitativeCheck {
            kind: QuantitativeMismatchKind::UnitOrPeriodUnspecified,
            claimed_value: Some(claimed.value.clone()),
            observed_value: Some(observed.value.clone()),
            detail: format!(
                "unspecified unit/currency between '{}' and '{}'",
                claimed.raw_span, observed.raw_span
            ),
        },
        ValueComparison::ValueMismatch => QuantitativeCheck {
            kind: QuantitativeMismatchKind::ValueMismatch,
            claimed_value: Some(claimed.value.clone()),
            observed_value: Some(observed.value.clone()),
            detail: format!(
                "value mismatch: claimed '{}' vs observed '{}'",
                claimed.raw_span, observed.raw_span
            ),
        },
    }
}

fn prefix_currency_symbol(ch: char) -> Option<&'static str> {
    match ch {
        '$' => Some("USD"),
        '€' => Some("EUR"),
        '£' => Some("GBP"),
        '¥' | '￥' => Some("CNY"),
        _ => None,
    }
}

fn parse_number_at(chars: &[char], start: usize) -> Option<(f64, usize)> {
    let n = chars.len();
    if start >= n || !chars[start].is_ascii_digit() {
        return None;
    }
    let mut i = start;
    let mut buf = String::new();
    let mut seen_dot = false;
    while i < n {
        let c = chars[i];
        if c.is_ascii_digit() {
            buf.push(c);
            i += 1;
        } else if c == ',' && i + 1 < n && chars[i + 1].is_ascii_digit() {
            i += 1;
        } else if c == '.' && !seen_dot && i + 1 < n && chars[i + 1].is_ascii_digit() {
            seen_dot = true;
            buf.push('.');
            i += 1;
        } else {
            break;
        }
    }
    let mut val: f64 = buf.parse().ok()?;
    if !val.is_finite() {
        return None;
    }
    // Optional compact magnitude suffix (`k`, `m`, `b`) when standalone (`$10k`, `$1.5M`)
    if i < n && (i + 1 == n || !chars[i + 1].is_ascii_alphabetic()) {
        match chars[i] {
            'k' | 'K' => {
                val *= 1_000.0;
                i += 1;
            }
            'm' | 'M' => {
                val *= 1_000_000.0;
                i += 1;
            }
            'b' | 'B' => {
                val *= 1_000_000_000.0;
                i += 1;
            }
            _ => {}
        }
    }
    Some((val, i))
}

fn lookbehind_currency(chars: &[char], num_start: usize) -> Option<String> {
    let mut end = num_start;
    while end > 0 && chars[end - 1].is_whitespace() {
        end -= 1;
    }
    let mut start = end;
    while start > 0 && (chars[start - 1].is_ascii_alphabetic() || chars[start - 1] == '$') {
        start -= 1;
    }
    if start < end {
        let tok: String = chars[start..end].iter().collect();
        return canonical_currency_code(&tok);
    }
    None
}

fn scan_trailing_currency(chars: &[char], start: usize) -> (Option<String>, usize) {
    let n = chars.len();
    if start >= n {
        return (None, start);
    }
    let tail: String = chars[start..n.min(start + 12)].iter().collect(); // [策略] 货币词最长扫描窗 12 字符
    let cjk_map = [
        ("美元", "USD"),
        ("美金", "USD"),
        ("欧元", "EUR"),
        ("英镑", "GBP"),
        ("人民币", "CNY"),
        ("日元", "JPY"),
        ("元", "CNY"),
    ];
    for (pat, code) in cjk_map {
        if tail.starts_with(pat) {
            return (Some(code.to_owned()), start + pat.chars().count());
        }
    }
    let mut end = start;
    while end < n && chars[end].is_ascii_alphabetic() {
        end += 1;
    }
    if end > start {
        let word: String = chars[start..end].iter().collect();
        let low = word.to_ascii_lowercase();
        let mapped = match low.as_str() {
            "usd" | "dollar" | "dollars" => Some("USD"),
            "eur" | "euro" | "euros" => Some("EUR"),
            "gbp" | "pound" | "pounds" => Some("GBP"),
            "cny" | "rmb" | "yuan" => Some("CNY"),
            "jpy" | "yen" => Some("JPY"),
            _ => None,
        };
        if let Some(code) = mapped {
            return (Some(code.to_owned()), end);
        }
    }
    (None, start)
}

fn scan_trailing_period(chars: &[char], start: usize) -> (MoneyPeriod, usize) {
    let n = chars.len();
    let mut j = start;
    while j < n && chars[j] == ' ' {
        j += 1;
    }
    if j >= n {
        return (MoneyPeriod::Unknown, start);
    }
    let tail: String = chars[j..n.min(j + 24)].iter().collect(); // [策略] 计费周期词组最长扫描窗 24 字符
    let low = tail.to_ascii_lowercase();
    let pats: &[(&str, MoneyPeriod)] = &[
        ("/month", MoneyPeriod::Monthly),
        ("/mo", MoneyPeriod::Monthly),
        ("per month", MoneyPeriod::Monthly),
        ("a month", MoneyPeriod::Monthly),
        ("monthly", MoneyPeriod::Monthly),
        ("/月", MoneyPeriod::Monthly),
        ("每月", MoneyPeriod::Monthly),
        ("/year", MoneyPeriod::Yearly),
        ("/yr", MoneyPeriod::Yearly),
        ("per year", MoneyPeriod::Yearly),
        ("a year", MoneyPeriod::Yearly),
        ("annually", MoneyPeriod::Yearly),
        ("annual", MoneyPeriod::Yearly),
        ("yearly", MoneyPeriod::Yearly),
        ("/年", MoneyPeriod::Yearly),
        ("每年", MoneyPeriod::Yearly),
        ("/week", MoneyPeriod::Weekly),
        ("/wk", MoneyPeriod::Weekly),
        ("per week", MoneyPeriod::Weekly),
        ("/周", MoneyPeriod::Weekly),
        ("/day", MoneyPeriod::Daily),
        ("per day", MoneyPeriod::Daily),
        ("/天", MoneyPeriod::Daily),
        ("/hr", MoneyPeriod::Hourly),
        ("/hour", MoneyPeriod::Hourly),
        ("per hour", MoneyPeriod::Hourly),
    ];
    for &(pat, period) in pats {
        if low.starts_with(pat) || tail.starts_with(pat) {
            let pat_chars = pat.chars().count();
            let after = j + pat_chars;
            if pat.is_ascii() && after < n && chars[after].is_ascii_alphabetic() {
                continue;
            }
            return (period, after);
        }
    }
    (MoneyPeriod::Unknown, start)
}

fn scan_trailing_unit(
    chars: &[char],
    start: usize,
) -> Option<(String, Option<UnitDim>, f64, usize)> {
    let n = chars.len();
    let mut j = start;
    while j < n && chars[j] == ' ' {
        j += 1;
    }
    if j >= n {
        return None;
    }
    if chars[j] == '%' {
        return Some(("%".to_owned(), Some(UnitDim::Percent), 1.0, j + 1));
    }
    let mut end = j;
    while end < n && (chars[end].is_ascii_alphabetic() || chars[end] == '/') {
        end += 1;
    }
    if end == j {
        return None;
    }
    let word: String = chars[j..end].iter().collect();
    let low = word.to_ascii_lowercase();
    // [协议] IEC 二进制字节单位换算基数 1024 与 SI 十进制换算基数 1000 及时间单位毫秒换算
    let matched = match low.as_str() {
        "kib" => ("kib", Some(UnitDim::BytesBinary), 1024.0),
        "mib" => ("mib", Some(UnitDim::BytesBinary), 1024.0 * 1024.0),
        "gib" => ("gib", Some(UnitDim::BytesBinary), 1024.0 * 1024.0 * 1024.0),
        "kb" => ("kb", Some(UnitDim::BytesDecimal), 1000.0),
        "mb" => ("mb", Some(UnitDim::BytesDecimal), 1_000_000.0),
        "gb" => ("gb", Some(UnitDim::BytesDecimal), 1_000_000_000.0),
        "tb" => ("tb", Some(UnitDim::BytesDecimal), 1_000_000_000_000.0),
        "ms" | "millis" | "milliseconds" => ("ms", Some(UnitDim::DurationMs), 1.0),
        "s" | "sec" | "secs" | "second" | "seconds" => ("s", Some(UnitDim::DurationMs), 1000.0),
        "min" | "mins" | "minute" | "minutes" => ("min", Some(UnitDim::DurationMs), 60_000.0), // [协议] 1 分钟 = 60,000 毫秒
        "hour" | "hours" | "hr" | "hrs" => ("hr", Some(UnitDim::DurationMs), 3_600_000.0), // [协议] 1 小时 = 3,600,000 毫秒
        "day" | "days" => ("day", Some(UnitDim::DurationMs), 86_400_000.0), // [协议] 1 天 = 86,400,000 毫秒
        "percent" | "pct" => ("%", Some(UnitDim::Percent), 1.0),
        "rps" | "qps" | "req/s" => ("rps", Some(UnitDim::RatePerSec), 1.0),
        "tokens" | "token" => ("tokens", None, 1.0),
        "seats" | "seat" => ("seats", None, 1.0),
        _ => return None,
    };
    Some((matched.0.to_owned(), matched.1, matched.2, end))
}

fn is_citation_number(chars: &[char], start: usize, end: usize) -> bool {
    start > 0 && chars[start - 1] == '[' && end < chars.len() && chars[end] == ']'
}

fn has_nearby_money_keyword(lower: &str, raw: &str, char_pos: usize, chars: &[char]) -> bool {
    let win_start = char_pos.saturating_sub(24); // [策略] 金额关键词上下文窗口 24 字符
    let win_end = (char_pos + 16).min(chars.len());
    let local: String = chars[win_start..win_end].iter().collect();
    let local_low = local.to_ascii_lowercase();
    for kw in [
        "price", "cost", "fee", "salary", "revenue", "售价", "价格", "费用",
    ] {
        if local_low.contains(kw) || local.contains(kw) {
            return true;
        }
    }
    let _ = (lower, raw);
    false
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn distinguishes_monthly_vs_yearly_and_currency_and_unit_conversions() {
        let a = extract_quantities("Product A costs $99/mo");
        let b = extract_quantities("Product A costs $99/yr");
        assert_eq!(a.len(), 1);
        assert_eq!(b.len(), 1);
        let cmp = compare_quantities(&a[0], &b[0]);
        assert_eq!(cmp.kind, QuantitativeMismatchKind::BillingPeriodMismatch);

        let zh_m = extract_quantities("A 产品售价 99 美元/月");
        let zh_y = extract_quantities("A 产品售价 99 美元/年");
        assert_eq!(
            compare_quantities(&zh_m[0], &zh_y[0]).kind,
            QuantitativeMismatchKind::BillingPeriodMismatch
        );
        assert_eq!(
            compare_quantities(&a[0], &zh_m[0]).kind,
            QuantitativeMismatchKind::Matched
        );

        let ms = extract_quantities("Latency is 1000 ms");
        let sec = extract_quantities("Latency is 1 s");
        assert_eq!(
            compare_quantities(&ms[0], &sec[0]).kind,
            QuantitativeMismatchKind::Matched
        );

        let gb = extract_quantities("Memory limit: 16 GB");
        let mb = extract_quantities("Memory limit: 16 MB");
        assert_eq!(
            compare_quantities(&gb[0], &mb[0]).kind,
            QuantitativeMismatchKind::ValueMismatch
        );
    }
}
