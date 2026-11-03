//! Office 文档内嵌的媒体文件及其位置：docx、pptx、xlsx 的 `*/media/*` 部件，
//! EPUB 清单里的图片。位置从引用它的地方读：docx 正文的第几段（页眉页脚另记），pptx 的第几张
//! 幻灯片，xlsx 的哪个工作表、锚定在哪个单元格，EPUB 的 spine 里第几个文档。
//! 不解码，只列出；[`media_bytes`] 从 ZIP 里原样取出。和转换（[`crate::convert`]）分开，
//! 不需要图片时不多读一个部件。

use std::collections::HashMap;

use sha2::{Digest, Sha256};

use crate::opc::{self, Rels, resolve};
use crate::xml::{Reader, Tok, local_name};
use crate::zip::Zip;
use crate::{Error, Format, Options, check_deadline};

/// 一个媒体文件。
#[derive(Debug, Clone, PartialEq)]
pub struct Media {
    /// ZIP 里的部件路径，例如 `word/media/image1.png`。
    pub part: String,
    /// 按文件头判断的格式：png、jpeg、gif、bmp、tiff、webp、svg、emf、wmf；认不出时用扩展名。
    pub format: String,
    /// 解压后的字节数。
    pub bytes: u64,
    /// 内容的 SHA-256（小写十六进制）。
    pub hash: String,
    /// 被引用的位置，按文档顺序；没被引用（如封面、残留的旧图）时为空。
    pub locations: Vec<MediaLocation>,
}

/// 媒体在文档里的一处位置。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct MediaLocation {
    /// `body`（docx 正文）、`header`、`footer`、`slide`、`sheet`、`chapter`（EPUB）。
    pub kind: &'static str,
    /// 从 1 开始：正文和页眉页脚是段落序号，幻灯片是张数，工作表和 EPUB 文档是在顺序里的位置。
    pub index: usize,
    /// 工作表名、EPUB 文档路径、页眉页脚部件名。
    pub name: Option<String>,
    /// xlsx 锚定的单元格（`B3`）。
    pub anchor: Option<String>,
}

impl Media {
    /// 导出用的文件名：哈希前 32 位十六进制加扩展名（和 pagesift-pdf 的图片同一个规则）。
    pub fn file_name(&self) -> String {
        let ext = match self.format.as_str() {
            "jpeg" => "jpg",
            "tiff" => "tif",
            f => f,
        };
        format!("{}.{ext}", &self.hash[..self.hash.len().min(32)]) // [协议] 文件签名与 A1 列名（各格式规范）；32 位十六进制 = 128 位哈希前缀
    }
}

/// 列出文档里的媒体文件和位置。CSV 没有媒体，返回空表。
pub fn media(bytes: &[u8], format: Format, opts: &Options<'_>) -> Result<Vec<Media>, Error> {
    if format == Format::Csv {
        return Ok(Vec::new());
    }
    let zip = Zip::open(bytes, &opts.limits)?;
    let mut found = Found::default();
    match format {
        Format::Docx => docx(&zip, &mut found, opts)?,
        Format::Pptx => pptx(&zip, &mut found, opts)?,
        Format::Xlsx => xlsx(&zip, &mut found, opts)?,
        Format::Epub => epub(&zip, &mut found, opts)?,
        Format::Csv => {}
    }
    // 没被引用的媒体部件也列上
    let parts: Vec<String> = zip
        .names()
        .filter(|(n, _)| n.contains("/media/") && !n.ends_with('/'))
        .map(|(n, _)| n.to_string())
        .collect();
    for p in parts {
        found.part(&p);
    }
    let sizes: HashMap<String, u64> = zip.names().map(|(n, s)| (n.to_lowercase(), s)).collect();
    let mut out = Vec::with_capacity(found.order.len());
    for part in found.order {
        check_deadline(opts)?;
        let Some(data) = zip.read(&part)? else {
            continue; // 引用指向不存在的部件
        };
        let locations = found.locations.remove(&part).unwrap_or_default();
        out.push(Media {
            format: sniff(&data, &part),
            bytes: sizes
                .get(&part.to_lowercase())
                .copied()
                .unwrap_or(data.len() as u64),
            hash: hex(&Sha256::digest(&data)),
            part,
            locations,
        });
    }
    Ok(out)
}

/// 原样取出一个媒体部件（[`Media::part`]）。没有这个部件时返回 `None`。
pub fn media_bytes(bytes: &[u8], part: &str, opts: &Options<'_>) -> Result<Option<Vec<u8>>, Error> {
    Zip::open(bytes, &opts.limits)?.read(part)
}

#[derive(Default)]
struct Found {
    order: Vec<String>,
    locations: HashMap<String, Vec<MediaLocation>>,
}

impl Found {
    fn part(&mut self, part: &str) {
        if !self.locations.contains_key(part) {
            self.order.push(part.to_string());
            self.locations.insert(part.to_string(), Vec::new());
        }
    }

    fn at(&mut self, part: &str, loc: MediaLocation) {
        self.part(part);
        if let Some(v) = self.locations.get_mut(part) {
            v.push(loc);
        }
    }
}

/// 在一个 XML 部件里找图片引用：DrawingML 的 `a:blip r:embed`、VML 的 `v:imagedata r:id`。
/// 回调参数是关系目标和引用前见过的段落数（`w:p` 开始标签数）。
fn scan_blips(
    xml: &str,
    rels: &Rels,
    mut hit: impl FnMut(&str, usize, &Option<String>),
) -> Result<(), Error> {
    let mut r = Reader::new(xml);
    let mut paras = 0;
    // xlsx 绘图的锚点：xdr:from 里的 col、row
    let mut anchor: Option<String> = None;
    let mut col: Option<u32> = None;
    let mut in_from = false;
    while let Some(t) = r.next()? {
        let Tok::Start(name, a) = t else {
            if let Tok::End(name) = t
                && local_name(name) == "from"
            {
                in_from = false;
            }
            continue;
        };
        match local_name(name) {
            "p" => paras += 1,
            "from" => in_from = true,
            "col" if in_from => col = r.text_until_end()?.trim().parse().ok(),
            "row" if in_from => {
                let row: Option<u32> = r.text_until_end()?.trim().parse().ok();
                if let (Some(c), Some(rw)) = (col, row) {
                    anchor = Some(format!("{}{}", column_name(c), rw + 1));
                }
            }
            "blip" | "imagedata" => {
                let id = a.get("embed").or_else(|| a.get("id"));
                if let Some(rel) = id.and_then(|id| rels.get(&id))
                    && !rel.external
                {
                    hit(&rel.target, paras.max(1), &anchor);
                }
            }
            _ => {}
        }
    }
    Ok(())
}

/// 从 0 开始的列号转成 A1 写法的列名（A、Z、AA…）。
fn column_name(mut c: u32) -> String {
    let mut s = Vec::new();
    loop {
        // [协议] A1 写法：26 进制、没有零（ECMA-376 第 1 部分 §18.17.2.3）
        s.push(b'A' + (c % 26) as u8);
        // [协议] 文件签名、A1 列名的 26 进制（各格式规范）
        if c < 26 {
            break;
        }
        c = c / 26 - 1; // [协议] 文件签名与 A1 列名（各格式规范）；32 位十六进制 = 128 位哈希前缀
    }
    s.reverse();
    String::from_utf8(s).unwrap_or_default()
}

fn docx(zip: &Zip<'_>, found: &mut Found, opts: &Options<'_>) -> Result<(), Error> {
    let main = opc::main_part(zip, "word/document.xml")?;
    let rels = Rels::of(zip, &main)?;
    if let Some(xml) = zip.read_text(&main)? {
        scan_blips(&xml, &rels, |t, p, _| {
            found.at(t, loc("body", p, None, None))
        })?;
    }
    for kind in ["header", "footer"] {
        for rel in rels.all(kind) {
            check_deadline(opts)?;
            let part = rel.target.clone();
            let Some(xml) = zip.read_text(&part)? else {
                continue;
            };
            let prels = Rels::of(zip, &part)?;
            scan_blips(&xml, &prels, |t, p, _| {
                found.at(t, loc(kind, p, Some(part.clone()), None))
            })?;
        }
    }
    Ok(())
}

fn pptx(zip: &Zip<'_>, found: &mut Found, opts: &Options<'_>) -> Result<(), Error> {
    let main = opc::main_part(zip, "ppt/presentation.xml")?;
    let Some(xml) = zip.read_text(&main)? else {
        return Ok(());
    };
    let rels = Rels::of(zip, &main)?;
    let mut r = Reader::new(&xml);
    let mut n = 0;
    while let Some(t) = r.next()? {
        // 和转换一样按 sldIdLst 的顺序；要的是关系 @r:id，不是数字 @id
        let Tok::Start(name, a) = t else { continue };
        if local_name(name) != "sldId" {
            continue;
        }
        let Some(rid) = a.iter().find(|(k, _)| k.ends_with(":id")).map(|(_, v)| v) else {
            continue;
        };
        let Some(part) = rels.get(&rid).map(|r| r.target.clone()) else {
            continue;
        };
        check_deadline(opts)?;
        let Some(sx) = zip.read_text(&part)? else {
            continue;
        };
        n += 1;
        let srels = Rels::of(zip, &part)?;
        scan_blips(&sx, &srels, |t, _, _| {
            found.at(t, loc("slide", n, None, None))
        })?;
    }
    Ok(())
}

fn xlsx(zip: &Zip<'_>, found: &mut Found, opts: &Options<'_>) -> Result<(), Error> {
    let main = opc::main_part(zip, "xl/workbook.xml")?;
    let Some(xml) = zip.read_text(&main)? else {
        return Ok(());
    };
    let rels = Rels::of(zip, &main)?;
    let (sheets, _) = crate::xlsx::workbook(&xml)?;
    for (i, (name, rid)) in sheets.iter().enumerate() {
        check_deadline(opts)?;
        let Some(sheet) = rels.get(rid).map(|r| r.target.clone()) else {
            continue;
        };
        let srels = Rels::of(zip, &sheet)?;
        for d in srels.all("drawing") {
            let Some(dx) = zip.read_text(&d.target)? else {
                continue;
            };
            let drels = Rels::of(zip, &d.target)?;
            scan_blips(&dx, &drels, |t, _, anchor| {
                found.at(t, loc("sheet", i + 1, Some(name.clone()), anchor.clone()))
            })?;
        }
    }
    Ok(())
}

fn epub(zip: &Zip<'_>, found: &mut Found, opts: &Options<'_>) -> Result<(), Error> {
    let Some(container) = zip.read_text("META-INF/container.xml")? else {
        return Ok(());
    };
    let opf_path = crate::epub::rootfile(&container)?;
    let Some(opf) = zip.read_text(&opf_path)? else {
        return Ok(());
    };
    let base = opf_path.rsplit_once('/').map_or("", |(d, _)| d);
    let pkg = crate::epub::package(&opf)?;
    let mut n = 0;
    for idref in &pkg.spine {
        let Some((_, href, _)) = pkg.manifest.iter().find(|(id, _, _)| id == idref) else {
            continue;
        };
        check_deadline(opts)?;
        let path = resolve(base, href);
        let Some(doc) = zip.read_text(&path)? else {
            continue;
        };
        n += 1;
        let dir = path.rsplit_once('/').map_or("", |(d, _)| d);
        let mut r = Reader::new(&doc);
        while let Some(t) = r.next()? {
            let Tok::Start(name, a) = t else { continue };
            let src = match local_name(name) {
                "img" => a.get("src"),
                "image" => a.get("href"),
                _ => None,
            };
            if let Some(src) = src
                && !src.contains(':')
            {
                found.at(
                    &resolve(dir, &src),
                    loc("chapter", n, Some(path.clone()), None),
                );
            }
        }
    }
    // 清单里的图片（封面等）即使没被引用也列上
    for (_, href, media) in &pkg.manifest {
        if media.starts_with("image/") {
            found.part(&resolve(base, href));
        }
    }
    Ok(())
}

fn loc(
    kind: &'static str,
    index: usize,
    name: Option<String>,
    anchor: Option<String>,
) -> MediaLocation {
    MediaLocation {
        kind,
        index,
        name,
        anchor,
    }
}

/// 按文件头认格式（各格式规范的签名），认不出用扩展名。
fn sniff(data: &[u8], part: &str) -> String {
    // [协议] 文件签名：PNG（ISO/IEC 15948 §5.2）、JPEG SOI（T.81）、GIF87a/89a、BMP、TIFF II/MM、
    // RIFF…WEBP、EMF（记录类型 1、偏移 40 处 " EMF"）、WMF 可放置头 D7CDC69A
    let f = if data.starts_with(&[0x89, b'P', b'N', b'G']) {
        "png"
        // [协议] 文件签名、A1 列名的 26 进制（各格式规范）
    } else if data.starts_with(&[0xFF, 0xD8, 0xFF]) {
        "jpeg"
    } else if data.starts_with(b"GIF8") {
        "gif"
    } else if data.starts_with(b"BM") {
        "bmp"
    } else if data.starts_with(b"II*\0") || data.starts_with(b"MM\0*") {
        "tiff"
        // [协议] 文件签名、A1 列名的 26 进制（各格式规范）
    } else if data.starts_with(b"RIFF") && data.get(8..12) == Some(b"WEBP") {
        "webp"
        // [协议] 文件签名、A1 列名的 26 进制（各格式规范）
    } else if data.get(40..44) == Some(b" EMF") {
        "emf"
        // [协议] 文件签名、A1 列名的 26 进制（各格式规范）
    } else if data.starts_with(&[0xD7, 0xCD, 0xC6, 0x9A]) {
        "wmf"
    } else if part.to_ascii_lowercase().ends_with(".svg") {
        "svg"
    } else {
        return part
            .rsplit_once('.')
            .map_or("bin".to_string(), |(_, e)| e.to_ascii_lowercase());
    };
    f.to_string()
}

fn hex(bytes: &[u8]) -> String {
    bytes.iter().map(|b| format!("{b:02x}")).collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::zip::tests::build;

    const PNG: &[u8] = &[0x89, b'P', b'N', b'G', 0x0D, 0x0A, 0x1A, 0x0A, 1, 2];
    const JPG: &[u8] = &[0xFF, 0xD8, 0xFF, 0xE0, 9, 9];

    #[test]
    fn docx_media_with_paragraph_positions() {
        let doc = r#"<w:document><w:body><w:p/><w:p><w:r><w:drawing><a:blip r:embed="i1"/></w:drawing></w:r></w:p><w:p><v:imagedata r:id="i2"/></w:p></w:body></w:document>"#;
        let rels = r#"<Relationships><Relationship Id="i1" Type="x/image" Target="media/a.png"/><Relationship Id="i2" Type="x/image" Target="media/b.jpeg"/></Relationships>"#;
        let z = build(&[
            ("word/document.xml", doc.as_bytes(), true),
            ("word/_rels/document.xml.rels", rels.as_bytes(), false),
            ("word/media/a.png", PNG, false),
            ("word/media/b.jpeg", JPG, true),
            ("word/media/unused.png", PNG, false),
        ]);
        let m = media(&z, Format::Docx, &Options::default()).unwrap();
        let got: Vec<(&str, &str, Vec<usize>)> = m
            .iter()
            .map(|x| {
                (
                    x.part.as_str(),
                    x.format.as_str(),
                    x.locations.iter().map(|l| l.index).collect(),
                )
            })
            .collect();
        assert_eq!(
            got,
            [
                ("word/media/a.png", "png", vec![2]),
                ("word/media/b.jpeg", "jpeg", vec![3]),
                ("word/media/unused.png", "png", vec![]),
            ]
        );
        assert_eq!(m[0].bytes, PNG.len() as u64);
        assert_eq!(m[0].hash, m[2].hash);
        assert!(m[1].file_name().ends_with(".jpg"));
        assert_eq!(
            media_bytes(&z, "word/media/b.jpeg", &Options::default()).unwrap(),
            Some(JPG.to_vec())
        );
    }

    #[test]
    fn pptx_slides_and_xlsx_anchors() {
        let pres = r#"<p:presentation><p:sldIdLst><p:sldId id="256" r:id="s2"/><p:sldId id="257" r:id="s1"/></p:sldIdLst></p:presentation>"#;
        let prels = r#"<Relationships><Relationship Id="s1" Type="x/slide" Target="slides/slide1.xml"/><Relationship Id="s2" Type="x/slide" Target="slides/slide2.xml"/></Relationships>"#;
        let slide =
            r#"<p:sld><p:pic><p:blipFill><a:blip r:embed="i"/></p:blipFill></p:pic></p:sld>"#;
        let srels = r#"<Relationships><Relationship Id="i" Type="x/image" Target="../media/logo.png"/></Relationships>"#;
        let z = build(&[
            ("ppt/presentation.xml", pres.as_bytes(), false),
            ("ppt/_rels/presentation.xml.rels", prels.as_bytes(), false),
            ("ppt/slides/slide1.xml", slide.as_bytes(), false),
            ("ppt/slides/slide2.xml", b"<p:sld/>", false),
            ("ppt/slides/_rels/slide1.xml.rels", srels.as_bytes(), false),
            ("ppt/media/logo.png", PNG, false),
        ]);
        let m = media(&z, Format::Pptx, &Options::default()).unwrap();
        // slide1 在 sldIdLst 里排第二（编号和转换输出的 `<!-- Slide N -->` 一致）
        assert_eq!(m[0].locations[0].kind, "slide");
        assert_eq!(m[0].locations[0].index, 2);

        let wb =
            r#"<workbook><sheets><sheet name="Data" sheetId="1" r:id="w1"/></sheets></workbook>"#;
        let wrels = r#"<Relationships><Relationship Id="w1" Type="x/worksheet" Target="worksheets/sheet1.xml"/></Relationships>"#;
        let srels = r#"<Relationships><Relationship Id="d" Type="x/drawing" Target="../drawings/drawing1.xml"/></Relationships>"#;
        let drawing = r#"<xdr:wsDr><xdr:twoCellAnchor><xdr:from><xdr:col>27</xdr:col><xdr:row>2</xdr:row></xdr:from><xdr:pic><xdr:blipFill><a:blip r:embed="p"/></xdr:blipFill></xdr:pic></xdr:twoCellAnchor></xdr:wsDr>"#;
        let drels = r#"<Relationships><Relationship Id="p" Type="x/image" Target="../media/chart.png"/></Relationships>"#;
        let z = build(&[
            ("xl/workbook.xml", wb.as_bytes(), false),
            ("xl/_rels/workbook.xml.rels", wrels.as_bytes(), false),
            ("xl/worksheets/sheet1.xml", b"<worksheet/>", false),
            (
                "xl/worksheets/_rels/sheet1.xml.rels",
                srels.as_bytes(),
                false,
            ),
            ("xl/drawings/drawing1.xml", drawing.as_bytes(), false),
            (
                "xl/drawings/_rels/drawing1.xml.rels",
                drels.as_bytes(),
                false,
            ),
            ("xl/media/chart.png", PNG, false),
        ]);
        let m = media(&z, Format::Xlsx, &Options::default()).unwrap();
        let l = &m[0].locations[0];
        assert_eq!((l.kind, l.index), ("sheet", 1));
        assert_eq!(l.name.as_deref(), Some("Data"));
        assert_eq!(l.anchor.as_deref(), Some("AB3"));
    }

    #[test]
    fn epub_images_by_chapter() {
        let container = r#"<container><rootfiles><rootfile full-path="OEBPS/content.opf"/></rootfiles></container>"#;
        let opf = r#"<package><manifest><item id="c1" href="text/ch1.xhtml" media-type="application/xhtml+xml"/><item id="img" href="img/a.png" media-type="image/png"/><item id="cover" href="img/cover.jpg" media-type="image/jpeg"/></manifest><spine><itemref idref="c1"/></spine></package>"#;
        let ch = r#"<html><body><p>x</p><img src="../img/a.png"/><img src="http://example.com/x.png"/></body></html>"#;
        let z = build(&[
            ("META-INF/container.xml", container.as_bytes(), false),
            ("OEBPS/content.opf", opf.as_bytes(), false),
            ("OEBPS/text/ch1.xhtml", ch.as_bytes(), false),
            ("OEBPS/img/a.png", PNG, false),
            ("OEBPS/img/cover.jpg", JPG, false),
        ]);
        let m = media(&z, Format::Epub, &Options::default()).unwrap();
        assert_eq!(m.len(), 2);
        assert_eq!(m[0].part, "OEBPS/img/a.png");
        assert_eq!(
            m[0].locations[0].name.as_deref(),
            Some("OEBPS/text/ch1.xhtml")
        );
        assert!(m[1].locations.is_empty());
    }

    #[test]
    fn column_names() {
        assert_eq!(column_name(0), "A");
        assert_eq!(column_name(25), "Z");
        assert_eq!(column_name(26), "AA");
        assert_eq!(column_name(701), "ZZ");
        assert_eq!(column_name(702), "AAA");
    }
}
