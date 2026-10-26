//! 绘图路径：记下每次绘制的路径外框和线段构成（识别矢量图用）。

use super::{GState, Interp, MAX_SHAPES_PER_PAGE, RULE_SKEW, Shape};
use crate::file::{Dictionary, Object};
use crate::lexer::Operand;
use crate::obj::{deref, get_dict};

/// 颜色空间的大类，用来判断颜色是不是白色。
#[derive(Debug, Clone, Copy, PartialEq)]
pub(super) enum Space {
    Gray,
    Rgb,
    Cmyk,
    /// Separation、DeviceN：色值是油墨量，全 0 是没上墨（白）。
    Tint,
    Other,
}

/// 当前的填充或描边颜色：白色（白纸上看不见）的填充、描边不算图形证据（Word 表格单元格的留白填充等）。
#[derive(Debug, Clone, Copy)]
pub(super) struct Paint {
    pub(super) space: Space,
    pub(super) white: bool,
}

impl Paint {
    /// 初始颜色：DeviceGray 的黑色。
    pub(super) const BLACK: Paint = Paint {
        space: Space::Gray,
        white: false,
    };
}

fn white_of(space: Space, v: &[f64]) -> bool {
    match space {
        Space::Gray => v.len() == 1 && v[0] >= 0.99,
        Space::Rgb => v.len() == 3 && v.iter().all(|&x| x >= 0.99),
        Space::Cmyk => v.len() == 4 && v.iter().all(|&x| x <= 0.01),
        Space::Tint => !v.is_empty() && v.iter().all(|&x| x <= 0.01),
        Space::Other => false,
    }
}

impl Interp<'_> {
    /// 颜色操作符（g/G、rg/RG、k/K、cs/CS、sc/scn/SC/SCN）：更新填充或描边颜色。不是颜色操作符返回 false。
    pub(super) fn color(
        &self,
        op: &[u8],
        args: &[Operand],
        resources: Option<&Dictionary>,
        fill: &mut Paint,
        stroke: &mut Paint,
    ) -> bool {
        let nums: Vec<f64> = args.iter().filter_map(Operand::num).collect();
        let (target, space) = match op {
            b"g" => (fill, Some(Space::Gray)),
            b"G" => (stroke, Some(Space::Gray)),
            b"rg" => (fill, Some(Space::Rgb)),
            b"RG" => (stroke, Some(Space::Rgb)),
            b"k" => (fill, Some(Space::Cmyk)),
            b"K" => (stroke, Some(Space::Cmyk)),
            b"cs" | b"CS" => {
                let target = if op == b"cs" { fill } else { stroke };
                let space = match args.first() {
                    Some(Operand::Name(n)) => self.space_of(n, resources),
                    _ => Space::Other,
                };
                // 换颜色空间后是该空间的初始颜色（黑色、满墨），不是白
                *target = Paint {
                    space,
                    white: false,
                };
                return true;
            }
            b"sc" | b"scn" => (fill, None),
            b"SC" | b"SCN" => (stroke, None),
            _ => return false,
        };
        let space = space.unwrap_or(target.space);
        // 图案（最后一个操作数是名字）不当白色
        let pattern = matches!(args.last(), Some(Operand::Name(_)));
        *target = Paint {
            space,
            white: !pattern && white_of(space, &nums),
        };
        true
    }

    /// 颜色空间名（或 /ColorSpace 资源里的名字）属于哪一类。
    fn space_of(&self, name: &[u8], resources: Option<&Dictionary>) -> Space {
        match name {
            b"DeviceGray" | b"CalGray" | b"G" => return Space::Gray,
            b"DeviceRGB" | b"CalRGB" | b"RGB" => return Space::Rgb,
            b"DeviceCMYK" | b"CMYK" => return Space::Cmyk,
            b"Pattern" => return Space::Other,
            _ => {}
        }
        let Some(cs) = resources
            .and_then(|r| get_dict(self.doc, r, b"ColorSpace"))
            .and_then(|d| d.get(name))
        else {
            return Space::Other;
        };
        match deref(self.doc, cs) {
            Object::Name(n) if n.as_slice() != name => self.space_of(n, None),
            Object::Array(a) => match a.first().map(|o| deref(self.doc, o)) {
                Some(Object::Name(n)) => match n.as_slice() {
                    b"CalGray" => Space::Gray,
                    b"CalRGB" => Space::Rgb,
                    b"Separation" | b"DeviceN" => Space::Tint,
                    b"ICCBased" => match a.get(1).map(|o| deref(self.doc, o)) {
                        Some(Object::Stream(st)) => match st
                            .dict
                            .get(b"N")
                            .and_then(crate::obj::num)
                            .map(|n| n as i64)
                        {
                            Some(1) => Space::Gray,
                            Some(3) => Space::Rgb,
                            Some(4) => Space::Cmyk,
                            _ => Space::Other,
                        },
                        _ => Space::Other,
                    },
                    _ => Space::Other,
                },
                _ => Space::Other,
            },
            _ => Space::Other,
        }
    }

    /// 记下一次绘制的路径：每个 `re` 矩形一条，其余线段和曲线合成一条（页面坐标外框）。
    pub(super) fn shapes(
        &mut self,
        segs: &[(f64, f64, f64, f64)],
        rects: &[(f64, f64, f64, f64)],
        curves: u32,
        pbox: [f64; 4],
        gs: &GState,
        fill: bool,
    ) {
        let ctm = &gs.ctm;
        // 白色的填充（或白色描边）在白纸上看不见
        let visible = !(if fill { gs.fill.white } else { gs.stroke.white });
        let page_box = |b: [f64; 4]| {
            let p = [
                ctm.apply(b[0], b[1]),
                ctm.apply(b[2], b[1]),
                ctm.apply(b[2], b[3]),
                ctm.apply(b[0], b[3]),
            ];
            let mut out = EMPTY_BOX;
            for q in p {
                grow(&mut out, q);
            }
            out
        };
        let mut push = |b: [f64; 4], slanted: u32, curves: u32, rect: bool| {
            if self.ink.shapes.len() < MAX_SHAPES_PER_PAGE && b.iter().all(|v| v.is_finite()) {
                self.ink.shapes.push(Shape {
                    x0: b[0],
                    y0: b[1],
                    x1: b[2],
                    y1: b[3],
                    slanted,
                    curves,
                    rect,
                    fill,
                    visible,
                });
            }
        };
        for &(x, y, w, h) in rects {
            push(
                page_box([x.min(x + w), y.min(y + h), x.max(x + w), y.max(y + h)]),
                0,
                0,
                true,
            );
        }
        if !segs.is_empty() || curves > 0 {
            let slanted = segs
                .iter()
                .filter(|&&(x0, y0, x1, y1)| {
                    let (a, b) = (ctm.apply(x0, y0), ctm.apply(x1, y1));
                    (a.0 - b.0).abs() > RULE_SKEW && (a.1 - b.1).abs() > RULE_SKEW
                })
                .count() as u32;
            push(page_box(pbox), slanted, curves, false);
        }
    }
}

pub(super) const EMPTY_BOX: [f64; 4] = [
    f64::INFINITY,
    f64::INFINITY,
    f64::NEG_INFINITY,
    f64::NEG_INFINITY,
];

pub(super) fn grow(b: &mut [f64; 4], p: (f64, f64)) {
    b[0] = b[0].min(p.0);
    b[1] = b[1].min(p.1);
    b[2] = b[2].max(p.0);
    b[3] = b[3].max(p.1);
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn white_by_space() {
        assert!(white_of(Space::Gray, &[1.0]) && !white_of(Space::Gray, &[0.5]));
        assert!(white_of(Space::Rgb, &[1.0, 1.0, 1.0]) && !white_of(Space::Rgb, &[1.0, 1.0, 0.8]));
        assert!(
            white_of(Space::Cmyk, &[0.0, 0.0, 0.0, 0.0])
                && !white_of(Space::Cmyk, &[0.0, 0.0, 0.0, 1.0])
        );
        assert!(white_of(Space::Tint, &[0.0]) && !white_of(Space::Tint, &[1.0]));
        assert!(!white_of(Space::Other, &[1.0]));
    }
}
