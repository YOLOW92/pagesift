//! 有框线表格检测（基于 pdfplumber lattice 算法的纯 Rust 实现）。
//! 线段吸附、连接并求交点，从交点寻找闭合矩形作为单元格，共享角点的单元格聚类为独立表格。

use crate::interp::Rule;

/// 吸附、连接、求交点的容差（页面单位，对标 pdfplumber 默认值）。
pub(crate) const SNAP: f64 = 3.0;
pub(crate) const JOIN: f64 = 3.0;
pub(crate) const TOL: f64 = 1.0;
/// 交点上限保护，防止复杂矢量图引发平方级开销。
pub(crate) const MAX_POINTS: usize = 3000;

#[derive(Debug, Clone, Copy)]
pub(crate) struct HEdge {
    pub(crate) y: f64,
    pub(crate) x0: f64,
    pub(crate) x1: f64,
}

#[derive(Debug, Clone, Copy)]
pub(crate) struct VEdge {
    pub(crate) x: f64,
    pub(crate) y0: f64,
    pub(crate) y1: f64,
}

/// 坐标相近（差不超过 `tol`）的聚成一组，每组取平均值。
pub(crate) fn snap(values: &[f64], tol: f64) -> Vec<f64> {
    let mut idx: Vec<usize> = (0..values.len()).collect();
    idx.sort_by(|&a, &b| values[a].total_cmp(&values[b]));
    let mut out = vec![0.0; values.len()];
    let mut group: Vec<usize> = Vec::new();
    let flush = |group: &mut Vec<usize>, out: &mut Vec<f64>| {
        if group.is_empty() {
            return;
        }
        let mean = group.iter().map(|&i| values[i]).sum::<f64>() / group.len() as f64;
        for &i in group.iter() {
            out[i] = mean;
        }
        group.clear();
    };
    for i in idx {
        if let Some(&last) = group.last()
            && values[i] - values[last] > tol
        {
            flush(&mut group, &mut out);
        }
        group.push(i);
    }
    flush(&mut group, &mut out);
    out
}

/// 吸附后，同一条直线上首尾相接（间隔不超过 JOIN）的线段连成一条。
pub(crate) fn merge(rules: &[Rule]) -> (Vec<HEdge>, Vec<VEdge>) {
    let hs: Vec<&Rule> = rules.iter().filter(|r| r.horizontal()).collect();
    let vs: Vec<&Rule> = rules.iter().filter(|r| !r.horizontal()).collect();
    let hy = snap(&hs.iter().map(|r| r.y0).collect::<Vec<_>>(), SNAP);
    let vx = snap(&vs.iter().map(|r| r.x0).collect::<Vec<_>>(), SNAP);
    let mut h: Vec<HEdge> = hs
        .iter()
        .zip(&hy)
        .map(|(r, &y)| HEdge {
            y,
            x0: r.x0,
            x1: r.x1,
        })
        .collect();
    let mut v: Vec<VEdge> = vs
        .iter()
        .zip(&vx)
        .map(|(r, &x)| VEdge {
            x,
            y0: r.y0,
            y1: r.y1,
        })
        .collect();
    h.sort_by(|a, b| a.y.total_cmp(&b.y).then(a.x0.total_cmp(&b.x0)));
    v.sort_by(|a, b| a.x.total_cmp(&b.x).then(a.y0.total_cmp(&b.y0)));
    let mut hj: Vec<HEdge> = Vec::new();
    for e in h {
        match hj.last_mut() {
            Some(l) if l.y == e.y && e.x0 <= l.x1 + JOIN => l.x1 = l.x1.max(e.x1),
            _ => hj.push(e),
        }
    }
    let mut vj: Vec<VEdge> = Vec::new();
    for e in v {
        match vj.last_mut() {
            Some(l) if l.x == e.x && e.y0 <= l.y1 + JOIN => l.y1 = l.y1.max(e.y1),
            _ => vj.push(e),
        }
    }
    (hj, vj)
}

/// 有框线的表格（pdfplumber 的 lattice 算法）。返回每张表的单元格（x0, top, x1, bottom）。
pub(crate) fn lattice(h: &[HEdge], v: &[VEdge]) -> Vec<Vec<(f64, f64, f64, f64)>> {
    let mut points: Vec<(f64, f64, Vec<usize>, Vec<usize>)> = Vec::new();
    for (vi, ve) in v.iter().enumerate() {
        for (hi, he) in h.iter().enumerate() {
            if ve.y0 - TOL <= he.y
                && he.y <= ve.y1 + TOL
                && he.x0 - TOL <= ve.x
                && ve.x <= he.x1 + TOL
            {
                match points.iter_mut().find(|p| p.0 == ve.x && p.1 == he.y) {
                    Some(p) => {
                        p.2.push(vi);
                        p.3.push(hi);
                    }
                    None => points.push((ve.x, he.y, vec![vi], vec![hi])),
                }
                if points.len() > MAX_POINTS {
                    return Vec::new();
                }
            }
        }
    }
    points.sort_by(|a, b| a.0.total_cmp(&b.0).then(b.1.total_cmp(&a.1)));
    let at = |x: f64, y: f64| points.iter().position(|p| p.0 == x && p.1 == y);
    let connects = |a: usize, b: usize| {
        let (p, q) = (&points[a], &points[b]);
        (p.0 == q.0 && p.2.iter().any(|e| q.2.contains(e)))
            || (p.1 == q.1 && p.3.iter().any(|e| q.3.contains(e)))
    };
    let mut cells: Vec<(f64, f64, f64, f64)> = Vec::new();
    for i in 0..points.len() {
        let (x, y) = (points[i].0, points[i].1);
        let below: Vec<usize> = (i + 1..points.len())
            .filter(|&j| points[j].0 == x && points[j].1 < y)
            .collect();
        let right: Vec<usize> = (i + 1..points.len())
            .filter(|&j| points[j].1 == y && points[j].0 > x)
            .collect();
        'found: for &b in &below {
            if !connects(i, b) {
                continue;
            }
            for &r in &right {
                if !connects(i, r) {
                    continue;
                }
                if let Some(br) = at(points[r].0, points[b].1)
                    && connects(br, r)
                    && connects(br, b)
                {
                    cells.push((x, y, points[r].0, points[b].1));
                    break 'found;
                }
            }
        }
    }
    let corners = |c: &(f64, f64, f64, f64)| [(c.0, c.1), (c.0, c.3), (c.2, c.1), (c.2, c.3)];
    let mut tables: Vec<Vec<(f64, f64, f64, f64)>> = Vec::new();
    let mut left: Vec<(f64, f64, f64, f64)> = cells;
    while let Some(first) = left.pop() {
        let mut table = vec![first];
        let mut pts: Vec<(f64, f64)> = corners(&first).to_vec();
        loop {
            let before = table.len();
            left.retain(|c| {
                if corners(c).iter().any(|p| pts.contains(p)) {
                    table.push(*c);
                    pts.extend(corners(c));
                    false
                } else {
                    true
                }
            });
            if table.len() == before {
                break;
            }
        }
        tables.push(table);
    }
    tables
}
