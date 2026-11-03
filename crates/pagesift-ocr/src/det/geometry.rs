//! 后处理用的平面几何：凸包、最小外接矩形（旋转卡壳）、四角排序、多边形外扩（unclip）、
//! 凸多边形扫描线填充。坐标用 f64，y 轴向下（图像坐标）。

/// 平面上的点 (x, y)。
pub type P = (f64, f64);

fn cross(o: P, a: P, b: P) -> f64 {
    (a.0 - o.0) * (b.1 - o.1) - (a.1 - o.1) * (b.0 - o.0)
}

/// 凸包（Andrew 单调链），去掉共线点；点数不足 3 或全部共线时返回去重后的端点。
pub fn convex_hull(points: &[P]) -> Vec<P> {
    let mut pts = points.to_vec();
    pts.sort_by(|a, b| a.partial_cmp(b).expect("坐标不能是 NaN"));
    pts.dedup();
    if pts.len() < 3 {
        return pts;
    }
    let mut hull: Vec<P> = Vec::with_capacity(2 * pts.len());
    for pass in 0..2 {
        let base = hull.len();
        let iter: Box<dyn Iterator<Item = &P>> = if pass == 0 {
            Box::new(pts.iter())
        } else {
            Box::new(pts.iter().rev())
        };
        for &p in iter {
            while hull.len() >= base + 2
                && cross(hull[hull.len() - 2], hull[hull.len() - 1], p) <= 0.0
            {
                hull.pop();
            }
            hull.push(p);
        }
        hull.pop(); // 每条链的末点是另一条链的起点
    }
    if hull.len() < 2 {
        // 全部共线：只剩两端
        return vec![pts[0], pts[pts.len() - 1]];
    }
    hull
}

/// 最小面积外接矩形：四个角（沿矩形边依次排）和两条边长 (w, h)。
/// 最优矩形必有一条边与凸包的某条边共线（Freeman–Shapira），逐边试一遍取面积最小的。
pub fn min_area_rect(points: &[P]) -> ([P; 4], f64, f64) {
    let hull = convex_hull(points);
    match hull.len() {
        0 => return ([(0.0, 0.0); 4], 0.0, 0.0),
        1 => return ([hull[0]; 4], 0.0, 0.0),
        _ => {}
    }
    let n = hull.len();
    let mut best: Option<(f64, [P; 4], f64, f64)> = None;
    for i in 0..n {
        let (a, b) = (hull[i], hull[(i + 1) % n]);
        let len = ((b.0 - a.0).powi(2) + (b.1 - a.1).powi(2)).sqrt();
        if len == 0.0 {
            continue;
        }
        let u = ((b.0 - a.0) / len, (b.1 - a.1) / len);
        let v = (-u.1, u.0);
        let (mut u0, mut u1, mut v0, mut v1) = (f64::MAX, f64::MIN, f64::MAX, f64::MIN);
        for p in &hull {
            let pu = p.0 * u.0 + p.1 * u.1;
            let pv = p.0 * v.0 + p.1 * v.1;
            u0 = u0.min(pu);
            u1 = u1.max(pu);
            v0 = v0.min(pv);
            v1 = v1.max(pv);
        }
        let area = (u1 - u0) * (v1 - v0);
        if best.as_ref().is_none_or(|b| area < b.0 - 1e-9) {
            let at = |s: f64, t: f64| (s * u.0 + t * v.0, s * u.1 + t * v.1);
            best = Some((
                area,
                [at(u0, v0), at(u1, v0), at(u1, v1), at(u0, v1)],
                u1 - u0,
                v1 - v0,
            ));
        }
    }
    let (_, corners, w, h) = best.expect("凸包至少有一条非零长的边");
    (corners, w, h)
}

/// 把矩形的四个角排成 左上、右上、右下、左下，同参考实现的 `_get_mini_boxes`：
/// 按 x 排序，左边两点里 y 小的是左上，右边两点里 y 小的是右上。
pub fn order_box(corners: [P; 4]) -> [P; 4] {
    let mut p = corners;
    // 稳定排序，与 Python 的 sorted 一致
    p.sort_by(|a, b| a.0.partial_cmp(&b.0).expect("坐标不能是 NaN"));
    let (tl, bl) = if p[1].1 > p[0].1 {
        (p[0], p[1])
    } else {
        (p[1], p[0])
    };
    let (tr, br) = if p[3].1 > p[2].1 {
        (p[2], p[3])
    } else {
        (p[3], p[2])
    };
    [tl, tr, br, bl]
}

/// 最小外接矩形，排好四角，并给出短边长（参考实现 `_get_mini_boxes` 的两个返回值）。
pub fn mini_box(points: &[P]) -> ([P; 4], f64) {
    let (c, w, h) = min_area_rect(points);
    (order_box(c), w.min(h))
}

/// 多边形面积（取绝对值）。
pub fn area(poly: &[P]) -> f64 {
    signed_area2(poly).abs() / 2.0
}

/// 鞋带公式的两倍有向面积；y 向下时正值表示屏幕上顺时针。
fn signed_area2(poly: &[P]) -> f64 {
    let n = poly.len();
    (0..n)
        .map(|i| {
            let (a, b) = (poly[i], poly[(i + 1) % n]);
            a.0 * b.1 - a.1 * b.0
        })
        .sum()
}

/// 闭合多边形的周长。
pub fn perimeter(poly: &[P]) -> f64 {
    let n = poly.len();
    (0..n)
        .map(|i| {
            let (a, b) = (poly[i], poly[(i + 1) % n]);
            ((b.0 - a.0).powi(2) + (b.1 - a.1).powi(2)).sqrt()
        })
        .sum()
}

/// 把多边形每条边向外平移 d，相邻两条平移后的边求交得到新顶点（尖角连接）。
///
/// 外扩距离 d = 面积 × ratio / 周长，出自 DB 论文（Liao 等，2020）的 unclip：训练时文字区域
/// 按 D = A(1 − r²)/L 向内收缩，推理时反过来按 D' = A'·r'/L' 向外扩回去。对 w × h 的矩形，
/// d = w·h·r / (2(w + h))，外扩后是 (w + 2d) × (h + 2d) 的矩形。
/// 参考实现的 `_unclip` 也是这样逐边平移求交（不用 pyclipper 的圆角连接）；对矩形两者一样。
/// 周长为 0 时无法外扩，返回 `None`。
pub fn unclip(poly: &[P], ratio: f64) -> Option<Vec<P>> {
    let n = poly.len();
    let per = perimeter(poly);
    if n < 3 || per <= 0.0 {
        return None;
    }
    let d = area(poly) * ratio / per;
    // [推导] 外法向：有向面积为正（y 向下即屏幕顺时针）时，边方向 (dx, dy) 的外法向是 (dy, −dx)
    let ccw = signed_area2(poly) > 0.0;
    let dirs: Vec<P> = (0..n)
        .map(|i| {
            let (a, b) = (poly[i], poly[(i + 1) % n]);
            let (dx, dy) = (b.0 - a.0, b.1 - a.1);
            // [协议] 参考实现 `_unclip` 的 1e-6：避免零长边除零
            let l = (dx * dx + dy * dy).sqrt().max(1e-6);
            (dx / l, dy / l)
        })
        .collect();
    let normals: Vec<P> = dirs
        .iter()
        .map(|&(x, y)| if ccw { (y, -x) } else { (-y, x) })
        .collect();
    let shifted: Vec<P> = (0..n)
        .map(|i| (poly[i].0 + d * normals[i].0, poly[i].1 + d * normals[i].1))
        .collect();
    let out = (0..n)
        .map(|i| {
            let j = (i + n - 1) % n; // 前一条边
            let c = dirs[j].0 * dirs[i].1 - dirs[j].1 * dirs[i].0;
            // [协议] 参考实现 `_unclip` 的平行判定阈值 1e-6；平行时取两条法向的平均
            if c.abs() < 1e-6 {
                return (
                    poly[i].0 + 0.5 * d * (normals[j].0 + normals[i].0),
                    poly[i].1 + 0.5 * d * (normals[j].1 + normals[i].1),
                );
            }
            // 前一条边的直线：shifted[j] + t·dirs[j]；当前边过 shifted[i]、方向 dirs[i]
            let v = (shifted[i].0 - shifted[j].0, shifted[i].1 - shifted[j].1);
            let t = (v.0 * dirs[i].1 - v.1 * dirs[i].0) / c;
            (shifted[j].0 + t * dirs[j].0, shifted[j].1 + t * dirs[j].1)
        })
        .collect();
    Some(out)
}

/// 凸多边形（顶点为整数像素坐标）覆盖的像素，按 `cv2.fillPoly` 的习惯边界也算在内。
/// 对每个落在 `0..h` 的行 y，给出 `[x0, x1]`（已裁到 `0..w`），行内无像素时不给。
pub fn fill_convex_rows(poly: &[(i32, i32)], w: usize, h: usize) -> Vec<(usize, usize, usize)> {
    if poly.is_empty() || w == 0 || h == 0 {
        return Vec::new();
    }
    let ymin = poly.iter().map(|p| p.1).min().unwrap().max(0);
    let ymax = poly.iter().map(|p| p.1).max().unwrap().min(h as i32 - 1);
    let n = poly.len();
    let mut rows = Vec::new();
    for y in ymin..=ymax {
        let (mut lo, mut hi) = (f64::MAX, f64::MIN);
        let yf = y as f64;
        for i in 0..n {
            let (a, b) = (poly[i], poly[(i + 1) % n]);
            let (y0, y1) = (a.1.min(b.1), a.1.max(b.1));
            if y < y0 || y > y1 {
                continue;
            }
            if a.1 == b.1 {
                // 水平边：两端都算
                lo = lo.min(a.0.min(b.0) as f64);
                hi = hi.max(a.0.max(b.0) as f64);
            } else {
                let x = a.0 as f64 + (yf - a.1 as f64) * (b.0 - a.0) as f64 / (b.1 - a.1) as f64;
                lo = lo.min(x);
                hi = hi.max(x);
            }
        }
        if lo > hi {
            continue;
        }
        // [推导] 边上的像素算在内：左端向上取整、右端向下取整（留 1e-9 吃掉除法误差）
        let x0 = (lo - 1e-9).ceil().max(0.0);
        let x1 = (hi + 1e-9).floor().min(w as f64 - 1.0);
        if x0 <= x1 {
            rows.push((y as usize, x0 as usize, x1 as usize));
        }
    }
    rows
}

#[cfg(test)]
mod tests {
    use super::*;

    fn close(a: P, b: P) -> bool {
        (a.0 - b.0).abs() < 1e-6 && (a.1 - b.1).abs() < 1e-6
    }

    #[test]
    fn 凸包去掉内点和共线点() {
        let pts = [
            (0.0, 0.0),
            (2.0, 0.0),
            (1.0, 0.0),
            (2.0, 2.0),
            (0.0, 2.0),
            (1.0, 1.0),
        ];
        let h = convex_hull(&pts);
        assert_eq!(h.len(), 4, "{h:?}");
        assert_eq!(convex_hull(&[(0.0, 0.0), (1.0, 1.0), (3.0, 3.0)]).len(), 2);
    }

    #[test]
    fn 轴对齐矩形() {
        let pts = [(1.0, 1.0), (1.0, 3.0), (4.0, 3.0), (4.0, 1.0), (2.0, 2.0)];
        let (b, short) = mini_box(&pts);
        assert!((short - 2.0).abs() < 1e-9);
        let want = [(1.0, 1.0), (4.0, 1.0), (4.0, 3.0), (1.0, 3.0)];
        for (a, w) in b.iter().zip(want) {
            assert!(close(*a, w), "{b:?}");
        }
    }

    #[test]
    fn 旋转矩形() {
        // 绕原点转 30° 的 10 × 2 矩形，再加些内点
        let (s, c) = (30f64.to_radians().sin(), 30f64.to_radians().cos());
        let rot = |x: f64, y: f64| (x * c - y * s + 50.0, x * s + y * c + 50.0);
        let mut pts = vec![rot(0.0, 0.0), rot(10.0, 0.0), rot(10.0, 2.0), rot(0.0, 2.0)];
        pts.push(rot(5.0, 1.0));
        pts.push(rot(3.0, 0.5));
        let (corners, w, h) = min_area_rect(&pts);
        assert!((w * h - 20.0).abs() < 1e-6, "{w} × {h}");
        assert!((w.min(h) - 2.0).abs() < 1e-6);
        let (b, _) = mini_box(&pts);
        // 左上角是 x 最小的两点里 y 小的那个
        assert!(
            close(b[0], rot(0.0, 0.0)) || close(b[0], rot(0.0, 2.0)),
            "{b:?}"
        );
        assert_eq!(corners.len(), 4);
    }

    #[test]
    fn 退化情形() {
        let (_, s) = mini_box(&[(3.0, 3.0)]);
        assert_eq!(s, 0.0);
        let (_, w, h) = min_area_rect(&[(0.0, 5.0), (4.0, 5.0), (2.0, 5.0)]);
        assert!((w.max(h) - 4.0).abs() < 1e-9 && w.min(h).abs() < 1e-9);
    }

    #[test]
    fn 矩形外扩() {
        // 10 × 4、ratio 1.5：d = 40 × 1.5 / 28
        let r = [(0.0, 0.0), (10.0, 0.0), (10.0, 4.0), (0.0, 4.0)];
        let d = 40.0 * 1.5 / 28.0;
        let out = unclip(&r, 1.5).unwrap();
        let want = [(-d, -d), (10.0 + d, -d), (10.0 + d, 4.0 + d), (-d, 4.0 + d)];
        for (a, w) in out.iter().zip(want) {
            assert!(close(*a, w), "{out:?}");
        }
        // 反向（屏幕逆时针）给出同样的结果
        let rev: Vec<P> = r.iter().rev().copied().collect();
        let out2 = unclip(&rev, 1.5).unwrap();
        assert!((area(&out2) - (10.0 + 2.0 * d) * (4.0 + 2.0 * d)).abs() < 1e-9);
        assert!(unclip(&[(1.0, 1.0); 4], 1.5).is_none());
    }

    #[test]
    fn 扫描线填充含边界() {
        let rows = fill_convex_rows(&[(1, 1), (3, 1), (3, 2), (1, 2)], 10, 10);
        assert_eq!(rows, vec![(1, 1, 3), (2, 1, 3)]);
        // 菱形
        let rows = fill_convex_rows(&[(2, 0), (4, 2), (2, 4), (0, 2)], 10, 10);
        assert_eq!(rows[0], (0, 2, 2));
        assert_eq!(rows[2], (2, 0, 4));
        // 越界裁剪
        let rows = fill_convex_rows(&[(-3, -3), (5, -3), (5, 1), (-3, 1)], 4, 4);
        assert_eq!(rows, vec![(0, 0, 3), (1, 0, 3)]);
    }
}
