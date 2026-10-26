//! 写入方声明的文档结构：结构树里的标题、书签。有就照着用，没有才由版面推断（见 `layout::render`）。

mod numbering;
mod outline;
mod tags;
mod text;

pub(crate) use numbering::{Style, numbering};
pub(crate) use outline::Entry;
pub(crate) use tags::HeadingMcids;

use std::collections::HashMap;

use crate::file::{Document, ObjectId};
use crate::geom::Matrix;
use crate::layout::{Line, Para};
pub(crate) use text::strip_numbering;
use text::{Fit, Lazy, Titled, fit, split_after};

#[derive(Default)]
pub(crate) struct Structure {
    /// 结构树里的标题；没有结构树或树里没有标题时为 None。
    pub(crate) tags: Option<HeadingMcids>,
    pub(crate) outline: Vec<Entry>,
}

pub(crate) fn read(doc: &Document, pages: &[ObjectId]) -> Structure {
    Structure {
        tags: tags::headings(doc, pages),
        outline: outline::entries(doc, pages),
    }
}

/// 结构树：行的 MCID 落在标题元素里，就给这一行标上层级。返回标上的行数。
pub(crate) fn tag_lines(lines: &mut [Line], headings: &HashMap<i32, (u8, u32)>) -> usize {
    let mut n = 0;
    for line in lines.iter_mut() {
        let h = line.mcid.and_then(|m| headings.get(&m).copied());
        line.hint = h.map(|(level, _)| level);
        line.group = h.map(|(_, element)| element);
        n += usize::from(h.is_some());
    }
    n
}

/// 去页眉页脚之前：这一页（或相邻页）书签标题对得上的行标成 keep，免得页首的小节标题被当页眉删掉。
/// 段落文字只需归一化到最长的书签标题再多一个字符（见 `Titled::with_limit`）。
fn title_limit(entries: &[Entry]) -> usize {
    entries.iter().map(|e| Titled::new(&e.title).full.chars().count()).max().unwrap_or(0) + 1
}

pub(crate) fn keep_outline_lines(pages: &mut [Vec<Line>], entries: &[Entry]) {
    // 每页的行只归一化一次（书签条目多时，逐条重算是 条目数 × 行数 次），而且只归一化到最长的标题再多一个字
    let mut cache: HashMap<usize, Vec<Option<Titled>>> = HashMap::new();
    let limit = title_limit(entries);
    for e in entries {
        let Some(p) = e.page else { continue };
        let title = Titled::new(&e.title);
        if title.full.chars().count() < 2 {
            continue;
        }
        for q in [Some(p), p.checked_add(1)].into_iter().flatten() {
            let Some(lines) = pages.get_mut(q) else { continue };
            let titled = cache
                .entry(q)
                .or_insert_with(|| lines.iter().map(|l| (!l.raw).then(|| Titled::with_limit(&l.text, limit))).collect());
            for (line, t) in lines.iter_mut().zip(titled.iter()) {
                if let Some(t) = t
                    && matches!(fit(&title, t), Some(Fit::Exact | Fit::Prefix(_)))
                {
                    line.keep = true;
                }
            }
        }
    }
}

/// 书签：在目标页（及相邻页）上找文字对得上的段落，标上层级；段落以标题开头、后面粘着正文时拆成两段。
/// `bases[i]` 是第 i 页用户空间到页面坐标的变换（用来换算目标位置）。返回对上的条目数。
pub(crate) fn mark_outline(pages: &mut [Vec<Para>], entries: &[Entry], bases: &[Option<Matrix>]) -> usize {
    let mut matched = 0;
    // 每页段落的归一化文字只算一次；拆段改了某页时只作废那一页
    let mut cache: HashMap<usize, Vec<Option<Lazy>>> = HashMap::new();
    let limit = title_limit(entries);
    for e in entries {
        let Some(p) = e.page.filter(|&p| p < pages.len()) else {
            continue;
        };
        let title = Titled::new(&e.title);
        if title.full.chars().count() < 2 {
            continue;
        }
        let top = e.top.zip(bases[p]).map(|(t, m)| m.apply(0.0, t).1);
        // 目标页优先；目标在页底时标题常在下一页，偶尔目标指到了前一页
        let mut best: Option<(usize, usize, Fit, f64)> = None;
        let near = [Some(p), p.checked_add(1), p.checked_sub(1)];
        for (rank, q) in near.into_iter().flatten().enumerate() {
            let Some(paras) = pages.get(q) else { continue };
            let titled = cache
                .entry(q)
                .or_insert_with(|| paras.iter().map(|para| (!para.raw).then(|| Lazy::new(&para.text))).collect());
            for (i, (para, t)) in paras.iter().zip(titled.iter()).enumerate() {
                let Some(t) = t.as_ref().filter(|_| para.hint.is_none()) else {
                    continue;
                };
                let Some(fit) = t.fit(&title, &para.text, limit) else {
                    continue;
                };
                // 目标位置一般在标题上方一点：在目标下方、离得越近越好；别的页排在后面
                let dist = match (rank, top) {
                    (0, Some(t)) => {
                        let d = t - para.top;
                        if d >= -para.size { d.abs() } else { 1e5 + d.abs() }
                    }
                    (0, None) => i as f64,
                    _ => 1e6 * rank as f64 + i as f64,
                };
                if best.as_ref().is_none_or(|b| (dist, fit) < (b.3, b.2)) {
                    best = Some((q, i, fit, dist));
                }
            }
        }
        let Some((q, i, fit, _)) = best else {
            continue;
        };
        let paras = &mut pages[q];
        if let Fit::Prefix(n) = fit {
            split_after(paras, i, n);
            cache.remove(&q);
        }
        paras[i].hint = Some(e.level);
        matched += 1;
    }
    matched
}
