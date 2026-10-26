//! 旋转的文字：竖写的坐标轴标题、转 90° 的表头、整页横排的宽表格。
//!
//! 内容流解释时，书写方向是 90°、180°、270° 的字各自换算到“把这个方向转正”的坐标系里存放（`PageInk::turned`），
//! 水平的字照旧放在 `glyphs`。斜着的（多是水印）不收。排版前在这里决定：
//! - 某个旋转方向的字比水平的多（整页横排的表格），就把整页转到那个方向：字、框线、图形都换算过去，页面宽高对调；
//! - 其余方向的字是少数，在各自的坐标系里拼成行，再把行的外框换算回页面坐标，和别的行一起排版：
//!   图区会认领坐标轴标题；不参加表格识别（竖写的单词排成行列不是表格）。
//!   整个落在主方向文字左右范围之外的，是页边的边注（arXiv 的竖排编号、期刊侧边的下载信息、行号），
//!   和页眉页脚一样不进正文。
//!
//! 方向 k 的坐标系：把页面顺时针转 k×90°，让这个方向的字变成从左往右写。页面宽 W、高 H。

use crate::geom::{Glyph, PageInk};
use crate::layout::{self, Line};

/// 字的书写方向（文字空间 x 轴在页面上的方向 (a, b)）：0 水平，1 朝上，2 倒写，3 朝下；斜的返回 None。
pub(crate) fn quarter(a: f64, b: f64) -> Option<u8> {
    if a > 0.0 && b.abs() <= 0.1 * a {
        Some(0)
    } else if b > 0.0 && a.abs() <= 0.1 * b {
        Some(1)
    } else if a < 0.0 && b.abs() <= 0.1 * -a {
        Some(2)
    } else if b < 0.0 && a.abs() <= 0.1 * -b {
        Some(3)
    } else {
        None
    }
}

/// 页面坐标 → 方向 k 的坐标系。
pub(crate) fn to_frame(k: u8, x: f64, y: f64, w: f64, h: f64) -> (f64, f64) {
    match k {
        1 => (y, w - x),
        2 => (w - x, h - y),
        3 => (h - y, x),
        _ => (x, y),
    }
}

/// 方向 k 的坐标系 → 页面坐标。
fn from_frame(k: u8, u: f64, v: f64, w: f64, h: f64) -> (f64, f64) {
    match k {
        1 => (w - v, u),
        2 => (w - u, h - v),
        3 => (v, h - u),
        _ => (u, v),
    }
}

/// 方向 k 的坐标系的宽和高。
fn frame_size(k: u8, w: f64, h: f64) -> (f64, f64) {
    if k % 2 == 1 { (h, w) } else { (w, h) }
}

/// 主方向文字的左右范围：按字数去掉最左、最右各 1%，贴着页边的页码这类零星字不把范围撑开。
fn body_span(gs: &[Glyph]) -> Option<(f64, f64)> {
    if gs.is_empty() {
        return None;
    }
    let mut lo: Vec<f64> = gs.iter().map(|g| g.x.min(g.x_end)).collect();
    let mut hi: Vec<f64> = gs.iter().map(|g| g.x.max(g.x_end)).collect();
    lo.sort_by(f64::total_cmp);
    hi.sort_by(f64::total_cmp);
    let cut = gs.len() / 100;
    Some((lo[cut], hi[hi.len() - 1 - cut]))
}

fn chars(gs: &[Glyph]) -> usize {
    gs.iter().map(|g| g.text.chars().count()).sum()
}

/// 定下页面的书写方向，返回少数方向的字拼成的行（已换算到最终的页面坐标）。
pub(crate) fn orient(ink: &mut PageInk) -> Vec<Line> {
    if ink.turned.iter().all(Vec::is_empty) {
        return Vec::new();
    }
    let (w, h) = (ink.width, ink.height);
    // 主方向：字最多的方向；一样多时保持水平
    let mut main = 0u8;
    let mut best = chars(&ink.glyphs);
    for k in 1..=3u8 {
        let n = chars(&ink.turned[usize::from(k) - 1]);
        if n > best {
            main = k;
            best = n;
        }
    }
    // 每组字和它所在的坐标系
    let mut groups: Vec<(u8, Vec<Glyph>)> = vec![(0, std::mem::take(&mut ink.glyphs))];
    for k in 1..=3u8 {
        let g = std::mem::take(&mut ink.turned[usize::from(k) - 1]);
        if !g.is_empty() {
            groups.push((k, g));
        }
    }
    if main != 0 {
        let to = |x: f64, y: f64| to_frame(main, x, y, w, h);
        for r in &mut ink.rules {
            let (a, b) = (to(r.x0, r.y0), to(r.x1, r.y1));
            (r.x0, r.y0, r.x1, r.y1) = (a.0.min(b.0), a.1.min(b.1), a.0.max(b.0), a.1.max(b.1));
        }
        for s in &mut ink.shapes {
            let (a, b) = (to(s.x0, s.y0), to(s.x1, s.y1));
            (s.x0, s.y0, s.x1, s.y1) = (a.0.min(b.0), a.1.min(b.1), a.0.max(b.0), a.1.max(b.1));
        }
        (ink.width, ink.height) = frame_size(main, w, h);
        // 书签目标的位置按原页面坐标给出，转过的页面对不上，不用它
        ink.base = None;
    }
    let mut out = Vec::new();
    // 先放回主方向的字，量出正文的左右范围（主方向坐标系）
    let mut rest = Vec::new();
    for (k, gs) in groups {
        if k == main {
            ink.glyphs = gs;
        } else {
            rest.push((k, gs));
        }
    }
    let body = body_span(&ink.glyphs);
    for (k, gs) in rest {
        for mut l in layout::fragments(&gs, None) {
            // 行在方向 k 坐标系里的外框 → 页面坐标 → 主方向坐标系
            let corners =
                [(l.x0, l.y - 0.25 * l.size), (l.x1, l.y + 0.85 * l.size)].map(|(u, v)| {
                    let (x, y) = from_frame(k, u, v, w, h);
                    to_frame(main, x, y, w, h)
                });
            let (x0, x1) = (
                corners[0].0.min(corners[1].0),
                corners[0].0.max(corners[1].0),
            );
            let (y0, y1) = (
                corners[0].1.min(corners[1].1),
                corners[0].1.max(corners[1].1),
            );
            if body.is_some_and(|(b0, b1)| x1 < b0 - 2.0 || x0 > b1 + 2.0) {
                continue;
            }
            // 竖着的行：基线放在外框的竖向中点，中心点落在它实际占的范围里（图区按中心认领）
            (l.x0, l.x1, l.y) = (x0, x1, (y0 + y1) / 2.0);
            l.turned = true;
            out.push(l);
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::font::MathKind;

    fn glyph(text: &str, x: f64, y: f64) -> Glyph {
        Glyph {
            text: text.into(),
            x,
            y,
            x_end: x + 5.0 * text.chars().count() as f64,
            size: 10.0,
            bold: false,
            math: MathKind::Text,
            mcid: None,
            artifact: false,
        }
    }

    #[test]
    fn frames_round_trip() {
        for k in 0..4u8 {
            let (u, v) = to_frame(k, 120.0, 700.0, 612.0, 792.0);
            let (x, y) = from_frame(k, u, v, 612.0, 792.0);
            assert!(
                (x - 120.0).abs() < 1e-9 && (y - 700.0).abs() < 1e-9,
                "k={k}"
            );
        }
    }

    #[test]
    fn directions() {
        assert_eq!(quarter(1.0, 0.0), Some(0));
        assert_eq!(quarter(0.0, 1.0), Some(1));
        assert_eq!(quarter(-1.0, 0.0), Some(2));
        assert_eq!(quarter(0.0, -1.0), Some(3));
        assert_eq!(quarter(0.7, 0.7), None, "45° 的水印不收");
    }

    /// 少数的竖写字（坐标轴标题）拼成一行，放回它在页面上的位置。
    #[test]
    fn minority_vertical_label_becomes_a_line() {
        let mut ink = PageInk {
            width: 612.0,
            height: 792.0,
            ..PageInk::default()
        };
        for i in 0..20 {
            ink.glyphs.push(glyph(
                "body",
                72.0 + 30.0 * (i % 10) as f64,
                700.0 - 14.0 * (i / 10) as f64,
            ));
        }
        // 正文栏里 x=150 处自下而上写的 “Percent”（图表的纵轴标题）：在方向 1 的坐标系里是一行水平字
        let (u, v) = to_frame(1, 150.0, 400.0, 612.0, 792.0);
        ink.turned[0].push(glyph("Percent", u, v));
        let lines = orient(&mut ink);
        assert_eq!(ink.glyphs.len(), 20, "水平的字仍是主方向");
        assert_eq!(lines.len(), 1);
        let l = &lines[0];
        assert_eq!(l.text, "Percent");
        assert!(
            l.x0 > 135.0 && l.x1 < 155.0,
            "竖着的窄条：{} {}",
            l.x0,
            l.x1
        );
        assert!(l.turned);
        assert!(l.y > 400.0 && l.y < 440.0, "竖向中点：{}", l.y);
    }

    /// 页边的竖排编号（arXiv 首页左边那行）在正文左右范围之外：是边注，不进正文。
    #[test]
    fn margin_stamp_is_dropped() {
        let mut ink = PageInk {
            width: 612.0,
            height: 792.0,
            ..PageInk::default()
        };
        for i in 0..20 {
            ink.glyphs.push(glyph(
                "body",
                72.0 + 40.0 * (i % 10) as f64,
                700.0 - 14.0 * (i / 10) as f64,
            ));
        }
        let (u, v) = to_frame(1, 20.0, 300.0, 612.0, 792.0);
        ink.turned[0].push(glyph("arXiv:2309.16609v1", u, v));
        assert!(orient(&mut ink).is_empty());
    }

    /// 页码贴在页边，不把正文范围撑开：页码外侧的竖排期刊名仍是边注。
    #[test]
    fn stray_page_number_does_not_widen_the_body() {
        let mut ink = PageInk {
            width: 612.0,
            height: 792.0,
            ..PageInk::default()
        };
        for i in 0..300 {
            ink.glyphs.push(glyph(
                "w",
                72.0 + 1.5 * (i % 300) as f64,
                700.0 - 14.0 * (i / 30) as f64,
            ));
        }
        ink.glyphs.push(glyph("39", 590.0, 700.0));
        let (u, v) = to_frame(1, 565.0, 300.0, 612.0, 792.0);
        ink.turned[0].push(glyph("Journal vol 37", u, v));
        assert!(orient(&mut ink).is_empty());
    }

    /// 整页横排的表格：转过的字占多数，整页转过去，宽高对调。
    #[test]
    fn mostly_turned_page_is_rotated() {
        let mut ink = PageInk {
            width: 612.0,
            height: 792.0,
            ..PageInk::default()
        };
        // 页面底部、横排表格范围之内的一行水平字（转过来后在表格左右范围之内）
        ink.glyphs.push(glyph("Table 7", 300.0, 150.0));
        for i in 0..30 {
            ink.turned[0].push(glyph(
                "cell",
                80.0 + 40.0 * (i % 5) as f64,
                500.0 - 14.0 * (i / 5) as f64,
            ));
        }
        let lines = orient(&mut ink);
        assert_eq!(ink.glyphs.len(), 30);
        assert_eq!((ink.width, ink.height), (792.0, 612.0));
        assert!(ink.base.is_none());
        assert_eq!(lines.len(), 1, "原来水平的那行成了少数方向的一行");
    }
}
