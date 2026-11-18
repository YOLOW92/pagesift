//! Dual-axis temporal validity (`asOf` observation time vs `effectiveAt` validity window),
//! exact Gregorian UTC calendar year conversion, unit evidence support, and source independence helpers.

use std::collections::BTreeSet;

use pagesift_graph::context::{KnowledgeUnit, UnitKind, ValidityStatus};
use pagesift_graph::{ConflictRecord, parse_timestamp_ms};

use crate::context::tokenize_unified::{MAX_YEAR, MIN_YEAR, scan_years_into};

/// Fast zero-allocation parser for canonical UTC ISO-8601 `YYYY-MM-DDTHH:MM:SSZ` timestamps,
/// falling back to `pagesift_graph::parse_timestamp_ms` for arbitrary offsets / HTTP dates.
#[inline]
fn parse_ts_fast(s: &str) -> Option<u64> {
    let b = s.trim().as_bytes();
    // [协议] 标准秒级 UTC RFC-3339 格式 `YYYY-MM-DDTHH:MM:SSZ` 固定 20 字节
    if b.len() == 20
        && b[4] == b'-'
        && b[7] == b'-'
        && (b[10] == b'T' || b[10] == b't' || b[10] == b' ')
        && b[13] == b':'
        && b[16] == b':'
        && (b[19] == b'Z' || b[19] == b'z')
    {
        let yr = parse_digits(&b[0..4])? as i64; // [协议] YYYY 偏移 0..4
        let mo = parse_digits(&b[5..7])?; // [协议] MM 偏移 5..7
        let dy = parse_digits(&b[8..10])?; // [协议] DD 偏移 8..10
        let hh = parse_digits(&b[11..13])?; // [协议] HH 偏移 11..13
        let mm = parse_digits(&b[14..16])?; // [协议] mm 偏移 14..16
        let ss = parse_digits(&b[17..19])?; // [协议] ss 偏移 17..19
        // [协议] 公历合法月日时分秒范围上界（12月、31日、23时、59分、59秒）
        if (1970..=9999).contains(&yr)
            && (1..=12).contains(&mo)
            && (1..=31).contains(&dy)
            && hh <= 23
            && mm <= 59
            && ss <= 59
            && dy <= max_days_in_month(yr, mo)
        {
            let days = days_from_civil(yr, mo, dy);
            if days >= 0 {
                let secs = (days as u64) * 86_400
                    + u64::from(hh) * 3_600
                    + u64::from(mm) * 60
                    + u64::from(ss);
                return Some(secs * 1_000);
            }
        }
    }
    parse_timestamp_ms(s)
}

#[inline]
fn parse_digits(slice: &[u8]) -> Option<u32> {
    let mut acc = 0u32;
    for &c in slice {
        if !c.is_ascii_digit() {
            return None;
        }
        // [协议] 十进制位权基数 10 與 ASCII '0' 偏移 48
        acc = acc * 10 + u32::from(c - b'0');
    }
    Some(acc)
}

#[inline]
fn max_days_in_month(year: i64, month: u32) -> u32 {
    match month {
        4 | 6 | 9 | 11 => 30, // [协议] 公历小月 4/6/9/11 为 30 天
        2 => {
            let leap = (year % 4 == 0 && year % 100 != 0) || (year % 400 == 0); // [协议] 格里高利闰年规则
            if leap { 29 } else { 28 } // [协议] 2 月闰年 29 天、平年 28 天
        }
        _ => 31, // [协议] 公历大月 31 天
    }
}

#[inline]
fn days_from_civil(year: i64, month: u32, day: u32) -> i64 {
    // [协议] 1/2 月视作上一公历年的第 13/14 月以对齐闰日尾置（Hinnant 2021）
    let y = year - i64::from(month <= 2);
    // [协议] 400 年完整闰周期
    let era = if y >= 0 { y } else { y - 399 } / 400;
    let yoe = (y - era * 400) as u64; // [协议] 400 年周期内年份偏移
    let m = u64::from(month);
    let d = u64::from(day);
    // [协议] 3 月起算月份偏移（m > 2 减 3，否则加 9）
    let doy = (153 * (if m > 2 { m - 3 } else { m + 9 }) + 2) / 5 + d - 1;
    // [协议] 400 年纪元内总天数（平年 365 + 4年一闰 - 100年不闰）
    let doe = yoe * 365 + yoe / 4 - yoe / 100 + doy;
    era * 146_097 + (doe as i64) - 719_468 // [协议] 146,097 天纪元跨度与 1970-01-01 偏移 719,468
}

/// Exact proleptic Gregorian UTC calendar year from Unix epoch milliseconds (Howard Hinnant algorithm).
/// Eliminates Julian-year `365.25d` boundary drift around Dec 31 / Jan 1 across leap cycles.
pub(super) fn utc_year_from_unix_ms(ms: u64) -> u64 {
    // [协议] 每日毫秒数 86,400 * 1,000 = 86,400,000
    let days = (ms / 86_400_000) as i64;
    // [协议] 0000-03-01 至 1970-01-01 的格里高利历天数偏移常数 719,468（Hinnant 2021）
    let z = days + 719_468;
    // [协议] 格里高利历每 400 年精确包含 146,097 天（97 个闰年）
    let era = if z >= 0 { z } else { z - 146_096 } / 146_097;
    let doe = (z - era * 146_097) as u64; // [协议] 同上 400 年周期内天数偏移
    // [协议] 400 年周期内闰日修正项：1,460 (4年)、36,524 (100年)、146,096 (400年末日)、365 (平年天数)
    let yoe = (doe - doe / 1_460 + doe / 36_524 - doe / 146_096) / 365;
    // [协议] 400 年纪元跨度还原公历年份基数
    let y = (yoe as i64) + era * 400;
    // [协议] 4 年一闰与 100 年不闰修正计算自 3 月 1 日起的年内天数
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    // [协议] 5/153 线性分段映射自 3 月起的月份索引（0..=11）
    let mp = (5 * doy + 2) / 153;
    // [协议] 将 3 月起算的月份索引还原为公历 1..=12 月（mp < 10 对应 3..=12 月，否则对应次年 1..=2 月）
    let m = if mp < 10 { mp + 3 } else { mp - 9 };
    (y + i64::from(m <= 2)).max(0) as u64
}

pub(super) fn collect_unit_years(u: &KnowledgeUnit) -> BTreeSet<u64> {
    let mut years = BTreeSet::new();
    for a in &u.entity_anchors {
        if let Some(y_str) = a.strip_prefix("year:")
            && let Ok(y) = y_str.parse::<u64>()
        {
            years.insert(y);
        }
    }
    for h in &u.location.heading_path {
        scan_years_into(h, &mut years);
    }
    if let Some(temp) = &u.temporal {
        let from_ms = temp.valid_from.as_deref().and_then(parse_ts_fast);
        let until_ms = temp.valid_until.as_deref().and_then(parse_ts_fast);
        match (from_ms, until_ms) {
            (Some(f), Some(e)) if f <= e => {
                let yf = utc_year_from_unix_ms(f);
                let ye = utc_year_from_unix_ms(e);
                // [策略] 跨度不超过 16 年的显式有效期展开覆盖年份集合
                if ye.saturating_sub(yf) <= 16 {
                    for yr in yf..=ye {
                        years.insert(yr);
                    }
                } else {
                    years.insert(yf);
                    years.insert(ye);
                }
            }
            (Some(f), _) => {
                years.insert(utc_year_from_unix_ms(f));
            }
            (None, Some(e)) => {
                years.insert(utc_year_from_unix_ms(e));
            }
            _ => {}
        }
    }
    years
}

pub(super) fn extract_query_years(query_anchors: &[String]) -> BTreeSet<u64> {
    let mut out = BTreeSet::new();
    for a in query_anchors {
        if let Some(y_str) = a.strip_prefix("year:")
            && let Ok(yr) = y_str.parse::<u32>()
            && (MIN_YEAR..=MAX_YEAR).contains(&yr)
        {
            out.insert(u64::from(yr));
        }
    }
    out
}

/// Evaluate validity of `u` strictly separating `as_of_ms` (observation cutoff) from `effective_at_ms`
/// (business validity target), without fabricating certainty when validity is unknown!
pub(super) fn evaluate_validity(
    u: &KnowledgeUnit,
    effective_at_ms: Option<u64>,
    as_of_ms: Option<u64>,
    query_years: &BTreeSet<u64>,
) -> ValidityStatus {
    let (from_ms, until_ms) = match &u.temporal {
        Some(temp) => (
            temp.valid_from.as_deref().and_then(parse_ts_fast),
            temp.valid_until.as_deref().and_then(parse_ts_fast),
        ),
        None => (None, None),
    };

    if let Some(target_ms) = effective_at_ms {
        if let Some(f) = from_ms
            && target_ms < f
        {
            return ValidityStatus::NotYetValidAtTarget;
        }
        if let Some(end) = until_ms
            && target_ms > end
        {
            return ValidityStatus::ExpiredAtTarget;
        }
        if from_ms.is_some() || until_ms.is_some() {
            return ValidityStatus::ValidAtTarget;
        }

        let target_year = utc_year_from_unix_ms(target_ms);
        let u_years = collect_unit_years(u);
        if !u_years.is_empty() {
            if u_years.contains(&target_year) {
                return ValidityStatus::ValidAtTarget;
            }
            if u_years.iter().all(|&y| y > target_year) {
                return ValidityStatus::NotYetValidAtTarget;
            }
            if u_years.iter().all(|&y| y < target_year) {
                return ValidityStatus::ExpiredAtTarget;
            }
        }
        return ValidityStatus::UnknownValidity;
    }

    // Query explicitly names calendar year(s) (e.g. `"2025 enterprise pricing"`) without `effective_at_ms`
    if !query_years.is_empty() {
        let u_years = collect_unit_years(u);
        if !u_years.is_empty() {
            if !u_years.is_disjoint(query_years) {
                return ValidityStatus::ValidAtTarget;
            }
            let q_max = *query_years.iter().next_back().unwrap_or(&0);
            if u_years.iter().all(|&y| y > q_max) {
                return ValidityStatus::NotYetValidAtTarget;
            }
            return ValidityStatus::ExpiredAtTarget;
        }
    }

    // No explicit `effective_at_ms`: check parsed validity bounds against reference observation time
    // (`as_of_ms` if provided, otherwise the unit's own `observed_at_ms`) so archival expired windows
    // do not masquerade as currently valid at `1.0`!
    if from_ms.is_some() || until_ms.is_some() {
        let ref_ms = as_of_ms.unwrap_or(u.observed_at_ms);
        if ref_ms > 0 {
            if let Some(end) = until_ms
                && ref_ms > end
            {
                return ValidityStatus::ExpiredAtTarget;
            }
            if let Some(f) = from_ms
                && ref_ms < f
            {
                return ValidityStatus::NotYetValidAtTarget;
            }
        }
        ValidityStatus::ValidAtTarget
    } else {
        ValidityStatus::UnknownValidity
    }
}

pub(super) fn freshness_score_for_validity(
    u: &KnowledgeUnit,
    validity: ValidityStatus,
    effective_at_ms: Option<u64>,
) -> f32 {
    match validity {
        ValidityStatus::ValidAtTarget => 1.0,
        ValidityStatus::UnknownValidity => {
            if let Some(eff_ms) = effective_at_ms {
                let target_year = utc_year_from_unix_ms(eff_ms);
                let u_years = collect_unit_years(u);
                if !u_years.is_empty() && !u_years.contains(&target_year) {
                    // [策略] 缺少显式 validFrom 但文本/标题显式年份与目标时间点冲突时降权至 0.20
                    0.20
                } else {
                    // [策略] 调用方显式指定 effectiveAt 时，未声明有效期的单元得分 0.45，低于显式命中窗口的单元（1.0）
                    0.45
                }
            } else {
                // [策略] 未指定 effectiveAt 时未知有效期给 0.75 中性偏高分，不伪造 1.0 确定性
                0.75
            }
        }
        // [策略] 已过期或尚未生效单元显著降权至 0.10
        ValidityStatus::ExpiredAtTarget | ValidityStatus::NotYetValidAtTarget => 0.10,
    }
}

pub(super) fn compute_unit_support(u: &KnowledgeUnit, required_fields: &[String]) -> f32 {
    let mut base = if !u.evidence_spans.is_empty() && u.evidence_spans.iter().any(|e| e.verified) {
        1.0f32
    } else if !u.claim_ids.is_empty() || matches!(u.kind, UnitKind::Table | UnitKind::CodeBlock) {
        // [策略] 结构化表格/代码块或带声明绑定赋 0.85 基础证据支持度
        0.85f32
    } else {
        // [策略] 普通正文块基础支持度 0.60
        0.60f32
    };
    if !u.status.is_verified_active() {
        base *= 0.5;
    }
    if !required_fields.is_empty() {
        let lower_text = u.text.to_ascii_lowercase();
        let matched = required_fields
            .iter()
            .filter(|req_f| {
                let low = req_f.to_ascii_lowercase();
                u.evidence_spans
                    .iter()
                    .any(|ev| ev.field_path.to_ascii_lowercase().contains(&low))
                    || lower_text.contains(&low)
            })
            .count();
        let ratio = matched as f32 / required_fields.len() as f32;
        base = 0.5 * base + 0.5 * ratio;
    }
    base.clamp(0.0, 1.0)
}

/// Normalize an evidentiary origin root key so scheme/case/trailing-slash differences never inflate counts.
pub(crate) fn canonical_origin_root(raw: &str) -> String {
    let t = raw.trim().trim_end_matches('/');
    let stripped = t
        .strip_prefix("https://")
        .or_else(|| t.strip_prefix("http://"))
        .unwrap_or(t)
        .trim_end_matches('/');
    stripped.to_ascii_lowercase()
}

/// Verify that a [`ConflictRecord`] represents genuine disagreement across >= 2 independent sources
/// rather than syndicated mirrors or derivative copies sharing the same origin!
pub(super) fn is_genuine_independent_conflict<'a, F>(
    conflict: &ConflictRecord,
    mut lookup_doc_origin: F,
) -> bool
where
    F: FnMut(&str) -> Option<(&'a str, bool)>,
{
    if conflict.variants.len() < 2 {
        return false;
    }
    let indep_variants = conflict
        .variants
        .iter()
        .filter(|v| v.independent_source_count > 0)
        .count();
    if indep_variants < 2 {
        return false;
    }

    // If evidence spans map to indexed units, ensure at least 2 distinct independent origin roots exist
    // when all variants are resolved in the index (rejects two mirrors of the same origin root!).
    let mut resolved_variants = 0usize;
    let mut distinct_indep_roots: BTreeSet<String> = BTreeSet::new();
    for var in &conflict.variants {
        let mut var_resolved = false;
        for ev in &var.evidence {
            if let Some((root, indep)) = lookup_doc_origin(&ev.doc_id) {
                var_resolved = true;
                if indep && !root.trim().is_empty() {
                    distinct_indep_roots.insert(canonical_origin_root(root));
                }
            }
        }
        if var_resolved {
            resolved_variants += 1;
        }
    }
    if resolved_variants >= 2 && distinct_indep_roots.len() < 2 {
        return false;
    }
    true
}
