//! 解释时顺带记下的证据：图像放在页面上的位置（图片识别用）、阅读顺序的换行与跳变（质量分用）。

use super::*;

impl Interp<'_> {
    /// 阅读顺序证据：内容流里相邻两个字形换了行（基线差超过半个字号）记一次换行；
    /// 换到比上一行高一行以上、横向又和上一行重叠的位置（同一栏里往回走）记一次跳变。
    pub(super) fn order(&mut self, x: f64, x_end: f64, y: f64, size: f64) {
        let (lo, hi) = (x.min(x_end), x.max(x_end));
        match self.last {
            Some((_, _, ly, lsize)) if (y - ly).abs() > lsize.max(size) / 2.0 => {
                let ev = &mut self.ink.evidence;
                ev.breaks += 1;
                let (s0, s1) = self.line_span;
                if y - ly > lsize.max(size) && lo < s1 + size && hi > s0 - size {
                    ev.jumps += 1;
                }
                self.line_span = (lo, hi);
            }
            Some(_) => self.line_span = (self.line_span.0.min(lo), self.line_span.1.max(hi)),
            None => self.line_span = (lo, hi),
        }
        self.last = Some((x, x_end, y, size));
    }

    pub(super) fn image(&mut self, ctm: &Matrix, source: Option<ImageSource>) {
        if self.text_only {
            return;
        }
        let area = ctm.det().abs();
        if area.is_finite() {
            self.ink.image_count += 1;
            self.ink.image_area += area.min(self.ink.width * self.ink.height);
        }
        // 图像画在单位正方形上（ISO 32000-2 §8.9.4），外框是它四个角经 CTM 变换后的范围
        let Some(source) = source else { return };
        if self.ink.images.len() >= MAX_SHAPES_PER_PAGE {
            return;
        }
        let pts = [
            ctm.apply(0.0, 0.0),
            ctm.apply(1.0, 0.0),
            ctm.apply(0.0, 1.0),
            ctm.apply(1.0, 1.0),
        ];
        let (mut x0, mut y0, mut x1, mut y1) = (f64::MAX, f64::MAX, f64::MIN, f64::MIN);
        for (x, y) in pts {
            (x0, y0, x1, y1) = (x0.min(x), y0.min(y), x1.max(x), y1.max(y));
        }
        if [x0, y0, x1, y1].iter().all(|v| v.is_finite()) {
            self.ink.images.push(Placed {
                source,
                x0,
                y0,
                x1,
                y1,
            });
        }
    }
}
