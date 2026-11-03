//! 2D 几何与排版图形状态基础类型（矩阵、线段、字形与页面墨迹）。

use crate::font::MathKind;

#[derive(Debug, Clone)]
pub(crate) struct Glyph {
    pub(crate) text: crate::short::Short,
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
    /// 既不水平也不竖直的线段数（折线图、箭头、饼图的边），到 u16 上限为止。
    pub(crate) slanted: u16,
    /// 曲线段数（饼图、圆点、圆角），到 u16 上限为止。
    pub(crate) curves: u16,
    /// 是 `re` 画的矩形。
    pub(crate) rect: bool,
    pub(crate) fill: bool,
    /// 白色的填充或描边（白纸上看不见）：不算图形证据，但它的边不当刻度。
    pub(crate) visible: bool,
}

/// 页面上画的一次图像（Do 一个图像 XObject，或一段内联图像），页面坐标的外框。
#[derive(Debug, Clone)]
pub(crate) struct Placed {
    pub(crate) source: ImageSource,
    pub(crate) x0: f64,
    pub(crate) y0: f64,
    pub(crate) x1: f64,
    pub(crate) y1: f64,
}

#[derive(Debug, Clone)]
pub(crate) enum ImageSource {
    /// 图像 XObject 的对象号。
    Object(crate::file::ObjectId),
    /// 内联图像：`BI` 与 `ID` 之间的字典文本、`ID` 与 `EI` 之间的数据。
    Inline(std::rc::Rc<(Vec<u8>, Vec<u8>)>),
}

/// 解释一页时顺带数的文字质量证据（见 quality.rs）。
#[derive(Debug, Default, Clone, Copy)]
pub(crate) struct TextEvidence {
    /// 解码出的字符数（不含空白）。
    pub(crate) chars: usize,
    /// 其中的乱码字符：U+FFFD、私用区、C0/C1 控制字符。
    pub(crate) garbled: usize,
    /// 字体给不出 Unicode、被丢掉的字形数。
    pub(crate) unmapped: usize,
    /// 内容流里相邻两个字形之间的换行次数。
    pub(crate) breaks: usize,
    /// 其中往回跳的次数：下一个字形比上一个高出一行以上，且横向范围重叠（同一栏里倒着走）。
    pub(crate) jumps: usize,
    /// 页面上没画的按钮表单域个数（没有别的字时复核用）。
    pub(crate) buttons: usize,
}

#[derive(Debug, Default)]
pub(crate) struct PageInk {
    /// 水平书写的字，页面坐标。
    pub(crate) glyphs: Vec<Glyph>,
    /// 书写方向是 90°、180°、270° 的字，各自在“把这个方向转正”的坐标系里（见 orient.rs）。
    pub(crate) turned: [Vec<Glyph>; 3],
    pub(crate) rules: Vec<Rule>,
    pub(crate) shapes: Vec<Shape>,
    pub(crate) image_area: f64,
    pub(crate) image_count: usize,
    /// 每次画的图像（注释外观里的不算）。
    pub(crate) images: Vec<Placed>,
    pub(crate) evidence: TextEvidence,
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
