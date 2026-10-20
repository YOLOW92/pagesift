//! 测试用 PDF 生成器：用 lopdf 写出带指定内容流的文档，不依赖任何外部 PDF 文件。

#![allow(dead_code)]

use std::collections::BTreeMap;

use lopdf::{Dictionary, Document, Object, ObjectId, Stream, StringFormat, dictionary};

/// 页面内容：原始内容流，加上是否挂一张图像 XObject。
pub struct Page {
    pub content: String,
    pub image: bool,
}

impl Page {
    pub fn text(content: String) -> Page {
        Page {
            content,
            image: false,
        }
    }
}

/// 中文/日文用的 CID 字体：字符到 CID 的分配，生成 ToUnicode。
#[derive(Default)]
pub struct CjkFont {
    cids: BTreeMap<char, u16>,
}

impl CjkFont {
    /// 把文本编码成 Identity-H 的十六进制串。
    pub fn hex(&mut self, text: &str) -> String {
        let mut s = String::from("<");
        for c in text.chars() {
            let next = self.cids.len() as u16 + 1;
            let cid = *self.cids.entry(c).or_insert(next);
            s.push_str(&format!("{cid:04X}"));
        }
        s.push('>');
        s
    }

    fn to_unicode(&self) -> Vec<u8> {
        let mut s = String::from(
            "/CIDInit /ProcSet findresource begin\n12 dict begin\nbegincmap\n\
             /CMapName /Adobe-Identity-UCS def\n/CMapType 2 def\n\
             1 begincodespacerange\n<0000> <FFFF>\nendcodespacerange\n",
        );
        s.push_str(&format!("{} beginbfchar\n", self.cids.len()));
        for (c, cid) in &self.cids {
            let mut buf = [0u16; 2];
            let units: String = c
                .encode_utf16(&mut buf)
                .iter()
                .map(|u| format!("{u:04X}"))
                .collect();
            s.push_str(&format!("<{cid:04X}> <{units}>\n"));
        }
        s.push_str("endbfchar\nendcmap\nCMapName currentdict /CMap defineresource pop\nend\nend\n");
        s.into_bytes()
    }
}

pub struct Builder {
    pub pages: Vec<Page>,
    pub cjk: Option<CjkFont>,
    pub info: Option<Dictionary>,
    /// 额外的字体资源（名字 → 字体字典）。
    pub extra_fonts: Vec<(String, Dictionary)>,
}

impl Builder {
    pub fn new(pages: Vec<Page>) -> Builder {
        Builder {
            pages,
            cjk: None,
            info: None,
            extra_fonts: Vec::new(),
        }
    }

    pub fn document(self) -> Document {
        let mut doc = Document::with_version("1.5");
        let pages_id = doc.new_object_id();
        let f1 = doc.add_object(dictionary! {
            "Type" => "Font", "Subtype" => "Type1",
            "BaseFont" => "Helvetica", "Encoding" => "WinAnsiEncoding",
        });
        let f2 = doc.add_object(dictionary! {
            "Type" => "Font", "Subtype" => "Type1",
            "BaseFont" => "Helvetica-Bold", "Encoding" => "WinAnsiEncoding",
        });
        let mut fonts = dictionary! { "F1" => f1, "F2" => f2 };
        if let Some(cjk) = &self.cjk {
            let tu = doc.add_object(Stream::new(dictionary! {}, cjk.to_unicode()));
            let descendant = doc.add_object(dictionary! {
                "Type" => "Font", "Subtype" => "CIDFontType2",
                "BaseFont" => "NotoSansCJK-Regular",
                "CIDSystemInfo" => dictionary! {
                    "Registry" => Object::string_literal("Adobe"),
                    "Ordering" => Object::string_literal("Identity"),
                    "Supplement" => 0,
                },
                "DW" => 1000,
            });
            let f3 = doc.add_object(dictionary! {
                "Type" => "Font", "Subtype" => "Type0",
                "BaseFont" => "NotoSansCJK-Regular",
                "Encoding" => "Identity-H",
                "DescendantFonts" => vec![descendant.into()],
                "ToUnicode" => tu,
            });
            fonts.set("F3", f3);
        }
        for (name, dict) in self.extra_fonts {
            let id = doc.add_object(dict);
            fonts.set(name.into_bytes(), id);
        }
        let image = doc.add_object(Stream::new(
            dictionary! {
                "Type" => "XObject", "Subtype" => "Image",
                "Width" => 2, "Height" => 2, "ColorSpace" => "DeviceGray",
                "BitsPerComponent" => 8,
            },
            vec![0, 255, 255, 0],
        ));
        let resources = doc.add_object(dictionary! {
            "Font" => fonts,
            "XObject" => dictionary! { "Im1" => image },
        });
        let mut kids: Vec<Object> = Vec::new();
        for page in self.pages {
            let mut content = String::new();
            if page.image {
                content.push_str("q 612 0 0 792 0 0 cm /Im1 Do Q\n");
            }
            content.push_str(&page.content);
            let cid = doc.add_object(Stream::new(dictionary! {}, content.into_bytes()));
            let pid: ObjectId = doc.add_object(dictionary! {
                "Type" => "Page", "Parent" => pages_id, "Contents" => cid,
            });
            kids.push(pid.into());
        }
        let count = kids.len() as i64;
        doc.objects.insert(
            pages_id,
            Object::Dictionary(dictionary! {
                "Type" => "Pages", "Kids" => kids, "Count" => count,
                "Resources" => resources,
                "MediaBox" => vec![0.into(), 0.into(), 612.into(), 792.into()],
            }),
        );
        let catalog = doc.add_object(dictionary! { "Type" => "Catalog", "Pages" => pages_id });
        doc.trailer.set("Root", catalog);
        if let Some(info) = self.info {
            let id = doc.add_object(info);
            doc.trailer.set("Info", id);
        }
        doc.trailer.set(
            "ID",
            vec![
                Object::String(b"0123456789abcdef".to_vec(), StringFormat::Hexadecimal),
                Object::String(b"0123456789abcdef".to_vec(), StringFormat::Hexadecimal),
            ],
        );
        doc
    }

    pub fn build(self) -> Vec<u8> {
        save(self.document())
    }
}

pub fn save(mut doc: Document) -> Vec<u8> {
    doc.compress();
    save_raw(doc)
}

/// 不压缩，变异测试时字节翻转能落到对象结构上。
pub fn save_raw(mut doc: Document) -> Vec<u8> {
    let mut out = Vec::new();
    doc.save_to(&mut out).expect("save pdf");
    out
}

/// PDF 字面串转义。
pub fn lit(s: &str) -> String {
    let mut o = String::from("(");
    for c in s.chars() {
        match c {
            '(' | ')' | '\\' => {
                o.push('\\');
                o.push(c);
            }
            _ => o.push(c),
        }
    }
    o.push(')');
    o
}

/// 在 (x, y) 处用 F1/F2 写一行。
pub fn line(font: &str, size: f64, x: f64, y: f64, text: &str) -> String {
    format!(
        "BT /{font} {size} Tf 1 0 0 1 {x} {y} Tm {} Tj ET\n",
        lit(text)
    )
}

/// 把文字按每行最多 `width` 个字符折行。
pub fn wrap(text: &str, width: usize) -> Vec<String> {
    let mut lines = Vec::new();
    let mut cur = String::new();
    for w in text.split_whitespace() {
        if !cur.is_empty() && cur.len() + 1 + w.len() > width {
            lines.push(std::mem::take(&mut cur));
        }
        if !cur.is_empty() {
            cur.push(' ');
        }
        cur.push_str(w);
    }
    if !cur.is_empty() {
        lines.push(cur);
    }
    lines
}

/// 一段正文：从 (x, y) 开始按 `leading` 往下排，返回内容流和下一行的 y。
pub fn paragraph(
    size: f64,
    x: f64,
    mut y: f64,
    leading: f64,
    width: usize,
    text: &str,
) -> (String, f64) {
    let mut s = String::new();
    for l in wrap(text, width) {
        s.push_str(&line("F1", size, x, y, &l));
        y -= leading;
    }
    (s, y)
}
