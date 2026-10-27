//! 推断出来的标题怎么分级：先看字号（大的在上），同一字号里看编号的样式和深度。
//!
//! 编号样式的先后按它们在文档里第一次出现的顺序（写文档的人先用大一级的编号）；
//! 自带深度的十进制编号（`3`、`3.1`）在同一样式族里按深度排。没编号的标题跟同字号里最常见的编号样式同级。

use std::collections::HashMap;

use crate::structure::Style;

pub(crate) struct Cand {
    pub(crate) size: f64,
    pub(crate) style: Option<Style>,
    /// 编号从头数起（`1`、`一`、`(a)`…）
    pub(crate) first: bool,
}

/// 样式族：十进制编号各深度算一族，字母编号一族，其余各自一族。
fn family(s: Style) -> u8 {
    match s {
        Style::Decimal(_) => 0,
        Style::Letter(_) => 1,
        Style::Roman => 2,
        Style::Chapter(u) => 20 + u,
        Style::Paren => 4,
        Style::Circled => 5,
        Style::Kana => 6,
        Style::Lenticular => 7,
        Style::Angle => 8,
        Style::Mark => 9,
        Style::Han => 10,
        Style::HanParen => 11,
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
    // 样式之间的上下级从编号本身读出：下级编号在每个新的上级条目下重新从头数起，
    // 所以某种编号每次从头数起时，紧挨在它前面的另一种编号就是它的上级（任何语言、任何文档都是这样）。
    // 按得票定上下级；没有证据的样式之间仍按第一次出现的先后
    let mut votes: HashMap<(u8, u8), usize> = HashMap::new();
    let mut last: Option<u8> = None;
    for c in cands {
        let Some(s) = c.style else { continue };
        let f = family(s);
        if c.first
            && let Some(p) = last
            && p != f
        {
            *votes.entry((p, f)).or_default() += 1;
        }
        last = Some(f);
    }
    // 章节单位自带上下级（编 > 章 > 节 > 条，Part > Chapter > Section），和 Docling 的族序一致
    let above = |a: u8, b: u8| {
        if a >= 20 && b >= 20 {
            return a < b;
        }
        votes.get(&(a, b)).copied().unwrap_or(0) > votes.get(&(b, a)).copied().unwrap_or(0)
    };
    let mut rest: Vec<(usize, u8)> = first.iter().map(|(&f, &i)| (i, f)).collect();
    rest.sort_unstable();
    let mut order: HashMap<u8, usize> = HashMap::new();
    while !rest.is_empty() {
        // 没有上级还没排的样式里，取最早出现的；有环就直接取最早出现的
        let k = (0..rest.len())
            .find(|&k| !rest.iter().any(|&(_, g)| above(g, rest[k].1)))
            .unwrap_or(0);
        let (_, f) = rest.remove(k);
        order.insert(f, order.len());
    }
    let first = order;
    let rank = |s: Style| -> (usize, u8) { (first[&family(s)], s.depth().unwrap_or(0)) };
    // 字号按 0.5pt 分档：试过 Docling 的 5% 相对容差，把 SEC 文件里 10pt 和 10.5pt 两级标题并成了一级
    let bucket = |size: f64| (size * 2.0).round() as i64;
    // 每个字号里最常见的编号样式：没编号的标题跟它同级
    let mut common: HashMap<i64, HashMap<(usize, u8), usize>> = HashMap::new();
    for c in cands {
        if let Some(s) = c.style {
            *common
                .entry(bucket(c.size))
                .or_default()
                .entry(rank(s))
                .or_default() += 1;
        }
    }
    let common: HashMap<i64, (usize, u8)> = common
        .into_iter()
        .filter_map(|(b, m)| {
            m.into_iter()
                .max_by(|a, b| a.1.cmp(&b.1).then(b.0.cmp(&a.0)))
                .map(|(r, _)| (b, r))
        })
        .collect();
    type Key = (i64, Option<(usize, u8)>);
    let key = |c: &Cand| -> Key {
        let b = bucket(c.size);
        let r = c.style.map(rank).or_else(|| common.get(&b).copied());
        (-b, r)
    };
    let all: Vec<Key> = cands.iter().map(key).collect();
    let mut keys = all.clone();
    keys.sort();
    keys.dedup();
    // 超过 6 级时，先把最上面只出现一次、没编号的几档（封面的题名块）并进第一级，
    // 免得最深的几级编号被挤成同一级
    let single = |k: &Key| k.1.is_none() && all.iter().filter(|x| *x == k).count() == 1;
    let mut merged = 0;
    if keys.first().is_some_and(single) {
        while merged < keys.len().saturating_sub(6) && keys.get(merged + 1).is_some_and(single) {
            merged += 1;
        }
    }
    let dense: Vec<usize> = (0..keys.len()).map(|i| i.saturating_sub(merged)).collect();
    cands
        .iter()
        .map(|c| {
            let k = key(c);
            (dense[keys.iter().position(|x| *x == k).unwrap_or(0)] + 1).min(6)
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn size_then_numbering() {
        let c = |size: f64, style: Option<Style>| Cand {
            size,
            style,
            first: false,
        };
        let cands = [
            c(17.0, None),                    // 文档题名
            c(12.0, Some(Style::Decimal(1))), // 2 PRETRAINING
            c(10.0, Some(Style::Decimal(2))), // 2.1 DATA
            c(10.0, Some(Style::Decimal(3))), // 2.1.1
            c(12.0, None),                    // References（和一级标题同字号）
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
        // 上下级看编号重新计数：（一）先出现（前言里的列表），但每个“一、”“二、”下面都从（一）重新数起，所以在“一、”之下
        let f = |style: Style, first: bool| Cand {
            size: 10.5,
            style: Some(style),
            first,
        };
        let zh = [
            f(Style::HanParen, true),
            f(Style::Han, true),
            f(Style::HanParen, true),
            f(Style::HanParen, false),
            f(Style::Han, false),
            f(Style::HanParen, true),
        ];
        assert_eq!(levels(&zh), vec![2, 1, 2, 2, 1, 2]);
        // 七档：封面上两行只出现一次的大字并进第一级，下面的编号各占一级
        let deep = [
            c(20.0, None),
            c(16.0, None),
            c(14.0, None),
            c(10.5, Some(Style::Lenticular)),
            c(10.5, Some(Style::Angle)),
            c(10.5, Some(Style::Decimal(1))),
            c(10.5, Some(Style::Paren)),
            c(10.5, Some(Style::Circled)),
            c(10.5, Some(Style::Decimal(1))),
        ];
        assert_eq!(levels(&deep), vec![1, 1, 1, 2, 3, 4, 5, 6, 4]);
    }
}
