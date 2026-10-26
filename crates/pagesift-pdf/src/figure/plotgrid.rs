//! 图表的网格线：横竖各至少 MIN_LINES 条长线交织成网，格子大多是空的。散点图、折线图的背景网格就是这样，
//! 点彼此离得远，单靠点聚不成图。表格的框线也交织成网，但几乎每个格子都有字，所以拿“有字的格子占比”区分：
//! 网里的文字行不到格子数的三分之一才算图表网格。空白表单的格子也大多空着，所以调用方还要求网里画着别的东西。

use super::evidence::Centers;
use crate::geom::Rule;

/// 每个方向至少几条线（四条线围出三格）。
const MIN_LINES: usize = 4;
/// 网格线最短多少（pt）。
const MIN_LEN: f64 = 40.0;
/// 端点对齐的容差（pt）。
const TOL: f64 = 1.0;
/// 横线组和竖线组两两配对的上限（线很多的页面不做，免得退化成平方）。
const MAX_PAIRS: usize = 10_000;

/// 一组端点对齐的平行线：沿线方向的范围、垂直方向的位置（排好）。
struct Bundle {
    lo: f64,
    hi: f64,
    at: Vec<f64>,
}

/// 端点对齐（两端各差 TOL 以内）的平行长线分成组，只留至少 MIN_LINES 条的组。
fn bundles(rules: &[Rule], horizontal: bool) -> Vec<Bundle> {
    let mut v: Vec<(f64, f64, f64)> = rules
        .iter()
        .filter(|r| r.horizontal() == horizontal)
        .map(|r| {
            if horizontal {
                (r.x0.min(r.x1), r.x0.max(r.x1), r.y0)
            } else {
                (r.y0.min(r.y1), r.y0.max(r.y1), r.x0)
            }
        })
        .filter(|(lo, hi, _)| hi - lo >= MIN_LEN)
        .collect();
    v.sort_by(|a, b| a.0.total_cmp(&b.0).then(a.1.total_cmp(&b.1)));
    let mut out = Vec::new();
    let mut i = 0;
    while i < v.len() {
        // 起点和组首相差 TOL 以内的一段里，再按终点分组
        let mut j = i + 1;
        while j < v.len() && v[j].0 - v[i].0 <= TOL {
            j += 1;
        }
        let mut run: Vec<(f64, f64, f64)> = v[i..j].to_vec();
        run.sort_by(|a, b| a.1.total_cmp(&b.1));
        for g in run.chunk_by(|a, b| b.1 - a.1 <= TOL) {
            let mut at: Vec<f64> = g.iter().map(|t| t.2).collect();
            at.sort_by(f64::total_cmp);
            at.dedup_by(|a, b| (*a - *b).abs() <= TOL);
            if at.len() >= MIN_LINES {
                out.push(Bundle {
                    lo: g[0].0,
                    hi: g[g.len() - 1].1,
                    at,
                });
            }
        }
        i = j;
    }
    out
}

/// 图表网格的外框 `[x0, y0, x1, y1]`：竖线组正好铺满横线组的横向范围、竖线的上下端落在最外两条横线上（反之亦然），
/// 网里的文字行不到格子数的三分之一。
pub(super) fn plot_grids(rules: &[Rule], centers: &Centers) -> Vec<[f64; 4]> {
    let hs = bundles(rules, true);
    if hs.is_empty() {
        return Vec::new();
    }
    let vs = bundles(rules, false);
    if vs.is_empty() || hs.len().saturating_mul(vs.len()) > MAX_PAIRS {
        return Vec::new();
    }
    let mut out = Vec::new();
    // 并排（或上下叠放）的几张图的线端点也对齐，会分进同一组：配对时只取落在对方范围里的线
    let within = |at: &[f64], lo: f64, hi: f64| -> Vec<f64> {
        at.iter()
            .copied()
            .filter(|&p| p >= lo - TOL && p <= hi + TOL)
            .collect()
    };
    let spans = |a: f64, b: f64| (a - b).abs() <= TOL;
    for h in &hs {
        for v in &vs {
            let ys = within(&h.at, v.lo, v.hi);
            let xs = within(&v.at, h.lo, h.hi);
            if ys.len() < MIN_LINES || xs.len() < MIN_LINES {
                continue;
            }
            let (y0, y1, x0, x1) = (ys[0], ys[ys.len() - 1], xs[0], xs[xs.len() - 1]);
            if !(spans(v.lo, y0) && spans(v.hi, y1) && spans(h.lo, x0) && spans(h.hi, x1)) {
                continue;
            }
            let b = [x0, y0, x1, y1];
            let cells = (ys.len() - 1) * (xs.len() - 1);
            if centers.count(&b) * 3 < cells {
                out.push(b);
            }
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::layout::Line;

    fn rule(x0: f64, y0: f64, x1: f64, y1: f64) -> Rule {
        Rule { x0, y0, x1, y1 }
    }

    fn line(text: &str, x: f64, y: f64) -> Line {
        Line {
            text: text.into(),
            x0: x,
            x1: x + 20.0,
            y,
            size: 8.0,
            bold: false,
            raw: false,
            mcid: None,
            artifact: false,
            hint: None,
            group: None,
            keep: false,
        }
    }

    /// 5×5 条线、16 个格子的网。
    fn net() -> Vec<Rule> {
        let mut r = Vec::new();
        for i in 0..5 {
            let p = 100.0 + 25.0 * i as f64;
            r.push(rule(100.0, p, 200.0, p));
            r.push(rule(p, 100.0, p, 200.0));
        }
        r
    }

    #[test]
    fn empty_net_is_a_plot_grid() {
        let labels = [line("Model A", 130.0, 160.0), line("Model B", 160.0, 120.0)];
        assert_eq!(
            plot_grids(&net(), &Centers::new(&labels)),
            [[100.0, 100.0, 200.0, 200.0]]
        );
    }

    #[test]
    fn side_by_side_nets_are_two_grids() {
        let mut r = net();
        r.extend(
            net()
                .iter()
                .map(|q| rule(q.x0 + 150.0, q.y0, q.x1 + 150.0, q.y1)),
        );
        let got = plot_grids(&r, &Centers::new(&[]));
        assert_eq!(
            got,
            [[100.0, 100.0, 200.0, 200.0], [250.0, 100.0, 350.0, 200.0]]
        );
    }

    #[test]
    fn net_with_text_in_every_cell_is_a_table() {
        let mut cells = Vec::new();
        for i in 0..4 {
            for j in 0..4 {
                cells.push(line("42", 103.0 + 25.0 * i as f64, 108.0 + 25.0 * j as f64));
            }
        }
        assert!(plot_grids(&net(), &Centers::new(&cells)).is_empty());
    }
}
