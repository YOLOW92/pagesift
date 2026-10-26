//! PowerPoint（PresentationML，ECMA-376 第 1 部分第 19 章）转 Markdown。
//!
//! 幻灯片按 presentation.xml 的 sldIdLst 顺序；每张先写 `<!-- Slide N -->`，标题占位符成二级标题，
//! 其余形状按位置从上到下、从左到右排（占位符没写坐标时从版式、母版继承）。正文占位符的段落默认带
//! 项目符号（母版 bodyStyle 里设了 buNone 的级别除外），普通文本框只有显式设了符号才算列表。
//! 表格、图片替代文字、图表的缓存数据、演讲者备注都输出。

mod chart;
mod shapes;

use std::collections::HashMap;

use crate::md::{Block, escape, render};
use crate::opc::{self, Rels};
use crate::xml::{Reader, Tok, local_name};
use crate::zip::Zip;
use crate::{Converted, Error, Options, check_deadline};
use shapes::{Content, Para, Shape};

/// 版式或母版上的占位符位置：(类型, idx) → 坐标。
type Positions = Vec<((String, Option<String>), (i64, i64))>;

pub(crate) fn convert(zip: &Zip<'_>, opts: &Options<'_>) -> Result<Converted, Error> {
    let main = opc::main_part(zip, "ppt/presentation.xml")?;
    let xml = zip
        .read_text(&main)?
        .ok_or_else(|| Error::Malformed("no ppt/presentation.xml".into()))?;
    let rels = Rels::of(zip, &main)?;
    let mut ids = Vec::new();
    let mut r = Reader::new(&xml);
    while let Some(t) = r.next()? {
        // sldId 有数字的 @id 和关系的 @r:id 两个属性，要的是关系
        if let Tok::Start(name, a) = t
            && local_name(name) == "sldId"
            && let Some(rid) = a
                .iter()
                .find(|(k, _)| k.ends_with(":id"))
                .map(|(_, v)| v.into_owned())
        {
            ids.push(rid);
        }
    }
    let mut layouts: HashMap<String, (Positions, Vec<bool>)> = HashMap::new();
    let mut blocks = Vec::new();
    let mut n = 0;
    for rid in &ids {
        let Some(part) = rels.get(rid).map(|r| r.target.clone()) else {
            continue;
        };
        let Some(sx) = zip.read_text(&part)? else {
            continue;
        };
        check_deadline(opts)?;
        n += 1;
        let srels = Rels::of(zip, &part)?;
        let layout = srels
            .first("slideLayout")
            .map(|r| r.target.clone())
            .unwrap_or_default();
        if !layouts.contains_key(&layout) {
            let info = layout_info(zip, &layout)?;
            layouts.insert(layout.clone(), info);
        }
        let (positions, body_bullets) = &layouts[&layout];
        blocks.push(Block::Raw(format!("<!-- Slide {n} -->")));
        slide(zip, &sx, &srels, positions, body_bullets, &mut blocks)?;
        if let Some(notes) = srels.first("notesSlide") {
            notes_blocks(zip, &notes.target, &mut blocks)?;
        }
    }
    let mut out = Converted {
        markdown: render(&blocks),
        parts: n,
        ..Converted::default()
    };
    opc::core_properties(zip, &mut out)?;
    Ok(out)
}

fn slide(
    zip: &Zip<'_>,
    xml: &str,
    rels: &Rels,
    positions: &Positions,
    body_bullets: &[bool],
    blocks: &mut Vec<Block>,
) -> Result<(), Error> {
    let mut list = Vec::new();
    let mut r = Reader::new(xml);
    while let Some(t) = r.next()? {
        if let Tok::Start(name, _) = t
            && local_name(name) == "spTree"
        {
            shapes::tree(&mut r, &mut list)?;
            break;
        }
    }
    // 没写坐标的占位符用版式/母版上同类型（或同 idx）占位符的坐标
    for s in &mut list {
        if s.pos.is_none()
            && let Some((kind, idx)) = &s.ph
        {
            s.pos = positions
                .iter()
                .find(|((_, i), _)| idx.is_some() && i == idx)
                .or_else(|| {
                    positions
                        .iter()
                        .find(|((k, _), _)| k == kind || (is_title(k) && is_title(kind)))
                })
                .map(|(_, p)| *p);
        }
    }
    let title = |s: &Shape| s.ph.as_ref().is_some_and(|(k, _)| is_title(k));
    let mut order: Vec<usize> = (0..list.len()).collect();
    order.sort_by_key(|&i| {
        (
            !title(&list[i]),
            list[i].pos.unwrap_or((i64::MAX, i64::MAX)),
        )
    });
    let mut has_title = false;
    for i in order {
        let s = &list[i];
        match &s.content {
            Content::Text(paras) if title(s) && !has_title => {
                let text: Vec<&str> = paras
                    .iter()
                    .map(|p| p.text.as_str())
                    .filter(|t| !t.is_empty())
                    .collect();
                let text = crate::md::squeeze(&text.join(" ").replace('\n', " ").replace('*', ""));
                if !text.is_empty() {
                    blocks.push(Block::Heading(2, text));
                    has_title = true;
                }
            }
            Content::Text(paras) => {
                let body =
                    s.ph.as_ref()
                        .is_some_and(|(k, _)| matches!(k.as_str(), "body" | "obj"));
                text_blocks(paras, body, body_bullets, blocks);
            }
            Content::Picture { alt, embed } => {
                let src = embed
                    .as_deref()
                    .and_then(|e| rels.get(e))
                    .map(|r| r.target.replace(' ', "%20"))
                    .unwrap_or_default();
                let alt = crate::md::squeeze(&alt.replace(['[', ']', '\n'], " "));
                if !src.is_empty() || !alt.is_empty() {
                    blocks.push(Block::Para(format!("![{}]({src})", escape(&alt))));
                }
            }
            Content::Table(rows) if !rows.is_empty() => blocks.push(Block::Table(rows.clone())),
            Content::Table(_) => {}
            Content::Chart(id) => {
                let Some(part) = rels.get(id) else { continue };
                let Some(cx) = zip.read_text(&part.target)? else {
                    continue;
                };
                let (title, rows) = chart::read(&cx)?;
                blocks.push(Block::Para(if title.is_empty() {
                    "**Chart**".into()
                } else {
                    format!("**Chart: {}**", escape(&title))
                }));
                if !rows.is_empty() {
                    blocks.push(Block::Table(rows));
                }
            }
        }
    }
    Ok(())
}

fn is_title(kind: &str) -> bool {
    matches!(kind, "title" | "ctrTitle")
}

fn text_blocks(paras: &[Para], body: bool, body_bullets: &[bool], blocks: &mut Vec<Block>) {
    let mut counters = [0u32; 9];
    for p in paras {
        if p.text.is_empty() {
            continue;
        }
        let level = p.level.min(8);
        let inherited = body
            && body_bullets
                .get(usize::from(level))
                .copied()
                .unwrap_or(true);
        let text = p.text.replace('\n', "\\\n");
        if p.bullet.unwrap_or(inherited) {
            counters[usize::from(level)] += 1;
            for c in &mut counters[usize::from(level) + 1..] {
                *c = 0;
            }
            blocks.push(Block::Item {
                level,
                ordered: p.numbered,
                number: counters[usize::from(level)],
                text,
            });
        } else {
            counters = [0; 9];
            blocks.push(Block::Para(text));
        }
    }
}

/// 版式（和它的母版）上的占位符坐标，以及母版 bodyStyle 里每一级是否带项目符号。
fn layout_info(zip: &Zip<'_>, layout: &str) -> Result<(Positions, Vec<bool>), Error> {
    let mut positions = Positions::new();
    let mut bullets = vec![true; 9];
    if layout.is_empty() {
        return Ok((positions, bullets));
    }
    let master = Rels::of(zip, layout)?
        .first("slideMaster")
        .map(|r| r.target.clone());
    for part in [Some(layout.to_string()), master.clone()]
        .into_iter()
        .flatten()
    {
        let Some(xml) = zip.read_text(&part)? else {
            continue;
        };
        let mut list = Vec::new();
        let mut r = Reader::new(&xml);
        while let Some(t) = r.next()? {
            if let Tok::Start(name, _) = t
                && local_name(name) == "spTree"
            {
                shapes::tree(&mut r, &mut list)?;
                break;
            }
        }
        for s in list {
            if let (Some(ph), Some(pos)) = (s.ph, s.pos) {
                positions.push((ph, pos));
            }
        }
        if Some(&part) == master.as_ref() {
            bullets = body_style_bullets(&xml)?;
        }
    }
    Ok((positions, bullets))
}

/// 母版 `p:txStyles/p:bodyStyle` 每一级（lvl1pPr…lvl9pPr）有没有项目符号：写了 buNone 的没有。
fn body_style_bullets(xml: &str) -> Result<Vec<bool>, Error> {
    let mut out = vec![true; 9];
    let mut r = Reader::new(xml);
    let mut in_body = false;
    let mut level = None;
    while let Some(t) = r.next()? {
        match t {
            Tok::Start(name, _) => {
                let l = local_name(name);
                if l == "bodyStyle" {
                    in_body = true;
                } else if in_body && l.starts_with("lvl") && l.ends_with("pPr") {
                    level = l[3..l.len() - 3]
                        .parse::<usize>()
                        .ok()
                        .map(|n| n.saturating_sub(1));
                } else if in_body
                    && l == "buNone"
                    && let Some(i) = level.filter(|i| *i < 9)
                {
                    out[i] = false;
                }
            }
            Tok::End(name) if local_name(name) == "bodyStyle" => break,
            _ => {}
        }
    }
    Ok(out)
}

/// 演讲者备注：备注页上正文占位符的文字。
fn notes_blocks(zip: &Zip<'_>, part: &str, blocks: &mut Vec<Block>) -> Result<(), Error> {
    let Some(xml) = zip.read_text(part)? else {
        return Ok(());
    };
    let mut list = Vec::new();
    let mut r = Reader::new(&xml);
    while let Some(t) = r.next()? {
        if let Tok::Start(name, _) = t
            && local_name(name) == "spTree"
        {
            shapes::tree(&mut r, &mut list)?;
            break;
        }
    }
    let mut lines = Vec::new();
    for s in list {
        if let (Some((kind, _)), Content::Text(paras)) = (&s.ph, &s.content)
            && kind == "body"
        {
            lines.extend(
                paras
                    .iter()
                    .map(|p| p.text.clone())
                    .filter(|t| !t.is_empty()),
            );
        }
    }
    if !lines.is_empty() {
        blocks.push(Block::Para("**Notes:**".into()));
        blocks.extend(
            lines
                .into_iter()
                .map(|l| Block::Para(l.replace('\n', "\\\n"))),
        );
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::zip::tests::build;

    #[test]
    fn slides_in_order_with_titles_bullets_tables_pictures_and_notes() {
        let pres = r#"<p:presentation xmlns:r="r"><p:sldIdLst><p:sldId id="257" r:id="s2"/><p:sldId id="256" r:id="s1"/></p:sldIdLst></p:presentation>"#;
        let prels = r#"<Relationships><Relationship Id="s1" Type="x/slide" Target="slides/slide1.xml"/><Relationship Id="s2" Type="x/slide" Target="slides/slide2.xml"/></Relationships>"#;
        let s1 = r#"<p:sld><p:cSld><p:spTree>
<p:sp><p:nvSpPr><p:cNvPr id="3" name="Body"/><p:nvPr><p:ph idx="1"/></p:nvPr></p:nvSpPr><p:spPr/><p:txBody><a:bodyPr/><a:p><a:r><a:t>First point</a:t></a:r></a:p><a:p><a:pPr lvl="1"/><a:r><a:rPr b="1"/><a:t>Sub point</a:t></a:r></a:p></p:txBody></p:sp>
<p:sp><p:nvSpPr><p:cNvPr id="2" name="Title"/><p:nvPr><p:ph type="title"/></p:nvPr></p:nvSpPr><p:spPr/><p:txBody><a:p><a:r><a:t>Agenda</a:t></a:r></a:p></p:txBody></p:sp>
<p:pic><p:nvPicPr><p:cNvPr id="4" name="Picture 3" descr="Logo"/></p:nvPicPr><p:blipFill><a:blip r:embed="i1"/></p:blipFill><p:spPr><a:xfrm><a:off x="100" y="9000"/></a:xfrm></p:spPr></p:pic>
<p:graphicFrame><p:nvGraphicFramePr><p:cNvPr id="5" name="Table"/></p:nvGraphicFramePr><p:xfrm><a:off x="0" y="5000"/></p:xfrm><a:graphic><a:graphicData><a:tbl><a:tblGrid/><a:tr><a:tc><a:txBody><a:p><a:r><a:t>A</a:t></a:r></a:p></a:txBody></a:tc><a:tc><a:txBody><a:p><a:r><a:t>B</a:t></a:r></a:p></a:txBody></a:tc></a:tr></a:tbl></a:graphicData></a:graphic></p:graphicFrame>
<p:sp><p:nvSpPr><p:cNvPr id="6" name="Box" txBox="1"/><p:nvPr/></p:nvSpPr><p:spPr><a:xfrm><a:off x="0" y="7000"/></a:xfrm></p:spPr><p:txBody><a:p><a:r><a:t>Plain text</a:t></a:r></a:p></p:txBody></p:sp>
</p:spTree></p:cSld></p:sld>"#;
        let s1rels = r#"<Relationships><Relationship Id="i1" Type="x/image" Target="../media/logo.png"/><Relationship Id="n" Type="x/notesSlide" Target="../notesSlides/notesSlide1.xml"/><Relationship Id="l" Type="x/slideLayout" Target="../slideLayouts/slideLayout1.xml"/></Relationships>"#;
        let layout = r#"<p:sldLayout><p:cSld><p:spTree><p:sp><p:nvSpPr><p:nvPr><p:ph type="body" idx="1"/></p:nvPr></p:nvSpPr><p:spPr><a:xfrm><a:off x="0" y="1000"/></a:xfrm></p:spPr></p:sp></p:spTree></p:cSld></p:sldLayout>"#;
        let notes = r#"<p:notes><p:cSld><p:spTree><p:sp><p:nvSpPr><p:nvPr><p:ph type="sldImg"/></p:nvPr></p:nvSpPr></p:sp><p:sp><p:nvSpPr><p:nvPr><p:ph type="body" idx="1"/></p:nvPr></p:nvSpPr><p:txBody><a:p><a:r><a:t>Say hello.</a:t></a:r></a:p></p:txBody></p:sp></p:spTree></p:cSld></p:notes>"#;
        let s2 = r#"<p:sld><p:cSld><p:spTree><p:sp><p:nvSpPr><p:nvPr><p:ph type="ctrTitle"/></p:nvPr></p:nvSpPr><p:txBody><a:p><a:r><a:t>Welcome</a:t></a:r></a:p></p:txBody></p:sp></p:spTree></p:cSld></p:sld>"#;
        let parts: Vec<(&str, &[u8], bool)> = vec![
            ("ppt/presentation.xml", pres.as_bytes(), true),
            ("ppt/_rels/presentation.xml.rels", prels.as_bytes(), true),
            ("ppt/slides/slide1.xml", s1.as_bytes(), true),
            ("ppt/slides/_rels/slide1.xml.rels", s1rels.as_bytes(), true),
            ("ppt/slides/slide2.xml", s2.as_bytes(), true),
            ("ppt/slideLayouts/slideLayout1.xml", layout.as_bytes(), true),
            ("ppt/notesSlides/notesSlide1.xml", notes.as_bytes(), true),
        ];
        let got = crate::convert(&build(&parts), crate::Format::Pptx, &Options::default()).unwrap();
        assert_eq!(
            got.markdown,
            "<!-- Slide 1 -->\n\n## Welcome\n\n<!-- Slide 2 -->\n\n## Agenda\n\n- First point\n    - **Sub point**\n\n| A | B |\n| --- | --- |\n\nPlain text\n\n![Logo](ppt/media/logo.png)\n\n**Notes:**\n\nSay hello.\n"
        );
        assert_eq!(got.parts, 2);
    }
}
