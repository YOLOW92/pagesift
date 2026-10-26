//! 2D 几何与排版图形状态基础类型（矩阵、线段、字形与页面墨迹）。

use crate::font::MathKind;

#[derive(Debug, Clone)]
pub(crate) struct Glyph {
    pub(crate) text: String,
    pub(crate) x: f64,
    pub(crate) y: f64,
    pub(crate) x_end: f64,
    pub(crate) size: f64,
    pub(crate) bold: bool,
    pub(crate) math: MathKind,
    /// 所在标记内容的 MCID（结构树用它把文字挂到元素上）；表单 XObject 里的不记。
    pub(crate) mcid: Option<i32>,
    /// 在 `/Artifact` 标记内容里：写入方声明的非正文（页眉、页脚、页码、装饰）。
    pub(crate) artifact: bool,
}

/// 页面上的一条水平或竖直线段（表格框线、分隔线），页面坐标。
#[derive(Debug, Clone, Copy, PartialEq)]
pub(crate) struct Rule {
    pub(crate) x0: f64,
    pub(crate) y0: f64,
    pub(crate) x1: f64,
    pub(crate) y1: f64,
}

impl Rule {
    pub(crate) fn horizontal(&self) -> bool {
        (self.y1 - self.y0).abs() <= (self.x1 - self.x0).abs()
    }
}

/// 一次绘制的路径（填充或描边），页面坐标的外框：识别矢量图（折线、柱、饼、图例色块）用。
/// `re` 画的矩形各记一个；其余线段和曲线合成一个。
#[derive(Debug, Clone, Copy, PartialEq)]
pub(crate) struct Shape {
    pub(crate) x0: f64,
    pub(crate) y0: f64,
    pub(crate) x1: f64,
    pub(crate) y1: f64,
    /// 既不水平也不竖直的线段数（折线图、箭头、饼图的边）。
    pub(crate) slanted: u32,
    /// 曲线段数（饼图、圆点、圆角）。
    pub(crate) curves: u32,
    /// 是 `re` 画的矩形。
    pub(crate) rect: bool,
    pub(crate) fill: bool,
    /// 白色的填充或描边（白纸上看不见）：不算图形证据，但它的边不当刻度。
    pub(crate) visible: bool,
}

#[derive(Debug, Default)]
pub(crate) struct PageInk {
    pub(crate) glyphs: Vec<Glyph>,
    pub(crate) rules: Vec<Rule>,
    pub(crate) shapes: Vec<Shape>,
    pub(crate) image_area: f64,
    pub(crate) image_count: usize,
    pub(crate) width: f64,
    pub(crate) height: f64,
    /// 用户空间 → 页面坐标（书签目标的位置要用它换算）；页面旋转过时为 None。
    pub(crate) base: Option<Matrix>,
}

#[derive(Debug, Clone, Copy, PartialEq)]
pub(crate) struct Matrix(pub(crate) [f64; 6]);

impl Matrix {
    pub(crate) const IDENTITY: Matrix = Matrix([1.0, 0.0, 0.0, 1.0, 0.0, 0.0]);

    pub(crate) fn translate(tx: f64, ty: f64) -> Matrix {
        Matrix([1.0, 0.0, 0.0, 1.0, tx, ty])
    }

    pub(crate) fn from_slice(v: &[f64]) -> Option<Matrix> {
        let m: [f64; 6] = v.get(..6)?.try_into().ok()?;
        m.iter().all(|x| x.is_finite()).then_some(Matrix(m))
    }

    /// self × other：先应用 self，再应用 other。
    pub(crate) fn mul(&self, o: &Matrix) -> Matrix {
        let [a1, b1, c1, d1, e1, f1] = self.0;
        let [a2, b2, c2, d2, e2, f2] = o.0;
        Matrix([
            a1 * a2 + b1 * c2,
            a1 * b2 + b1 * d2,
            c1 * a2 + d1 * c2,
            c1 * b2 + d1 * d2,
            e1 * a2 + f1 * c2 + e2,
            e1 * b2 + f1 * d2 + f2,
        ])
    }

    pub(crate) fn apply(&self, x: f64, y: f64) -> (f64, f64) {
        let [a, b, c, d, e, f] = self.0;
        (a * x + c * y + e, b * x + d * y + f)
    }

    pub(crate) fn det(&self) -> f64 {
        self.0[0] * self.0[3] - self.0[1] * self.0[2]
    }
}
