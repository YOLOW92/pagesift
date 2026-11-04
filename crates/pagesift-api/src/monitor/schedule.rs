//! `schedule` 的周期：Firecrawl 的 cron（常见的等间隔写法）和自然语言文本，换算成秒。
//!
//! 给了 cron 或文本时，监控按这个周期检查（最短、最长间隔都等于它，和 Firecrawl 一样）；
//! pagesift 扩展 `minIntervalMs`、`maxIntervalMs`（毫秒）给出时，在这个区间里按变化率的后验自适应。

use crate::error::ApiError;

/// [协议] Firecrawl 文档：“The minimum interval is 5 minutes.”
pub const MIN_INTERVAL_SECS: u64 = 300;
/// [策略] 最长间隔一年：再长的监控没有意义，算到期时间也不会溢出
pub const MAX_INTERVAL_SECS: u64 = 366 * DAY;

const MINUTE: u64 = 60;
const HOUR: u64 = 3600;
const DAY: u64 = 86400;
/// [协议] 一周 7 天。
const WEEK: u64 = 7 * DAY;

/// 等间隔的 cron 写法：`*/N * * * *`、`M * * * *`、`M */N * * *`、`M H * * *`、`M H * * D`。
/// 其他写法（非等间隔，如 `0 9,17 * * *`）报错，不悄悄近似。
pub fn cron_period(cron: &str) -> Result<u64, ApiError> {
    let f: Vec<&str> = cron.split_whitespace().collect();
    let bad = || {
        ApiError::invalid(format!(
            "schedule.cron {cron:?}: only evenly spaced schedules are supported (*/N * * * *, M * * * *, M */N * * *, M H * * *, M H * * D)"
        ))
    };
    // [协议] cron 的五个字段：分、时、日、月、星期
    if f.len() != 5 {
        return Err(bad());
    }
    let num = |s: &str| s.parse::<u64>().ok();
    let every = |s: &str| s.strip_prefix("*/").and_then(num);
    let rest_any = f[2] == "*" && f[3] == "*";
    match (f[0], f[1], rest_any, f[4]) {
        (m, "*", true, "*") if every(m).is_some() => {
            Ok(every(m).unwrap_or(1).saturating_mul(MINUTE))
        }
        ("*", "*", true, "*") => Ok(MINUTE),
        (m, "*", true, "*") if num(m).is_some() => Ok(HOUR),
        (m, h, true, "*") if num(m).is_some() && every(h).is_some() => {
            Ok(every(h).unwrap_or(1).saturating_mul(HOUR))
        }
        (m, h, true, "*") if num(m).is_some() && num(h).is_some() => Ok(DAY),
        (m, h, true, d) if num(m).is_some() && num(h).is_some() && num(d).is_some() => Ok(WEEK),
        _ => Err(bad()),
    }
}

/// Firecrawl 文档列出的文本写法：`every N minutes`、`hourly`、`every N hours`、`daily`、
/// `daily at …`、`weekly`（`starting at :07` 之类的相位不影响周期）。
pub fn text_period(text: &str) -> Result<u64, ApiError> {
    let t = text.trim().to_ascii_lowercase();
    let words: Vec<&str> = t.split_whitespace().collect();
    let n = |i: usize| words.get(i).and_then(|w| w.parse::<u64>().ok());
    let period = match words.as_slice() {
        ["hourly", ..] => Some(HOUR),
        ["daily", ..] => Some(DAY),
        ["weekly", ..] => Some(WEEK),
        ["every", "minute", ..] => Some(MINUTE),
        ["every", "hour", ..] => Some(HOUR),
        ["every", "day", ..] => Some(DAY),
        ["every", "week", ..] => Some(WEEK),
        ["every", _, unit, ..] => n(1).and_then(|k| {
            let u = unit.trim_end_matches('s');
            match u {
                "minute" | "min" => Some(k.saturating_mul(MINUTE)),
                "hour" => Some(k.saturating_mul(HOUR)),
                "day" => Some(k.saturating_mul(DAY)),
                "week" => Some(k.saturating_mul(WEEK)),
                _ => None,
            }
        }),
        _ => None,
    };
    period.ok_or_else(|| {
        ApiError::invalid(format!(
            "schedule.text {text:?}: use every N minutes, hourly, every N hours, daily, daily at …, or weekly"
        ))
    })
}

/// 只取了周期、没有实现的部分（时刻、时区），如实写成警告。
pub fn unsupported(s: &super::Schedule, period: Option<u64>) -> Vec<String> {
    let mut out = Vec::new();
    let Some(period) = period else {
        return out;
    };
    let normalized = normalized_cron(period);
    if let Some(c) = &s.cron
        && c.split_whitespace().collect::<Vec<_>>().join(" ") != normalized
    {
        out.push(format!(
            "monitor_time_ignored: cron {c:?} runs every {period} s from the moment the monitor was created; the time of day and weekday are not applied"
        ));
    }
    // 文本写法里周期后面还带时刻（daily at 9am、starting at :07）
    if let Some(t) = s.text.as_deref().filter(|_| s.cron.is_none())
        && t.to_ascii_lowercase().contains(" at ")
    {
        out.push(format!(
            "monitor_time_ignored: {t:?} runs every {period} s from the moment the monitor was created; the time is not applied"
        ));
    }
    if let Some(z) = s
        .timezone
        .as_deref()
        .filter(|z| !z.eq_ignore_ascii_case("UTC"))
    {
        out.push(format!(
            "monitor_timezone_ignored: timezone {z:?} is not applied; schedules are computed in UTC"
        ));
    }
    out
}

/// 归一化的 cron（响应里回给调用方，和 Firecrawl 一样总是给 cron）。
pub fn normalized_cron(period: u64) -> String {
    match period {
        p if p % WEEK == 0 => "0 0 * * 0".into(),
        p if p % DAY == 0 => "0 0 * * *".into(),
        p if p % HOUR == 0 && p / HOUR > 1 => format!("0 */{} * * *", p / HOUR),
        p if p == HOUR => "0 * * * *".into(),
        p => format!("*/{} * * * *", (p / MINUTE).max(1)),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn periods() {
        assert_eq!(cron_period("*/30 * * * *").unwrap(), 1800);
        assert_eq!(cron_period("7 * * * *").unwrap(), 3600);
        assert_eq!(cron_period("0 */2 * * *").unwrap(), 7200);
        assert_eq!(cron_period("30 9 * * *").unwrap(), 86400);
        assert!(cron_period("0 9,17 * * *").is_err());
        assert_eq!(
            text_period("every 15 minutes starting at :07").unwrap(),
            900
        );
        assert_eq!(text_period("daily at 9am").unwrap(), 86400);
        assert_eq!(text_period("every 2 hours").unwrap(), 7200);
        assert!(text_period("sometimes").is_err());
        assert_eq!(normalized_cron(1800), "*/30 * * * *");
        assert_eq!(normalized_cron(7200), "0 */2 * * *");
    }
}
