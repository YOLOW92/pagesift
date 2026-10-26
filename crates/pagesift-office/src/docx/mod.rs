//! Word（WordprocessingML，ECMA-376 第 1 部分第 17 章）转 Markdown。
//!
//! 标题按样式判定（见 [`styles::Styles::heading_level`]），编号段落按 numbering.xml 变成有序/无序列表，
//! 表格展开合并单元格，图片输出替代文字和包内路径，脚注、尾注变成 `[^n]` 引用加文末定义，
//! 公式（OMML）转成 LaTeX。页眉页脚、批注、修订里删除的文字不输出。

mod body;
mod math;
mod styles;
mod symbol;
mod table;

use std::collections::HashMap;

use crate::md::{Block, render};
use crate::opc::{self, Rels};
use crate::xml::{Reader, Tok, local_name};
use crate::zip::Zip;
use crate::{Converted, Error, Options};
use styles::{Numbering, Styles};

pub(crate) struct Ctx<'a> {
    styles: Styles,
    numbering: Numbering,
    rels: Rels,
    default_style: Option<String>,
    opts: &'a Options<'a>,
    /// 引用过的脚注/尾注：(是否尾注, id) → 标签，按第一次引用的顺序。
    notes: Vec<((bool, String), String)>,
}

impl Ctx<'_> {
    /// 脚注引用的标签：脚注 1、2、3…，尾注 e1、e2…，同一条重复引用用同一个标签。
    fn note_label(&mut self, endnote: bool, id: &str) -> String {
        let key = (endnote, id.to_string());
        if let Some((_, l)) = self.notes.iter().find(|(k, _)| *k == key) {
            return l.clone();
        }
        let n = self
            .notes
            .iter()
            .filter(|((e, _), _)| *e == endnote)
            .count()
            + 1;
        let label = if endnote {
            format!("e{n}")
        } else {
            n.to_string()
        };
        self.notes.push((key, label.clone()));
        label
    }
}

pub(crate) fn convert(zip: &Zip<'_>, opts: &Options<'_>) -> Result<Converted, Error> {
    let main = opc::main_part(zip, "word/document.xml")?;
    let xml = zip
        .read_text(&main)?
        .ok_or_else(|| Error::Malformed("no word/document.xml".into()))?;
    let rels = Rels::of(zip, &main)?;
    let part = |kind: &str| rels.first(kind).map(|r| r.target.clone());
    let styles = match part("styles") {
        Some(p) => zip
            .read_text(&p)?
            .map(|x| Styles::parse(&x))
            .transpose()?
            .unwrap_or_default(),
        None => Styles::default(),
    };
    let numbering = match part("numbering") {
        Some(p) => zip
            .read_text(&p)?
            .map(|x| Numbering::parse(&x))
            .transpose()?
            .unwrap_or_default(),
        None => Numbering::default(),
    };
    let default_style = styles.default_id().map(str::to_string);
    let footnotes = part("footnotes");
    let endnotes = part("endnotes");
    let mut ctx = Ctx {
        styles,
        numbering,
        rels,
        default_style,
        opts,
        notes: Vec::new(),
    };

    let mut blocks = Vec::new();
    let mut r = Reader::new(&xml);
    while let Some(t) = r.next()? {
        if let Tok::Start(name, _) = t
            && local_name(name) == "body"
        {
            blocks = body::container(&mut r, &mut ctx)?;
            break;
        }
    }

    // 文末的脚注定义：只给正文里引用过的，按引用顺序
    if !ctx.notes.is_empty() {
        let mut texts = HashMap::new();
        for (endnote, path) in [(false, footnotes), (true, endnotes)] {
            if let Some(xml) = path.map(|p| zip.read_text(&p)).transpose()?.flatten() {
                for (id, text) in notes(&xml, &mut ctx)? {
                    texts.insert((endnote, id), text);
                }
            }
        }
        for (key, label) in ctx.notes.clone() {
            if let Some(text) = texts.get(&key) {
                blocks.push(Block::Raw(format!("[^{label}]: {text}")));
            }
        }
    }

    let mut out = Converted {
        markdown: render(&blocks),
        parts: 1,
        ..Converted::default()
    };
    opc::core_properties(zip, &mut out)?;
    Ok(out)
}

/// footnotes.xml / endnotes.xml：每条注释的 id 和文字（多段合成一行）。分隔线之类的特殊注释跳过。
fn notes(xml: &str, ctx: &mut Ctx<'_>) -> Result<Vec<(String, String)>, Error> {
    let mut out = Vec::new();
    let mut r = Reader::new(xml);
    while let Some(t) = r.next()? {
        let Tok::Start(name, a) = t else { continue };
        if !matches!(local_name(name), "footnote" | "endnote") {
            continue;
        }
        if a.get("type").is_some_and(|t| t != "normal") {
            r.skip()?;
            continue;
        }
        let id = a.get("id").unwrap_or_default().into_owned();
        let blocks = body::container(&mut r, ctx)?;
        let text: Vec<String> = blocks
            .iter()
            .map(|b| match b {
                Block::Heading(_, t) | Block::Para(t) | Block::Raw(t) => t.clone(),
                Block::Item { text, .. } => text.clone(),
                Block::Table(rows) => rows
                    .iter()
                    .map(|r| r.join(" "))
                    .collect::<Vec<_>>()
                    .join(" "),
            })
            .collect();
        out.push((id, text.join(" ").replace("\\\n", " ")));
    }
    Ok(out)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::zip::tests::build;

    const W: &str = r#"xmlns:w="http://schemas.openxmlformats.org/wordprocessingml/2006/main" xmlns:r="http://schemas.openxmlformats.org/officeDocument/2006/relationships""#;

    fn docx(body: &str, extra: &[(&str, &str)]) -> Vec<u8> {
        let doc = format!("<w:document {W}><w:body>{body}</w:body></w:document>");
        let mut parts: Vec<(&str, Vec<u8>)> = vec![("word/document.xml", doc.into_bytes())];
        for (n, x) in extra {
            parts.push((n, x.as_bytes().to_vec()));
        }
        let refs: Vec<(&str, &[u8], bool)> = parts
            .iter()
            .map(|(n, d)| (*n, d.as_slice(), true))
            .collect();
        build(&refs)
    }

    fn md(bytes: &[u8]) -> String {
        crate::convert(bytes, crate::Format::Docx, &Options::default())
            .unwrap()
            .markdown
    }

    #[test]
    fn headings_lists_tables_links_and_footnotes() {
        let styles = format!(
            r#"<w:styles {W}><w:style w:type="paragraph" w:styleId="H1"><w:name w:val="heading 1"/></w:style></w:styles>"#
        );
        let numbering = format!(
            r#"<w:numbering {W}><w:abstractNum w:abstractNumId="0"><w:lvl w:ilvl="0"><w:numFmt w:val="decimal"/></w:lvl><w:lvl w:ilvl="1"><w:numFmt w:val="bullet"/></w:lvl></w:abstractNum><w:num w:numId="1"><w:abstractNumId w:val="0"/></w:num></w:numbering>"#
        );
        let rels = r#"<Relationships><Relationship Id="s" Type="x/styles" Target="styles.xml"/><Relationship Id="n" Type="x/numbering" Target="numbering.xml"/><Relationship Id="f" Type="x/footnotes" Target="footnotes.xml"/><Relationship Id="l" Type="x/hyperlink" Target="https://e.com" TargetMode="External"/></Relationships>"#;
        let foot = format!(
            r#"<w:footnotes {W}><w:footnote w:type="separator" w:id="-1"><w:p/></w:footnote><w:footnote w:id="1"><w:p><w:r><w:t>Note text.</w:t></w:r></w:p></w:footnote></w:footnotes>"#
        );
        let body = r#"<w:p><w:pPr><w:pStyle w:val="H1"/></w:pPr><w:r><w:rPr><w:b/></w:rPr><w:t>Intro</w:t></w:r></w:p>
<w:p><w:r><w:t xml:space="preserve">See </w:t></w:r><w:hyperlink r:id="l"><w:r><w:t>site</w:t></w:r></w:hyperlink><w:r><w:rPr><w:b w:val="0"/><w:i/></w:rPr><w:t xml:space="preserve"> now</w:t></w:r><w:r><w:footnoteReference w:id="1"/></w:r></w:p>
<w:p><w:pPr><w:numPr><w:ilvl w:val="0"/><w:numId w:val="1"/></w:numPr></w:pPr><w:r><w:t>one</w:t></w:r></w:p>
<w:p><w:pPr><w:numPr><w:ilvl w:val="1"/><w:numId w:val="1"/></w:numPr></w:pPr><w:r><w:t>sub</w:t></w:r></w:p>
<w:p><w:pPr><w:numPr><w:ilvl w:val="0"/><w:numId w:val="1"/></w:numPr></w:pPr><w:r><w:t>two</w:t></w:r></w:p>
<w:tbl><w:tblGrid/><w:tr><w:tc><w:tcPr><w:gridSpan w:val="2"/></w:tcPr><w:p><w:r><w:t>wide</w:t></w:r></w:p></w:tc></w:tr>
<w:tr><w:tc><w:p><w:r><w:t>a|b</w:t></w:r></w:p><w:p><w:r><w:t>c</w:t></w:r></w:p></w:tc><w:tc><w:tcPr><w:vMerge/></w:tcPr><w:p/></w:tc></w:tr></w:tbl>
<w:p><w:r><w:delText>gone</w:delText></w:r><w:del><w:r><w:t>also gone</w:t></w:r></w:del></w:p>"#;
        let bytes = docx(
            body,
            &[
                ("word/_rels/document.xml.rels", rels),
                ("word/styles.xml", &styles),
                ("word/numbering.xml", &numbering),
                ("word/footnotes.xml", &foot),
            ],
        );
        assert_eq!(
            md(&bytes),
            "# Intro\n\nSee [site](https://e.com) *now*[^1]\n\n1. one\n    - sub\n2. two\n\n| wide |  |\n| --- | --- |\n| a\\|b<br>c |  |\n\n[^1]: Note text.\n"
        );
    }

    #[test]
    fn images_textboxes_and_math() {
        let rels = r#"<Relationships><Relationship Id="img" Type="x/image" Target="media/cat.png"/></Relationships>"#;
        let body = r#"<w:p><w:r><w:drawing><wp:inline><wp:docPr id="1" name="Pic" descr="A [grey] cat"/><a:graphic><a:graphicData><pic:pic><pic:blipFill><a:blip r:embed="img"/></pic:blipFill></pic:pic></a:graphicData></a:graphic></wp:inline></w:drawing></w:r></w:p>
<w:p><w:r><mc:AlternateContent><mc:Choice><w:drawing><wps:txbx><w:txbxContent><w:p><w:r><w:t>Boxed</w:t></w:r></w:p></w:txbxContent></wps:txbx></w:drawing></mc:Choice><mc:Fallback><w:pict><w:txbxContent><w:p><w:r><w:t>Boxed</w:t></w:r></w:p></w:txbxContent></w:pict></mc:Fallback></mc:AlternateContent></w:r></w:p>
<w:p><m:oMath><m:sSup><m:e><m:r><m:t>E</m:t></m:r></m:e><m:sup><m:r><m:t>2</m:t></m:r></m:sup></m:sSup></m:oMath></w:p>"#;
        let bytes = docx(body, &[("word/_rels/document.xml.rels", rels)]);
        assert_eq!(
            md(&bytes),
            "![A grey cat](word/media/cat.png)\n\nBoxed\n\n${E}^{2}$\n"
        );
    }
}
