//! 并排的两栏文字不是表格：无框线表格的候选跨过栏间空白时，看两边像不像各走各的两栏。
//!
//! 真表格同一行的格子基线对齐、格子短；并排的两栏列表、正文和旁边的信息框各按自己的行距排（基线错开），
//! 或者两边都是写满栏宽的长行（两栏的参考文献）。

use crate::layout::Line;

fn median(mut v: Vec<f64>) -> Option<f64> {
    if v.is_empty() {
        return None;
    }
    v.sort_by(f64::total_cmp);
    Some(v[v.len() / 2])
}

/// `mine` 这些片段跨过某条栏间空白，而且两边像并排的两栏文字：返回那条空白的中线。
pub(crate) fn parallel_columns(
    frags: &[Line],
    mine: &[usize],
    gutters: &[(f64, f64)],
) -> Option<f64> {
    for (i, &(g0, g1)) in gutters.iter().enumerate() {
        let lo = if i == 0 {
            f64::NEG_INFINITY
        } else {
            gutters[i - 1].1 - 0.5
        };
        let hi = gutters.get(i + 1).map_or(f64::INFINITY, |g| g.0 + 0.5);
        let left: Vec<&Line> = mine
            .iter()
            .map(|&k| &frags[k])
            .filter(|f| f.x1 <= g0 + 0.5 && f.x0 >= lo)
            .collect();
        let right: Vec<&Line> = mine
            .iter()
            .map(|&k| &frags[k])
            .filter(|f| f.x0 >= g1 - 0.5 && f.x1 <= hi)
            .collect();
        if left.len() < 2 || right.len() < 2 {
            continue;
        }
        let (Some(ls), Some(rs)) = (
            median(left.iter().map(|f| f.size).collect()),
            median(right.iter().map(|f| f.size).collect()),
        ) else {
            continue;
        };
        // （两边字号不同不算证据：扫描件的字号有噪声，表头和表身字号也常不同）
        // 基线错开：配得上对的行里，过半错开 0.12 字号以上
        let mut pairs = 0;
        let mut off = 0;
        for l in &left {
            let d = right
                .iter()
                .map(|r| (r.y - l.y).abs())
                .fold(f64::INFINITY, f64::min);
            if d < 0.5 * l.size {
                pairs += 1;
                if d > 0.12 * l.size {
                    off += 1;
                }
            }
        }
        if pairs >= 3 && off * 2 >= pairs {
            return Some((g0 + g1) / 2.0);
        }
        // 两边都是写满栏宽的长行（栏宽至少 15 个字号：正文栏，不是表格里的一列）
        let full = |side: &[&Line], size: f64| {
            let x0 = side.iter().map(|f| f.x0).fold(f64::INFINITY, f64::min);
            let x1 = side.iter().map(|f| f.x1).fold(f64::NEG_INFINITY, f64::max);
            side.len() >= 3
                && x1 - x0 >= 15.0 * size
                && median(side.iter().map(|f| f.x1 - f.x0).collect())
                    .is_some_and(|m| m >= 0.6 * (x1 - x0))
        };
        if full(&left, ls) && full(&right, rs) {
            return Some((g0 + g1) / 2.0);
        }
    }
    None
}

#[cfg(test)]
mod tests {
    use super::*;

    fn frag(text: &str, x0: f64, x1: f64, y: f64) -> Line {
        Line {
            text: text.into(),
            x0,
            x1,
            y,
            size: 12.0,
            bold: false,
            raw: false,
            mcid: None,
            artifact: false,
            hint: None,
            group: None,
            keep: false,
            turned: false,
        }
    }

    #[test]
    fn side_by_side_lists_are_two_columns() {
        // 两栏列表：右栏的基线比左栏低 3pt（各按自己的行距排）
        let mut frags = Vec::new();
        for k in 0..6 {
            let y = 500.0 - 21.0 * k as f64;
            frags.push(frag("中国人民银行征信中心", 63.0, 183.0, y));
            frags.push(frag("中国人民银行金融信息中心", 323.0, 467.0, y - 3.0));
        }
        let mine: Vec<usize> = (0..frags.len()).collect();
        assert!(parallel_columns(&frags, &mine, &[(290.0, 320.0)]).is_some());
    }

    #[test]
    fn aligned_table_is_not_two_columns() {
        let mut frags = Vec::new();
        for k in 0..6 {
            let y = 500.0 - 21.0 * k as f64;
            frags.push(frag("営業収益", 106.0, 148.0, y));
            frags.push(frag("48兆5,000億円", 275.0, 343.0, y));
        }
        let mine: Vec<usize> = (0..frags.len()).collect();
        assert!(parallel_columns(&frags, &mine, &[(200.0, 270.0)]).is_none());
    }
}
