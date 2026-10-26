//! 内容流解释：跟踪 CTM 和文本矩阵，算出每个字形的位置和字号，统计图像覆盖面积。

use std::collections::HashMap;
use std::rc::Rc;
use std::time::Instant;

use crate::file::{Dictionary, Document, Object, ObjectId};

use crate::font::{DecodedChar, Font};
pub(crate) use crate::geom::{Glyph, Matrix, PageInk, Rule};
use crate::lexer::{Lexer, Operand};
use crate::obj::{deref, get, get_dict, get_name, num_array, stream_bytes};

/// 一条线段最多可以偏离水平或竖直多少（页面单位）。
const RULE_SKEW: f64 = 1.0;
/// 填充的矩形短边不超过这个宽度时当作一条线（很多 PDF 用细长矩形画表格线）。
const THIN_RECT: f64 = 2.5;
const MAX_RULES_PER_PAGE: usize = 20_000;

#[derive(Clone)]
struct GState {
    ctm: Matrix,
    font: Option<Rc<Font>>,
    size: f64,
    char_spacing: f64,
    word_spacing: f64,
    h_scale: f64,
    leading: f64,
    rise: f64,
}

#[derive(Debug)]
pub(crate) struct Timeout;

pub(crate) struct Interp<'a> {
    pub(crate) doc: &'a Document<'a>,
    pub(crate) fonts: &'a mut HashMap<ObjectId, Rc<Font>>,
    pub(crate) deadline: Option<Instant>,
    pub(crate) stream_limit: usize,
    ops_left: usize,
    ink: PageInk,
    fallback: Rc<Font>,
    /// 标记内容栈（BMC/BDC 压入，EMC 弹出）。
    marked: Vec<Marked>,
}

/// 一层标记内容。
#[derive(Clone, Copy)]
struct Marked {
    mcid: Option<i32>,
    artifact: bool,
}

/// 标记内容最多嵌套这么深，超出的 BDC 不再压栈（对应的 EMC 也就弹不到真实的层）。
const MAX_MARKED_DEPTH: usize = 64;

const MAX_FORM_DEPTH: usize = 8;
const MAX_STATE_DEPTH: usize = 256;
const MAX_OPS_PER_PAGE: usize = 5_000_000;
const MAX_GLYPHS_PER_PAGE: usize = 500_000;

impl<'a> Interp<'a> {
    pub(crate) fn new(
        doc: &'a Document,
        fonts: &'a mut HashMap<ObjectId, Rc<Font>>,
        deadline: Option<Instant>,
        stream_limit: usize,
    ) -> Self {
        Interp {
            doc,
            fonts,
            deadline,
            stream_limit,
            ops_left: MAX_OPS_PER_PAGE,
            ink: PageInk::default(),
            fallback: Rc::new(Font::fallback()),
            marked: Vec::new(),
        }
    }

    /// 解释一页，返回字形和图像统计。坐标已换算到页面显示方向（考虑 /Rotate）。
    pub(crate) fn run_page(mut self, page: &Dictionary) -> Result<PageInk, Timeout> {
        let doc = self.doc;
        let bbox = inherited(doc, page, b"CropBox")
            .or_else(|| inherited(doc, page, b"MediaBox"))
            .map(|o| num_array(doc, o))
            .filter(|v| v.len() == 4)
            .unwrap_or_else(|| vec![0.0, 0.0, 612.0, 792.0]);
        let (llx, lly) = (bbox[0].min(bbox[2]), bbox[1].min(bbox[3]));
        let w = (bbox[2] - bbox[0]).abs().max(1.0);
        let h = (bbox[3] - bbox[1]).abs().max(1.0);
        let rotate = inherited(doc, page, b"Rotate")
            .and_then(crate::obj::num)
            .map(|r| (r as i64).rem_euclid(360))
            .unwrap_or(0);
        let (rot, dw, dh) = match rotate {
            90 => (Matrix([0.0, -1.0, 1.0, 0.0, 0.0, w]), h, w),
            180 => (Matrix([-1.0, 0.0, 0.0, -1.0, w, h]), w, h),
            270 => (Matrix([0.0, 1.0, -1.0, 0.0, h, 0.0]), h, w),
            _ => (Matrix::IDENTITY, w, h),
        };
        self.ink.width = dw;
        self.ink.height = dh;
        let ctm = Matrix::translate(-llx, -lly).mul(&rot);
        self.ink.base = (rot == Matrix::IDENTITY).then_some(ctm);

        let resources = inherited(doc, page, b"Resources").and_then(|o| match o {
            Object::Dictionary(d) => Some(d),
            _ => None,
        });
        let content = page_content(doc, page, self.stream_limit);
        let gs = GState {
            ctm,
            font: None,
            size: 0.0,
            char_spacing: 0.0,
            word_spacing: 0.0,
            h_scale: 1.0,
            leading: 0.0,
            rise: 0.0,
        };
        self.run(&content, resources, gs, 0)?;
        Ok(self.ink)
    }

    fn run(
        &mut self,
        content: &[u8],
        resources: Option<&Dictionary>,
        mut gs: GState,
        depth: usize,
    ) -> Result<(), Timeout> {
        let mut stack: Vec<GState> = Vec::new();
        let mut tm = Matrix::IDENTITY;
        let mut tlm = Matrix::IDENTITY;
        let mut lx = Lexer::new(content);
        let mut chars: Vec<DecodedChar> = Vec::new();
        // 当前路径：线段（用户坐标）和矩形；曲线只移动当前点
        let mut segs: Vec<(f64, f64, f64, f64)> = Vec::new();
        let mut rects: Vec<(f64, f64, f64, f64)> = Vec::new();
        let mut cur = (0.0f64, 0.0f64);
        let mut start = (0.0f64, 0.0f64);
        while let Some(op) = lx.next_op() {
            if self.ops_left == 0 {
                break;
            }
            self.ops_left -= 1;
            if self.ops_left.is_multiple_of(512)
                && let Some(d) = self.deadline
                && Instant::now() >= d
            {
                return Err(Timeout);
            }
            let a = &op.args;
            let n = |i: usize| a.get(i).and_then(Operand::num).unwrap_or(0.0);
            match op.name.as_slice() {
                b"q" => {
                    if stack.len() < MAX_STATE_DEPTH {
                        stack.push(gs.clone());
                    }
                }
                b"Q" => {
                    if let Some(s) = stack.pop() {
                        gs = s;
                    }
                }
                b"cm" => {
                    let v: Vec<f64> = a.iter().filter_map(Operand::num).collect();
                    if let Some(m) = Matrix::from_slice(&v) {
                        gs.ctm = m.mul(&gs.ctm);
                    }
                }
                b"BT" => {
                    tm = Matrix::IDENTITY;
                    tlm = Matrix::IDENTITY;
                }
                b"Tf" => {
                    gs.size = n(1);
                    gs.font = match a.first() {
                        Some(Operand::Name(name)) => self.font(resources, name),
                        _ => None,
                    };
                }
                b"Tc" => gs.char_spacing = n(0),
                b"Tw" => gs.word_spacing = n(0),
                b"Tz" => gs.h_scale = n(0) / 100.0,
                b"TL" => gs.leading = n(0),
                b"Ts" => gs.rise = n(0),
                b"Td" => {
                    tlm = Matrix::translate(n(0), n(1)).mul(&tlm);
                    tm = tlm;
                }
                b"TD" => {
                    gs.leading = -n(1);
                    tlm = Matrix::translate(n(0), n(1)).mul(&tlm);
                    tm = tlm;
                }
                b"Tm" => {
                    let v: Vec<f64> = a.iter().filter_map(Operand::num).collect();
                    if let Some(m) = Matrix::from_slice(&v) {
                        tlm = m;
                        tm = m;
                    }
                }
                b"T*" => {
                    tlm = Matrix::translate(0.0, -gs.leading).mul(&tlm);
                    tm = tlm;
                }
                b"Tj" | b"'" | b"\"" => {
                    if op.name == b"\"" {
                        gs.word_spacing = n(0);
                        gs.char_spacing = n(1);
                    }
                    if op.name != b"Tj" {
                        tlm = Matrix::translate(0.0, -gs.leading).mul(&tlm);
                        tm = tlm;
                    }
                    if let Some(Operand::Str(s)) = a.last() {
                        self.show(s, &gs, &mut tm, &mut chars);
                    }
                }
                b"TJ" => {
                    if let Some(Operand::Array(items)) = a.first() {
                        for it in items {
                            match it {
                                Operand::Str(s) => self.show(s, &gs, &mut tm, &mut chars),
                                Operand::Num(k) => {
                                    let tx = -k / 1000.0 * gs.size * gs.h_scale;
                                    tm = Matrix::translate(tx, 0.0).mul(&tm);
                                }
                                _ => {}
                            }
                        }
                    }
                }
                b"m" => {
                    cur = (n(0), n(1));
                    start = cur;
                }
                b"l" => {
                    let p = (n(0), n(1));
                    segs.push((cur.0, cur.1, p.0, p.1));
                    cur = p;
                }
                b"c" => cur = (n(4), n(5)),
                b"v" | b"y" => cur = (n(2), n(3)),
                b"h" => {
                    segs.push((cur.0, cur.1, start.0, start.1));
                    cur = start;
                }
                b"re" => {
                    rects.push((n(0), n(1), n(2), n(3)));
                    cur = (n(0), n(1));
                    start = cur;
                }
                b"S" | b"s" | b"f" | b"F" | b"f*" | b"B" | b"B*" | b"b" | b"b*" => {
                    let fill = !matches!(op.name.as_slice(), b"S" | b"s");
                    self.rules(&segs, &rects, &gs.ctm, fill);
                    segs.clear();
                    rects.clear();
                }
                b"n" => {
                    segs.clear();
                    rects.clear();
                }
                b"BI" => self.image(&gs.ctm),
                b"BMC" | b"BDC" => {
                    let tag = match a.first() {
                        Some(Operand::Name(n)) => n.as_slice(),
                        _ => b"",
                    };
                    let mcid = if depth == 0 { self.mcid(resources, a.get(1)) } else { None };
                    let artifact = tag == b"Artifact" || self.marked.last().is_some_and(|m| m.artifact);
                    if self.marked.len() < MAX_MARKED_DEPTH {
                        self.marked.push(Marked { mcid, artifact });
                    }
                }
                b"EMC" => {
                    self.marked.pop();
                }
                b"Do" => {
                    if let Some(Operand::Name(name)) = a.first() {
                        self.xobject(resources, name, &gs, depth)?;
                    }
                }
                _ => {}
            }
        }
        Ok(())
    }

    /// 记下路径里水平、竖直的线段（换算到页面坐标）。矩形取四条边；填充的细长矩形当作一条线。
    fn rules(
        &mut self,
        segs: &[(f64, f64, f64, f64)],
        rects: &[(f64, f64, f64, f64)],
        ctm: &Matrix,
        fill: bool,
    ) {
        let mut push = |a: (f64, f64), b: (f64, f64)| {
            if self.ink.rules.len() >= MAX_RULES_PER_PAGE {
                return;
            }
            let (dx, dy) = ((b.0 - a.0).abs(), (b.1 - a.1).abs());
            if !(dx.is_finite() && dy.is_finite())
                || (dx > RULE_SKEW && dy > RULE_SKEW)
                || dx.max(dy) < 1.0
            {
                return;
            }
            let r = if dx >= dy {
                Rule {
                    x0: a.0.min(b.0),
                    x1: a.0.max(b.0),
                    y0: (a.1 + b.1) / 2.0,
                    y1: (a.1 + b.1) / 2.0,
                }
            } else {
                Rule {
                    x0: (a.0 + b.0) / 2.0,
                    x1: (a.0 + b.0) / 2.0,
                    y0: a.1.min(b.1),
                    y1: a.1.max(b.1),
                }
            };
            self.ink.rules.push(r);
        };
        for &(x0, y0, x1, y1) in segs {
            push(ctm.apply(x0, y0), ctm.apply(x1, y1));
        }
        for &(x, y, w, h) in rects {
            let p = [
                ctm.apply(x, y),
                ctm.apply(x + w, y),
                ctm.apply(x + w, y + h),
                ctm.apply(x, y + h),
            ];
            let (xs, ys) = (p.map(|q| q.0), p.map(|q| q.1));
            let (minx, maxx) = (
                xs.iter().copied().fold(f64::INFINITY, f64::min),
                xs.iter().copied().fold(f64::NEG_INFINITY, f64::max),
            );
            let (miny, maxy) = (
                ys.iter().copied().fold(f64::INFINITY, f64::min),
                ys.iter().copied().fold(f64::NEG_INFINITY, f64::max),
            );
            let (dw, dh) = (maxx - minx, maxy - miny);
            if fill && dh <= THIN_RECT && dw > dh {
                push((minx, (miny + maxy) / 2.0), (maxx, (miny + maxy) / 2.0));
            } else if fill && dw <= THIN_RECT && dh > dw {
                push(((minx + maxx) / 2.0, miny), ((minx + maxx) / 2.0, maxy));
            } else {
                push((minx, miny), (maxx, miny));
                push((minx, maxy), (maxx, maxy));
                push((minx, miny), (minx, maxy));
                push((maxx, miny), (maxx, maxy));
            }
        }
    }

    /// BDC 的属性：内联字典里的 /MCID，或者指向 /Properties 资源里的属性表。
    fn mcid(&self, resources: Option<&Dictionary>, props: Option<&Operand>) -> Option<i32> {
        let v = match props? {
            d @ Operand::Dict(_) => d.get(b"MCID").and_then(Operand::num)?,
            Operand::Name(name) => {
                let list = get_dict(self.doc, resources?, b"Properties")?;
                let p = get_dict(self.doc, list, name)?;
                crate::obj::get_num(self.doc, p, b"MCID")?
            }
            _ => return None,
        };
        (v >= 0.0 && v <= f64::from(i32::MAX)).then_some(v as i32)
    }

    fn font(&mut self, resources: Option<&Dictionary>, name: &[u8]) -> Option<Rc<Font>> {
        let doc = self.doc;
        let fonts = get_dict(doc, resources?, b"Font")?;
        let raw = fonts.get(name)?;
        if let Object::Reference(id) = raw {
            if let Some(f) = self.fonts.get(id) {
                return Some(f.clone());
            }
            let f = match deref(doc, raw) {
                Object::Dictionary(d) => Rc::new(Font::load(doc, d, self.stream_limit)),
                _ => return None,
            };
            self.fonts.insert(*id, f.clone());
            return Some(f);
        }
        match raw {
            Object::Dictionary(d) => Some(Rc::new(Font::load(doc, d, self.stream_limit))),
            _ => None,
        }
    }

    fn show(&mut self, bytes: &[u8], gs: &GState, tm: &mut Matrix, chars: &mut Vec<DecodedChar>) {
        let font = gs.font.clone().unwrap_or_else(|| self.fallback.clone());
        chars.clear();
        font.decode(bytes, chars);
        for ch in chars.iter() {
            let m = tm.mul(&gs.ctm);
            let [a, b, c, d, e, f] = m.0;
            let x = e + gs.rise * c;
            let y = f + gs.rise * d;
            let size = gs.size.abs() * c.hypot(d) * font.size_factor;
            let glyph_w = ch.width * gs.size * gs.h_scale;
            let horizontal = a > 0.0 && b.abs() <= 0.1 * a.abs();
            if horizontal
                && !ch.text.is_empty()
                && x.is_finite()
                && y.is_finite()
                && size.is_finite()
                && size > 0.1
                && self.ink.glyphs.len() < MAX_GLYPHS_PER_PAGE
                && x > -size
                && x < self.ink.width + size
                && y > -size
                && y < self.ink.height + size
            {
                self.ink.glyphs.push(Glyph {
                    text: ch.text.clone(),
                    x,
                    y,
                    x_end: x + glyph_w * a,
                    size,
                    bold: font.bold,
                    math: font.math,
                    mcid: self.marked.iter().rev().find_map(|m| m.mcid),
                    artifact: self.marked.last().is_some_and(|m| m.artifact),
                });
            }
            let mut tx = ch.width * gs.size + gs.char_spacing;
            if ch.is_space {
                tx += gs.word_spacing;
            }
            *tm = Matrix::translate(tx * gs.h_scale, 0.0).mul(tm);
        }
    }

    fn image(&mut self, ctm: &Matrix) {
        let area = ctm.det().abs();
        if area.is_finite() {
            self.ink.image_count += 1;
            self.ink.image_area += area.min(self.ink.width * self.ink.height);
        }
    }

    fn xobject(
        &mut self,
        resources: Option<&Dictionary>,
        name: &[u8],
        gs: &GState,
        depth: usize,
    ) -> Result<(), Timeout> {
        let doc = self.doc;
        let Some(xobjects) = resources.and_then(|r| get_dict(doc, r, b"XObject")) else {
            return Ok(());
        };
        let Some(Object::Stream(stream)) = get(doc, xobjects, name) else {
            return Ok(());
        };
        match get_name(doc, &stream.dict, b"Subtype") {
            Some(b"Image") => self.image(&gs.ctm),
            Some(b"Form") if depth < MAX_FORM_DEPTH => {
                let Some(content) = doc.stream_data(stream, self.stream_limit) else {
                    return Ok(());
                };
                let matrix = get(doc, &stream.dict, b"Matrix")
                    .map(|o| num_array(doc, o))
                    .and_then(|v| Matrix::from_slice(&v))
                    .unwrap_or(Matrix::IDENTITY);
                let mut inner = gs.clone();
                inner.ctm = matrix.mul(&gs.ctm);
                let form_res = get_dict(doc, &stream.dict, b"Resources").or(resources);
                let marked = self.marked.len();
                let r = self.run(&content, form_res, inner, depth + 1);
                self.marked.truncate(marked);
                r?;
            }
            _ => {}
        }
        Ok(())
    }
}

/// 页面属性可以从父节点继承。
fn inherited<'a>(doc: &'a Document, page: &'a Dictionary, key: &[u8]) -> Option<&'a Object> {
    let mut node = page;
    for _ in 0..64 {
        if let Some(v) = get(doc, node, key) {
            return Some(v);
        }
        node = get_dict(doc, node, b"Parent")?;
    }
    None
}

pub(crate) fn page_content(doc: &Document, page: &Dictionary, limit: usize) -> Vec<u8> {
    let mut out = Vec::new();
    let parts: Vec<&Object> = match get(doc, page, b"Contents") {
        Some(Object::Array(a)) => a.iter().collect(),
        Some(o) => vec![o],
        None => Vec::new(),
    };
    for p in parts {
        let remaining = limit.saturating_sub(out.len());
        if let Some(bytes) = stream_bytes(doc, p, remaining) {
            out.extend_from_slice(&bytes);
            out.push(b'\n');
        }
    }
    out
}
