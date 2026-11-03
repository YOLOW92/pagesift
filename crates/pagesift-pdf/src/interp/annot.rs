//! 注释外观与表单域：外观流当表单 XObject 画在 /Rect 上，没有外观的域把值写进框里。

use super::*;

impl Interp<'_> {
    /// 注释的外观（ISO 32000-2 §12.5.5）：表单域的值、图章、文本框的字都画在注释的外观流里，
    /// 不在页面内容流里。和阅读器显示的一样，把每个可见注释的常规外观（/AP /N，多状态的按 /AS 取）
    /// 当表单 XObject 画在它的 /Rect 上（算法 8.1：外观的 BBox 经 /Matrix 变换后映射到 /Rect）。
    /// 只收字（见 `text_only`）。隐藏的（/F 的 Hidden、NoView 位）、链接、弹出框和按钮
    /// （“提交”“重置”这类界面上的字，不是文档内容）不画。
    pub(super) fn annotations(&mut self, page: &Dictionary, gs: GState) -> Result<(), Timeout> {
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
            if matches!(get_name(doc, a, b"Subtype"), Some(b"Link" | b"Popup")) {
                continue;
            }
            if push_button(doc, a) {
                self.ink.evidence.buttons += 1;
                if !self.alt {
                    continue;
                }
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

/// 没有外观流的表单域要显示的文字：文本域和选择域的值（/V，可以从父域继承）、按钮的标题。
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
        // 按钮（只在复核路径上画，见 quality.rs）：外观特征字典里的标题 /MK /CA（§12.5.6.19）
        Some(b"Btn") if push_button(doc, widget) => get_dict(doc, widget, b"MK")
            .and_then(|mk| get(doc, mk, b"CA"))
            .and_then(string)?,
        _ => return None,
    };
    let text = text.trim().to_string();
    (!text.is_empty()).then_some(text)
}
