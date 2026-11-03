//! 二值图的连通域与外轮廓，取代参考实现里的 `cv2.findContours`。
//!
//! - 前景按 8 邻接连通（与 OpenCV 对前景的约定相同），图外一律当背景；
//! - 每个连通域从它在光栅顺序里的第一个像素出发，用 Suzuki–Abe（1985）的外边界跟踪
//!   绕一圈，得到外轮廓上的像素中心坐标；
//! - 只要外轮廓：参考实现用 `RETR_LIST`，还会把连通域里的洞的边界当成轮廓，那些多半是
//!   行内的重复小框，这里不要；
//! - 输出像 `CHAIN_APPROX_SIMPLE` 一样，把同方向的连续步并成一段、只留拐点。
//!
//! 后面只用到轮廓的凸包（最小外接矩形），所以拐点压缩不改变结果。

/// 一个像素坐标 (x, y)。
pub type Pt = (i32, i32);

/// [协议] 8 个方向，按屏幕上的逆时针排（y 向下）：东、东北、北、西北、西、西南、南、东南。
const DIRS: [Pt; 8] = [
    (1, 0),
    (1, -1),
    (0, -1),
    (-1, -1),
    (-1, 0),
    (-1, 1),
    (0, 1),
    (1, 1),
];

/// 从 `a` 指向相邻像素 `b` 的方向下标。
fn dir_of(a: Pt, b: Pt) -> usize {
    let d = (b.0 - a.0, b.1 - a.1);
    DIRS.iter().position(|&x| x == d).expect("两点不相邻")
}

/// 二值图（按行存放）。
pub struct Bitmap<'a> {
    pub data: &'a [bool],
    pub width: usize,
    pub height: usize,
}

impl Bitmap<'_> {
    #[inline]
    fn fg(&self, p: Pt) -> bool {
        p.0 >= 0
            && p.1 >= 0
            && (p.0 as usize) < self.width
            && (p.1 as usize) < self.height
            && self.data[p.1 as usize * self.width + p.0 as usize]
    }
}

/// 给 8 邻接连通域编号，返回每个连通域在光栅顺序里的第一个像素（也就是它最上一行最左的像素）。
fn component_starts(bm: &Bitmap) -> Vec<Pt> {
    let (w, h) = (bm.width, bm.height);
    let mut seen = vec![false; w * h];
    let mut starts = Vec::new();
    let mut stack = Vec::new();
    for i in 0..w * h {
        if !bm.data[i] || seen[i] {
            continue;
        }
        starts.push(((i % w) as i32, (i / w) as i32));
        seen[i] = true;
        stack.push(i);
        while let Some(j) = stack.pop() {
            let p = ((j % w) as i32, (j / w) as i32);
            for d in DIRS {
                let q = (p.0 + d.0, p.1 + d.1);
                if bm.fg(q) {
                    let k = q.1 as usize * w + q.0 as usize;
                    if !seen[k] {
                        seen[k] = true;
                        stack.push(k);
                    }
                }
            }
        }
    }
    starts
}

/// 从外边界的起点 `start`（左邻是背景）出发，沿外边界逆时针（屏幕上）跟踪一圈。
/// Suzuki–Abe 的步骤 3.1–3.5：先顺时针找到最后一个边界点 p1，再反复以当前点为中心、
/// 从上一点的下一个方向起逆时针找下一点，回到起点且当前点是 p1 时结束。
fn trace_outer(bm: &Bitmap, start: Pt) -> Vec<Pt> {
    // 3.1：从西邻起顺时针找第一个前景邻点
    let west = 4;
    let Some(d1) = (0..8)
        .map(|k| (west + 8 - k) % 8)
        .find(|&d| bm.fg((start.0 + DIRS[d].0, start.1 + DIRS[d].1)))
    else {
        return vec![start]; // 孤立像素
    };
    let p1 = (start.0 + DIRS[d1].0, start.1 + DIRS[d1].1);
    let (mut p2, mut p3) = (p1, start);
    let mut out = Vec::new();
    loop {
        out.push(p3);
        // 3.3：从 p2 之后的方向起逆时针找
        let d2 = dir_of(p3, p2);
        let p4 = (1..=8)
            .map(|k| (d2 + k) % 8)
            .map(|d| (p3.0 + DIRS[d].0, p3.1 + DIRS[d].1))
            .find(|&q| bm.fg(q))
            .expect("p2 本身是前景，必能找到");
        // 3.5
        if p4 == start && p3 == p1 {
            break;
        }
        p2 = p3;
        p3 = p4;
    }
    out
}

/// 把同方向的连续步合并，只留拐点（`CHAIN_APPROX_SIMPLE`）。
fn compress(path: Vec<Pt>) -> Vec<Pt> {
    let n = path.len();
    if n < 3 {
        return path;
    }
    let step = |a: Pt, b: Pt| (b.0 - a.0, b.1 - a.1);
    (0..n)
        .filter(|&i| {
            let prev = path[(i + n - 1) % n];
            let next = path[(i + 1) % n];
            step(prev, path[i]) != step(path[i], next)
        })
        .map(|i| path[i])
        .collect()
}

/// 二值图里所有连通域的外轮廓（拐点），按连通域的光栅顺序排。
pub fn outer_contours(bm: &Bitmap) -> Vec<Vec<Pt>> {
    component_starts(bm)
        .into_iter()
        .map(|s| compress(trace_outer(bm, s)))
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn bitmap(rows: &[&str]) -> (Vec<bool>, usize, usize) {
        let w = rows[0].len();
        let data = rows
            .iter()
            .flat_map(|r| r.bytes().map(|b| b == b'#'))
            .collect();
        (data, w, rows.len())
    }

    fn contours(rows: &[&str]) -> Vec<Vec<Pt>> {
        let (data, width, height) = bitmap(rows);
        outer_contours(&Bitmap {
            data: &data,
            width,
            height,
        })
    }

    #[test]
    fn 矩形只留四个角() {
        let c = contours(&["......", ".####.", ".####.", ".####.", "......"]);
        assert_eq!(c.len(), 1);
        // 屏幕上逆时针：左上 → 左下 → 右下 → 右上
        assert_eq!(c[0], vec![(1, 1), (1, 3), (4, 3), (4, 1)]);
    }

    #[test]
    fn 八邻接连通且贴边也行() {
        // 对角相连算一个连通域；贴着图边
        let c = contours(&["#..", ".#.", "..#"]);
        assert_eq!(c.len(), 1);
        let mut pts = c[0].clone();
        pts.sort();
        pts.dedup();
        assert_eq!(pts, vec![(0, 0), (2, 2)]);
    }

    #[test]
    fn 多个连通域和孤立点() {
        let c = contours(&["##..#", "##...", ".....", "..###"]);
        assert_eq!(c.len(), 3);
        assert_eq!(c[0], vec![(0, 0), (0, 1), (1, 1), (1, 0)]);
        assert_eq!(c[1], vec![(4, 0)]);
        // 单行的连通域：来回各走一遍，压缩后剩两端
        assert_eq!(c[2], vec![(2, 3), (4, 3)]);
    }

    #[test]
    fn 带洞的环只给外轮廓() {
        let c = contours(&["#####", "#...#", "#...#", "#####"]);
        assert_eq!(c.len(), 1);
        assert_eq!(c[0], vec![(0, 0), (0, 3), (4, 3), (4, 0)]);
    }

    #[test]
    fn 凹形轮廓经过每个外边界像素() {
        let rows = ["#.#", "#.#", "###"];
        let (data, width, height) = bitmap(&rows);
        let bm = Bitmap {
            data: &data,
            width,
            height,
        };
        let path = trace_outer(&bm, (0, 0));
        let mut pts = path.clone();
        pts.sort();
        pts.dedup();
        assert_eq!(pts.len(), 7, "{path:?}");
    }
}
