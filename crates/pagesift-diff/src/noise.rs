//! 噪声分类：把数字、价格、日期、相对时间、计数换成占位符后比较，
//! 占位符之外的文字完全一致才算噪声。

use std::sync::OnceLock;

use regex::Regex;
use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum NoiseCategory {
    Date,
    RelativeTime,
    Price,
    Count,
    Number,
}

const MONTHS: &str = "jan|feb|mar|apr|may|jun|jul|aug|sep|oct|nov|dec";
const UNITS: &str = "seconds?|secs?|minutes?|mins?|hours?|hrs?|days?|weeks?|months?|years?|yrs?";
const COUNT_CN: &str = "次阅读|次浏览|次播放|次下载|条评论|条回复|个赞|人关注|人在看|阅读|浏览|评论|点赞|收藏|播放|转发|分享|下载|回复|关注|赞";
const COUNT_EN: &str = "views?|comments?|likes?|reads?|shares?|replies|reply|retweets?|stars?|downloads?|followers?|subscribers?|votes?|points?|visits?|viewers?|reviews?|ratings?";

struct Patterns {
    any: Regex,
    kinds: [(NoiseCategory, Regex); 4],
}

fn patterns() -> &'static Patterns {
    static RE: OnceLock<Patterns> = OnceLock::new();
    RE.get_or_init(|| {
        let date = format!(
            r"(?i)(?:[0-9]{{4}}[-/.][0-9]{{1,2}}[-/.][0-9]{{1,2}}(?:[T ][0-9]{{1,2}}:[0-9]{{2}}(?::[0-9]{{2}}(?:\.[0-9]+)?)?(?:Z|[+-][0-9]{{2}}:?[0-9]{{2}})?)?|[0-9]{{4}}年[0-9]{{1,2}}月(?:[0-9]{{1,2}}[日号])?|[0-9]{{1,2}}月[0-9]{{1,2}}[日号]|[0-9]{{1,2}}:[0-9]{{2}}(?::[0-9]{{2}})?|\b(?:{m})[a-z]*\.? [0-9]{{1,2}}(?:st|nd|rd|th)?(?:, [0-9]{{4}})?\b|\b[0-9]{{1,2}}(?:st|nd|rd|th)? (?:{m})[a-z]*\.?(?:,? [0-9]{{4}})?\b)",
            m = MONTHS
        );
        let rel = format!(
            r"(?:(?i:\b(?:[0-9]+|an?|one)\s?(?:{u})\s+ago\b|\bin\s+(?:[0-9]+|an?)\s?(?:{u})\b|\bjust now\b|\byesterday\b)|[0-9]+\s?(?:个)?(?:秒钟?|分钟|小时|天|周|星期|个月|年)[前后]|刚刚|昨天|前天)",
            u = UNITS
        );
        let price = r"(?:(?:US\$|[$€£¥￥₹₩])\s?[0-9][0-9,]*(?:\.[0-9]+)?(?:[kKmM万亿]\b)?|(?:USD|EUR|GBP|CNY|RMB|JPY)\s?[0-9][0-9,]*(?:\.[0-9]+)?|[0-9][0-9,]*(?:\.[0-9]+)?\s?(?:USD|EUR|GBP|CNY|RMB|JPY|元|块钱|美元|欧元|日元|港元))";
        let count = format!(
            r"(?:[0-9][0-9,]*(?:\.[0-9]+)?[kKmM万]?\s?(?:{cn}|(?i:(?:{en})\b))|(?:{cn}|(?i:(?:{en})\b))[:：]?\s?[0-9][0-9,]*(?:\.[0-9]+)?)",
            cn = COUNT_CN,
            en = COUNT_EN
        );
        let num = r"(?:[0-9][0-9,]*(?:\.[0-9]+)?%?)";
        let full = |p: &str| Regex::new(&format!("^(?:{p})$")).expect("noise regex");
        Patterns {
            any: Regex::new(&format!("{date}|{rel}|{price}|{count}|{num}")).expect("noise regex"),
            kinds: [
                (NoiseCategory::Date, full(&date)),
                (NoiseCategory::RelativeTime, full(&rel)),
                (NoiseCategory::Price, full(price)),
                (NoiseCategory::Count, full(&count)),
            ],
        }
    })
}

struct Entity<'a> {
    kind: NoiseCategory,
    text: &'a str,
}

fn mask(s: &str) -> (String, Vec<Entity<'_>>) {
    let mut out = String::with_capacity(s.len());
    let mut ents = Vec::new();
    let mut last = 0;
    let pats = patterns();
    for m in pats.any.find_iter(s) {
        // 联合正则按优先级取最左匹配，按同样的顺序用锚定正则回查类别
        let kind = match pats.kinds.iter().find(|(_, re)| re.is_match(m.as_str())) {
            Some(&(k, _)) => k,
            None => {
                // 紧跟在字母后的数字（哈希、标识符）不当作数字
                if s[..m.start()]
                    .chars()
                    .next_back()
                    .is_some_and(|c| c.is_ascii_alphabetic() || c == '_')
                {
                    continue;
                }
                NoiseCategory::Number
            }
        };
        out.push_str(&s[last..m.start()]);
        out.push('\u{E000}');
        out.push(match kind {
            NoiseCategory::Date => 'd',
            NoiseCategory::RelativeTime => 'r',
            NoiseCategory::Price => 'p',
            NoiseCategory::Count => 'c',
            NoiseCategory::Number => 'n',
        });
        last = m.end();
        ents.push(Entity {
            kind,
            text: m.as_str(),
        });
    }
    out.push_str(&s[last..]);
    (out, ents)
}

/// 返回 `Some(变化的类别)` 表示只有噪声实体变了；`None` 表示有实质变化。
pub(crate) fn classify(
    old: &str,
    new: &str,
    enabled: &[NoiseCategory],
) -> Option<Vec<NoiseCategory>> {
    let (mo, eo) = mask(old);
    let (mn, en) = mask(new);
    if mo != mn {
        return None;
    }
    let mut cats: Vec<NoiseCategory> = eo
        .iter()
        .zip(&en)
        .filter(|(a, b)| a.text != b.text)
        .map(|(a, _)| a.kind)
        .collect();
    cats.sort();
    cats.dedup();
    if cats.is_empty() || cats.iter().any(|c| !enabled.contains(c)) {
        return None;
    }
    Some(cats)
}

#[cfg(test)]
mod tests {
    use super::*;
    use NoiseCategory::*;

    const ALL: [NoiseCategory; 5] = [Date, RelativeTime, Price, Count, Number];

    fn c(a: &str, b: &str) -> Option<Vec<NoiseCategory>> {
        classify(a, b, &ALL)
    }

    #[test]
    fn categories() {
        assert_eq!(
            c("Now $1,299.00 only", "Now $1,199.00 only"),
            Some(vec![Price])
        );
        assert_eq!(
            c("发布于 2024年1月5日", "发布于 2024年2月9日"),
            Some(vec![Date])
        );
        assert_eq!(
            c("Updated 3 hours ago", "Updated 5 hours ago"),
            Some(vec![RelativeTime])
        );
        assert_eq!(c("1,234 次阅读", "1,301 次阅读"), Some(vec![Count]));
        assert_eq!(c("12 条评论", "15 条评论"), Some(vec![Count]));
        assert_eq!(
            c("2024-01-05T10:00:00Z", "2024-01-06T10:00:00Z"),
            Some(vec![Date])
        );
        assert_eq!(
            c("version has 3 items", "version has 4 items"),
            Some(vec![Number])
        );
    }

    #[test]
    fn substantive() {
        assert_eq!(c("Price $5", "Cost $5"), None);
        assert_eq!(c("Now $5", "Now 5 views"), None);
        assert_eq!(c("a", "a b"), None);
        assert_eq!(classify("Now $5", "Now $6", &[Date]), None);
    }
}
