//! 内容流解释：跟踪 CTM 和文本矩阵，算出每个字形的位置和字号，统计图像覆盖面积。

use std::collections::HashMap;
use std::rc::Rc;
use std::time::Instant;

use crate::file::{Dictionary, Document, Object, ObjectId};
use crate::short::Short;

use crate::font::{DecodedChar, Font};
pub(crate) use crate::geom::{Glyph, Matrix, PageInk, Rule, Shape};
use crate::lexer::{Lexer, Operand};
use crate::obj::{deref, get, get_dict, get_name, num_array, stream_bytes};

mod paint;
use paint::{EMPTY_BOX, Paint, grow};

/// 一条线段最多可以偏离水平或竖直多少（页面单位）。
const RULE_SKEW: f64 = 1.0;
/// 填充的矩形短边不超过这个宽度时当作一条线（很多 PDF 用细长矩形画表格线）。
const THIN_RECT: f64 = 2.5;
const MAX_RULES_PER_PAGE: usize = 20_000;
const MAX_SHAPES_PER_PAGE: usize = 20_000;

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
    fill: Paint,
    stroke: Paint,
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
    /// 正在画注释的外观：只收字。表单域的边框、底色不是页面的表格线和图形，收了会改变版面判断。
    text_only: bool,
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
/// 一页最多画这么多个注释的外观（表单很多的页面有几百个域）。
const MAX_ANNOTATIONS: usize = 2_000;
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
            text_only: false,
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
            fill: Paint::BLACK,
            stroke: Paint::BLACK,
        };
        self.run(&content, resources, gs.clone(), 0)?;
        self.annotations(page, gs)?;
        Ok(self.ink)
    }

    /// 注释的外观（ISO 32000-2 §12.5.5）：表单域的值、图章、文本框的字都画在注释的外观流里，
    /// 不在页面内容流里。和阅读器显示的一样，把每个可见注释的常规外观（/AP /N，多状态的按 /AS 取）
    /// 当表单 XObject 画在它的 /Rect 上（算法 8.1：外观的 BBox 经 /Matrix 变换后映射到 /Rect）。
    /// 只收字（见 `text_only`）。隐藏的（/F 的 Hidden、NoView 位）、链接、弹出框和按钮
    /// （“提交”“重置”这类界面上的字，不是文档内容）不画。
    fn annotations(&mut self, page: &Dictionary, gs: GState) -> Result<(), Timeout> {
        const HIDDEN: i64 = 1 << 1;
        const NO_VIEW: i64 = 1 << 5;
        let doc = self.doc;
        let annots: Vec<&Object> = match get(doc, page, b"Annots") {
            Some(Object::Array(a)) => a.iter().take(MAX_ANNOTATIONS).collect(),
            _ => return Ok(()),
        };
        for annot in annots {
            let Object::Dictionary(a) = deref(doc, annot) else {
                continue;
            };
            if matches!(get_name(doc, a, b"Subtype"), Some(b"Link" | b"Popup"))
                || push_button(doc, a)
            {
                continue;
            }
            let flags = get(doc, a, b"F").and_then(crate::obj::num).unwrap_or(0.0) as i64;
            if flags & (HIDDEN | NO_VIEW) != 0 {
                continue;
            }
            let rect = get(doc, a, b"Rect")
                .map(|o| num_array(doc, o))
                .unwrap_or_default();
            let [rx0, ry0, rx1, ry1] = match rect.as_slice() {
                &[x0, y0, x1, y1] => [x0.min(x1), y0.min(y1), x0.max(x1), y0.max(y1)],
                _ => continue,
            };
            let normal = get_dict(doc, a, b"AP").and_then(|ap| match get(doc, ap, b"N") {
                Some(Object::Stream(s)) => Some(s),
                Some(Object::Dictionary(states)) => {
                    match get(doc, states, get_name(doc, a, b"AS")?) {
                        Some(Object::Stream(s)) => Some(s),
                        _ => None,
                    }
                }
                _ => None,
            });
            let Some(stream) = normal else {
                // 没有外观的表单域（/NeedAppearances，由阅读器生成外观）：把域的值写在它的框里
                if let Some(text) = field_text(doc, a) {
                    self.field(&text, a, [rx0, ry0, rx1, ry1], &gs);
                }
                continue;
            };
            let Some(content) = doc.stream_data(stream, self.stream_limit) else {
                continue;
            };
            let matrix = get(doc, &stream.dict, b"Matrix")
                .map(|o| num_array(doc, o))
                .and_then(|v| Matrix::from_slice(&v))
                .unwrap_or(Matrix::IDENTITY);
            let bbox = get(doc, &stream.dict, b"BBox")
                .map(|o| num_array(doc, o))
                .filter(|v| v.len() == 4)
                .unwrap_or_else(|| vec![rx0, ry0, rx1, ry1]);
            let corners = [
                matrix.apply(bbox[0], bbox[1]),
                matrix.apply(bbox[2], bbox[1]),
                matrix.apply(bbox[2], bbox[3]),
                matrix.apply(bbox[0], bbox[3]),
            ];
            let (bx0, bx1) = corners
                .iter()
                .fold((f64::INFINITY, f64::NEG_INFINITY), |(lo, hi), p| {
                    (lo.min(p.0), hi.max(p.0))
                });
            let (by0, by1) = corners
                .iter()
                .fold((f64::INFINITY, f64::NEG_INFINITY), |(lo, hi), p| {
                    (lo.min(p.1), hi.max(p.1))
                });
            let (bw, bh) = (bx1 - bx0, by1 - by0);
            if !(bw.is_finite() && bh.is_finite()) || bw <= 0.0 || bh <= 0.0 {
                continue;
            }
            let fit = Matrix::translate(-bx0, -by0)
                .mul(&Matrix([
                    (rx1 - rx0) / bw,
                    0.0,
                    0.0,
                    (ry1 - ry0) / bh,
                    0.0,
                    0.0,
                ]))
                .mul(&Matrix::translate(rx0, ry0));
            let mut inner = gs.clone();
            inner.ctm = matrix.mul(&fit).mul(&gs.ctm);
            let res = get_dict(doc, &stream.dict, b"Resources");
            let marked = self.marked.len();
            self.text_only = true;
            let r = self.run(&content, res, inner, 1);
            self.text_only = false;
            self.marked.truncate(marked);
            r?;
        }
        Ok(())
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
        // 当前路径里的曲线段数和外框（用户坐标，含控制点）
        let mut curves = 0u32;
        let mut pbox = EMPTY_BOX;
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
                    grow(&mut pbox, cur);
                }
                b"l" => {
                    let p = (n(0), n(1));
                    segs.push((cur.0, cur.1, p.0, p.1));
                    cur = p;
                    grow(&mut pbox, cur);
                }
                b"c" | b"v" | b"y" => {
                    let k = a.len().min(6) / 2;
                    for i in 0..k {
                        grow(&mut pbox, (n(2 * i), n(2 * i + 1)));
                    }
                    cur = (n(2 * k.max(1) - 2), n(2 * k.max(1) - 1));
                    curves += 1;
                }
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
                    if !self.text_only {
                        self.rules(&segs, &rects, &gs.ctm, fill);
                        self.shapes(&segs, &rects, curves, pbox, &gs, fill);
                    }
                    segs.clear();
                    rects.clear();
                    curves = 0;
                    pbox = EMPTY_BOX;
                }
                b"n" => {
                    segs.clear();
                    rects.clear();
                    curves = 0;
                    pbox = EMPTY_BOX;
                }
                b"g" | b"G" | b"rg" | b"RG" | b"k" | b"K" | b"cs" | b"CS" | b"sc" | b"scn"
                | b"SC" | b"SCN" => {
                    let (mut f, mut st) = (gs.fill, gs.stroke);
                    self.color(&op.name, a, resources, &mut f, &mut st);
                    (gs.fill, gs.stroke) = (f, st);
                }
                b"BI" => self.image(&gs.ctm),
                b"BMC" | b"BDC" => {
                    let tag = match a.first() {
                        Some(Operand::Name(n)) => n.as_slice(),
                        _ => b"",
                    };
                    let own = self.mcid(resources, a.get(1));
                    let mcid = if depth == 0 { own } else { None };
                    // 带 MCID 的 Artifact 自相矛盾（MCID 是给结构树里的正文用的，§14.8.2.2），按正文对待
                    let artifact = (tag == b"Artifact" && own.is_none())
                        || self.marked.last().is_some_and(|m| m.artifact);
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

    /// 没有外观流的表单域：按 /DA 的字号（0 或缺省时按框高）从框的左上角起一行一行写域的值，
    /// 字宽按半个字号估、不超出框（只为排版时分词，不为还原外观）。
    fn field(&mut self, text: &str, annot: &Dictionary, rect: [f64; 4], gs: &GState) {
        let [x0, y0, x1, y1] = rect;
        let da = match get(self.doc, annot, b"DA") {
            Some(Object::String(s, _)) => String::from_utf8_lossy(s).into_owned(),
            _ => String::new(),
        };
        let words: Vec<&str> = da.split_whitespace().collect();
        let size = words
            .iter()
            .position(|w| *w == "Tf")
            .and_then(|i| words.get(i.checked_sub(1)?)?.parse::<f64>().ok())
            .filter(|s| *s > 0.0)
            .unwrap_or(((y1 - y0) * 0.7).clamp(4.0, 12.0));
        let mut inner = gs.clone();
        inner.font = None;
        inner.size = size;
        let mut chars = Vec::new();
        for (i, line) in text.lines().enumerate() {
            let baseline = y1 - 2.0 - size * (i as f64 + 1.0);
            if baseline < y0 - size {
                break;
            }
            // 估的字宽不让一行超出框（相邻的按钮、域的字不会连成一个词）
            let n = line.chars().count().max(1) as f64;
            let width = 0.5f64.min((x1 - x0 - 4.0).max(size) / (n * size));
            chars.clear();
            chars.extend(line.chars().map(|c| DecodedChar {
                text: Short::from(c),
                width,
                is_space: c == ' ',
            }));
            let mut tm = Matrix::translate(x0 + 2.0, baseline.max(y0 + 1.0));
            self.place(&chars, &self.fallback.clone(), &inner, &mut tm);
        }
    }

    fn show(&mut self, bytes: &[u8], gs: &GState, tm: &mut Matrix, chars: &mut Vec<DecodedChar>) {
        let font = gs.font.clone().unwrap_or_else(|| self.fallback.clone());
        chars.clear();
        font.decode(bytes, chars);
        self.place(chars, &font, gs, tm);
    }

    /// 把解码好的字放到页面上，推进文本矩阵。
    fn place(&mut self, chars: &[DecodedChar], font: &Font, gs: &GState, tm: &mut Matrix) {
        for ch in chars {
            let m = tm.mul(&gs.ctm);
            let [a, b, c, d, e, f] = m.0;
            let x = e + gs.rise * c;
            let y = f + gs.rise * d;
            let size = gs.size.abs() * c.hypot(d) * font.size_factor;
            let glyph_w = ch.width * gs.size * gs.h_scale;
            // 斜着的字（多是水印）不收；转了 90°、180°、270° 的单独存放，排版前由 orient.rs 处理
            let dir = crate::orient::quarter(a, b);
            let stored =
                self.ink.glyphs.len() + self.ink.turned.iter().map(Vec::len).sum::<usize>();
            if let Some(dir) = dir
                && !ch.text.is_empty()
                && x.is_finite()
                && y.is_finite()
                && size.is_finite()
                && size > 0.1
                && stored < MAX_GLYPHS_PER_PAGE
                && x > -size
                && x < self.ink.width + size
                && y > -size
                && y < self.ink.height + size
            {
                let mut glyph = Glyph {
                    text: ch.text.clone(),
                    x,
                    y,
                    x_end: x + glyph_w * a,
                    size,
                    bold: font.bold,
                    math: font.math,
                    mcid: self.marked.iter().rev().find_map(|m| m.mcid),
                    artifact: self.marked.last().is_some_and(|m| m.artifact),
                };
                if dir == 0 {
                    self.ink.glyphs.push(glyph);
                } else {
                    let (u, v) =
                        crate::orient::to_frame(dir, x, y, self.ink.width, self.ink.height);
                    (glyph.x, glyph.y, glyph.x_end) = (u, v, u + glyph_w * a.hypot(b));
                    self.ink.turned[usize::from(dir) - 1].push(glyph);
                }
            }
            let mut tx = ch.width * gs.size + gs.char_spacing;
            if ch.is_space {
                tx += gs.word_spacing;
            }
            *tm = Matrix::translate(tx * gs.h_scale, 0.0).mul(tm);
        }
    }

    fn image(&mut self, ctm: &Matrix) {
        if self.text_only {
            return;
        }
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

/// 表单域的属性可以从父域继承（ISO 32000-2 §12.7.4）。
fn field_attr<'a>(doc: &'a Document, widget: &'a Dictionary, key: &[u8]) -> Option<&'a Object> {
    let mut node = widget;
    for _ in 0..32 {
        if let Some(v) = get(doc, node, key) {
            return Some(v);
        }
        node = get_dict(doc, node, b"Parent")?;
    }
    None
}

/// 按钮域里的按钮（不是复选框、单选框）。
fn push_button(doc: &Document, annot: &Dictionary) -> bool {
    const PUSH_BUTTON: i64 = 1 << 16;
    get_name(doc, annot, b"Subtype") == Some(b"Widget")
        && matches!(field_attr(doc, annot, b"FT").map(|o| deref(doc, o)), Some(Object::Name(n)) if n == b"Btn")
        && field_attr(doc, annot, b"Ff")
            .and_then(crate::obj::num)
            .unwrap_or(0.0) as i64
            & PUSH_BUTTON
            != 0
}

/// 没有外观流的表单域要显示的文字：文本域和选择域的值（/V，可以从父域继承）。
/// 复选框、单选框的值是状态名，不是文字。
fn field_text(doc: &Document, widget: &Dictionary) -> Option<String> {
    if get_name(doc, widget, b"Subtype") != Some(b"Widget") {
        return None;
    }
    let inherited = |key: &[u8]| field_attr(doc, widget, key);
    let string = |o: &Object| match deref(doc, o) {
        Object::String(s, _) => Some(crate::obj::text_string(s)),
        _ => None,
    };
    let text = match inherited(b"FT").and_then(|o| match deref(doc, o) {
        Object::Name(n) => Some(n.as_slice()),
        _ => None,
    }) {
        Some(b"Tx") => inherited(b"V").and_then(string)?,
        Some(b"Ch") => match inherited(b"V").map(|o| deref(doc, o))? {
            Object::Array(items) => items
                .iter()
                .filter_map(string)
                .collect::<Vec<_>>()
                .join("\n"),
            o => string(o)?,
        },
        _ => return None,
    };
    let text = text.trim().to_string();
    (!text.is_empty()).then_some(text)
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
