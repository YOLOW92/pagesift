//! 判断一行字像不像图上的标签、题注、正文。

use crate::layout::is_cjk;

fn cjk_share(t: &str) -> (usize, usize) {
    let n = t.chars().filter(|c| !c.is_whitespace()).count();
    (t.chars().filter(|&c| is_cjk(c)).count(), n)
}

/// 题注、资料来源、注释：留在正文里，也挡住图向外吸附。
pub(super) fn caption_like(t: &str) -> bool {
    let t = t.trim();
    let lower = t.to_lowercase();
    if lower.starts_with("source") || lower.starts_with("note") {
        return true;
    }
    for w in ["figure", "fig.", "table", "chart", "exhibit", "box"] {
        if let Some(rest) = t.get(w.len()..)
            && lower.starts_with(w)
            && rest
                .trim_start()
                .chars()
                .next()
                .is_some_and(|c| c.is_ascii_digit() || c.is_uppercase())
        {
            return true;
        }
    }
    [
        "図表",
        "図",
        "表",
        "（出所）",
        "（注）",
        "(出所)",
        "(注)",
        "资料来源",
        "注：",
        "出所：",
    ]
    .iter()
    .any(|w| {
        t.starts_with(w)
            && t.chars()
                .nth(w.chars().count())
                .is_none_or(|c| !is_cjk(c) || "（：:".contains(c))
    })
}

/// 数字样的标签：刻度值、年份、百分比（`2.5`、`−10`、`24/1Q`、`09年`、`50%`），或一排这样的值（横轴的年份）。
fn numeric(t: &str) -> bool {
    let token = |w: &str| {
        w.chars()
            .any(|c| c.is_ascii_digit() || ('０'..='９').contains(&c))
            && w.chars().filter(|c| c.is_alphabetic()).count() <= 3
            && w.chars().count() <= 16
    };
    t.chars().count() <= 80
        && t.split_whitespace().all(token)
        && t.split_whitespace().next().is_some()
}

/// 很短的标签：可以从图的左右两侧吸进来。
pub(super) fn short_label(t: &str) -> bool {
    let t = t.trim();
    numeric(t) || t.chars().count() <= 6
}

/// 像图上的标签（不是句子）：上下方向吸附用。
pub(super) fn label_like(t: &str) -> bool {
    let t = t.trim();
    if t.is_empty() {
        return false;
    }
    if numeric(t) {
        return true;
    }
    let (cjk, n) = cjk_share(t);
    if cjk * 2 > n {
        if t.contains('。') {
            return false;
        }
        let bracketed = t.starts_with(['（', '(', '<', '＜']);
        return n <= 18 || (bracketed && n <= 30);
    }
    let words = t.split_whitespace().count();
    let sentence = words >= 5 && t.ends_with(['.', ';', ':']);
    !sentence && words <= 6 && t.chars().count() <= 48
}

/// 像正文的一行：长句子。
pub(super) fn prose_like(t: &str) -> bool {
    let t = t.trim();
    let (cjk, n) = cjk_share(t);
    if cjk * 2 > n {
        return n >= 18 && t.contains(['、', '。', '，']);
    }
    t.split_whitespace().count() >= 8 && t.chars().count() >= 40
}
