//! 推断出来的标题怎么分级：先看字号（大的在上），同一字号里看编号的样式和深度。
//!
//! 编号样式的先后按它们在文档里第一次出现的顺序（写文档的人先用大一级的编号）；
//! 自带深度的十进制编号（`3`、`3.1`）在同一样式族里按深度排。没编号的标题跟同字号里最常见的编号样式同级。

use std::collections::HashMap;

use crate::structure::Style;

pub(crate) struct Cand {
    pub(crate) size: f64,
    pub(crate) style: Option<Style>,
}

/// 样式族：十进制编号各深度算一族，字母编号一族，其余各自一族。
fn family(s: Style) -> u8 {
    match s {
        Style::Decimal(_) => 0,
        Style::Letter(_) => 1,
        Style::Roman => 2,
        Style::Chapter => 3,
        Style::Paren => 4,
        Style::Circled => 5,
        Style::Kana => 6,
        Style::Lenticular => 7,
        Style::Angle => 8,
        Style::Mark => 9,
    }
}

/// 每个候选的层级（1–6），与输入同序。
pub(crate) fn levels(cands: &[Cand]) -> Vec<usize> {
    // 样式族第一次出现的位置
    let mut first: HashMap<u8, usize> = HashMap::new();
    for (i, c) in cands.iter().enumerate() {
        if let Some(s) = c.style {
            first.entry(family(s)).or_insert(i);
        }
    }
    let rank = |s: Style| -> (usize, u8) { (first[&family(s)], s.depth().unwrap_or(0)) };
    let bucket = |size: f64| (size * 2.0).round() as i64;
    // 每个字号里最常见的编号样式：没编号的标题跟它同级
    let mut common: HashMap<i64, HashMap<(usize, u8), usize>> = HashMap::new();
    for c in cands {
        if let Some(s) = c.style {
            *common.entry(bucket(c.size)).or_default().entry(rank(s)).or_default() += 1;
        }
    }
    let common: HashMap<i64, (usize, u8)> = common
        .into_iter()
        .filter_map(|(b, m)| m.into_iter().max_by(|a, b| a.1.cmp(&b.1).then(b.0.cmp(&a.0))).map(|(r, _)| (b, r)))
        .collect();
    let key = |c: &Cand| -> (i64, Option<(usize, u8)>) {
        let b = bucket(c.size);
        (-b, c.style.map(rank).or_else(|| common.get(&b).copied()))
    };
    let mut keys: Vec<(i64, Option<(usize, u8)>)> = cands.iter().map(key).collect();
    keys.sort();
    keys.dedup();
    cands
        .iter()
        .map(|c| {
            let k = key(c);
            (keys.iter().position(|x| *x == k).unwrap_or(0) + 1).min(6)
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn size_then_numbering() {
        let c = |size: f64, style: Option<Style>| Cand { size, style };
        let cands = [
            c(17.0, None),                        // 文档题名
            c(12.0, Some(Style::Decimal(1))),     // 2 PRETRAINING
            c(10.0, Some(Style::Decimal(2))),     // 2.1 DATA
            c(10.0, Some(Style::Decimal(3))),     // 2.1.1
            c(12.0, None),                        // References（和一级标题同字号）
        ];
        assert_eq!(levels(&cands), vec![1, 2, 3, 4, 2]);
        // 日文：同一字号，按样式出现的先后
        let ja = [
            c(10.5, Some(Style::Lenticular)),
            c(10.5, Some(Style::Angle)),
            c(10.5, Some(Style::Decimal(1))),
            c(10.5, Some(Style::Paren)),
            c(10.5, Some(Style::Decimal(1))),
        ];
        assert_eq!(levels(&ja), vec![1, 2, 3, 4, 3]);
    }
}
