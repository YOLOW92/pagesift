//! 正文（`w:body`、脚注、文本框、表格单元格）的遍历：段落、文字块、表格（ECMA-376 17.3、17.4）。

use super::Ctx;
use super::math;
use super::symbol::symbol_char;
use super::table::table;
use crate::md::{Block, escape};
use crate::xml::{Attrs, Reader, Tok, local_name};
use crate::{Error, check_deadline};

/// 一段行内文字和它的格式。`raw` 的是已经拼好的 Markdown（图片、公式、脚注引用），不再转义。
#[derive(Debug, Clone, Default)]
struct Seg {
    text: String,
    bold: bool,
    italic: bool,
    link: Option<String>,
    raw: bool,
}

/// 读一个容器元素（刚读到它的开始标签）里的块，直到它结束。
pub(super) fn container(r: &mut Reader<'_>, ctx: &mut Ctx<'_>) -> Result<Vec<Block>, Error> {
    let mut blocks = Vec::new();
    let mut depth = 1;
    let mut count = 0u32;
    while let Some(t) = r.next()? {
        match t {
            Tok::Start(name, a) => match local_name(name) {
                "p" => {
                    count += 1;
                    if count.is_multiple_of(256) {
                        check_deadline(ctx.opts)?;
                    }
                    paragraph(r, ctx, &mut blocks)?;
                }
                "tbl" => blocks.extend(table(r, ctx)?),
                // 删除的内容、节属性、Office 2010 以后的替代内容的旧版备份（和新版重复）
                "del" | "moveFrom" | "sectPr" | "Fallback" => r.skip()?,
                "oMathPara" | "oMath" => {
                    let node = math::Node::read(r, local_name(name))?;
                    blocks.push(Block::Raw(format!("$$\n{}\n$$", node.latex().trim())));
                }
                _ => {
                    let _ = a;
                    depth += 1;
                }
            },
            Tok::End(_) => {
                depth -= 1;
                if depth == 0 {
                    break;
                }
            }
            Tok::Text(_) => {}
        }
    }
    Ok(blocks)
}

#[derive(Default)]
struct ParaProps {
    style: Option<String>,
    num: Option<(String, u8)>,
    outline: Option<u8>,
}

fn paragraph_props(r: &mut Reader<'_>, p: &mut ParaProps) -> Result<(), Error> {
    let mut depth = 1;
    let mut num_id = None;
    let mut ilvl = 0u8;
    while let Some(t) = r.next()? {
        match t {
            Tok::Start(name, a) => match local_name(name) {
                // 段落标记的字符格式、修订前的旧属性
                "rPr" | "pPrChange" | "sectPr" => r.skip()?,
                local => {
                    depth += 1;
                    match local {
                        "pStyle" => p.style = a.get("val").map(|v| v.into_owned()),
                        "numId" => num_id = a.get("val").map(|v| v.into_owned()),
                        "ilvl" => ilvl = a.get("val").and_then(|v| v.parse().ok()).unwrap_or(0),
                        "outlineLvl" => p.outline = a.get("val").and_then(|v| v.parse().ok()),
                        _ => {}
                    }
                }
            },
            Tok::End(_) => {
                depth -= 1;
                if depth == 0 {
                    break;
                }
            }
            Tok::Text(_) => {}
        }
    }
    if let Some(id) = num_id {
        p.num = Some((id, ilvl));
    }
    Ok(())
}

/// 读一个段落，产出的块（通常一个；段落里的文本框、行内的表格另成块）追加到 `out`。
pub(super) fn paragraph(
    r: &mut Reader<'_>,
    ctx: &mut Ctx<'_>,
    out: &mut Vec<Block>,
) -> Result<(), Error> {
    let mut props = ParaProps::default();
    let mut segs: Vec<Seg> = Vec::new();
    let mut extra = Vec::new();
    // 当前所在的超链接（嵌套时以最内层为准）和它开始时的深度
    let mut links: Vec<(usize, Option<String>)> = Vec::new();
    let mut depth = 1usize;
    while let Some(t) = r.next()? {
        match t {
            Tok::Start(name, a) => match local_name(name) {
                "pPr" => paragraph_props(r, &mut props)?,
                "r" => {
                    let link = links.last().and_then(|l| l.1.clone());
                    run(r, ctx, &mut segs, &mut extra, link)?;
                }
                "hyperlink" => {
                    depth += 1;
                    links.push((depth, hyperlink_target(ctx, &a)));
                }
                "oMath" | "oMathPara" => {
                    let node = math::Node::read(r, local_name(name))?;
                    let tex = node.latex();
                    let tex = tex.trim();
                    if !tex.is_empty() {
                        segs.push(Seg {
                            text: format!("${tex}$"),
                            raw: true,
                            ..Seg::default()
                        });
                    }
                }
                "del" | "moveFrom" | "Fallback" => r.skip()?,
                _ => depth += 1,
            },
            Tok::End(_) => {
                if links.last().is_some_and(|l| l.0 == depth) {
                    links.pop();
                }
                depth -= 1;
                if depth == 0 {
                    break;
                }
            }
            Tok::Text(_) => {}
        }
    }
    finish_paragraph(ctx, props, &segs, out);
    out.extend(extra);
    Ok(())
}

fn hyperlink_target(ctx: &Ctx<'_>, a: &Attrs<'_>) -> Option<String> {
    if let Some(id) = a.get("id") {
        let rel = ctx.rels.get(&id)?;
        if rel.external {
            let anchor = a.get("anchor").map(|x| format!("#{x}")).unwrap_or_default();
            return Some(format!("{}{anchor}", rel.target));
        }
    }
    None
}

fn on(a: &Attrs<'_>) -> bool {
    a.get("val")
        .is_none_or(|v| !matches!(v.as_ref(), "0" | "false" | "off" | "none"))
}

/// 一个文字块 `w:r`：格式、文字、换行、脚注引用、图片、文本框。
fn run(
    r: &mut Reader<'_>,
    ctx: &mut Ctx<'_>,
    segs: &mut Vec<Seg>,
    extra: &mut Vec<Block>,
    link: Option<String>,
) -> Result<(), Error> {
    let mut bold = false;
    let mut italic = false;
    let mut text = String::new();
    let mut depth = 1;
    let mut in_rpr = false;
    // 用 Symbol 字体写的文字：字符码按 Symbol 编码解释（Word 插入的符号常是私用区的 F0xx）
    let mut symbol_font = false;
    let flush = |text: &mut String,
                 segs: &mut Vec<Seg>,
                 bold,
                 italic,
                 link: &Option<String>,
                 symbol: bool| {
        if !text.is_empty() {
            let mut t = std::mem::take(text);
            if symbol {
                t = t
                    .chars()
                    .map(|c| symbol_char("Symbol", c as u32).unwrap_or(c))
                    .collect();
            }
            segs.push(Seg {
                text: t,
                bold,
                italic,
                link: link.clone(),
                raw: false,
            });
        }
    };
    while let Some(t) = r.next()? {
        match t {
            Tok::Start(name, a) => {
                let local = local_name(name);
                match local {
                    "rPrChange" => r.skip()?,
                    "t" => text.push_str(&r.text_until_end()?),
                    // 域代码、删除的文字不输出；域的结果是普通文字，照常输出
                    "instrText" | "delText" | "delInstrText" | "Fallback" => r.skip()?,
                    "drawing" | "pict" | "object" => {
                        flush(&mut text, segs, bold, italic, &link, symbol_font);
                        drawing(r, ctx, segs, extra)?;
                    }
                    _ => {
                        depth += 1;
                        match local {
                            "rPr" => in_rpr = true,
                            "b" if in_rpr => bold = on(&a),
                            "i" if in_rpr => italic = on(&a),
                            "rFonts" if in_rpr => {
                                symbol_font = a
                                    .get("ascii")
                                    .or_else(|| a.get("hAnsi"))
                                    .is_some_and(|f| f.eq_ignore_ascii_case("symbol"));
                            }
                            "tab" | "ptab" if !in_rpr => text.push(' '),
                            "br" | "cr" => {
                                if a.get("type").is_none_or(|t| t == "textWrapping") {
                                    text.push('\n');
                                }
                            }
                            "noBreakHyphen" => text.push('-'),
                            "sym" => {
                                let font = a.get("font").unwrap_or_default();
                                if let Some(c) = a
                                    .get("char")
                                    .and_then(|c| u32::from_str_radix(&c, 16).ok())
                                    .and_then(|c| symbol_char(&font, c))
                                {
                                    text.push(c);
                                }
                            }
                            "footnoteReference" | "endnoteReference" => {
                                flush(&mut text, segs, bold, italic, &link, symbol_font);
                                if let Some(id) = a.get("id") {
                                    let label = ctx.note_label(local == "endnoteReference", &id);
                                    segs.push(Seg {
                                        text: format!("[^{label}]"),
                                        raw: true,
                                        ..Seg::default()
                                    });
                                }
                            }
                            _ => {}
                        }
                    }
                }
            }
            Tok::End(name) => {
                if local_name(name) == "rPr" {
                    in_rpr = false;
                }
                depth -= 1;
                if depth == 0 {
                    break;
                }
            }
            Tok::Text(_) => {}
        }
    }
    flush(&mut text, segs, bold, italic, &link, symbol_font);
    Ok(())
}

/// 图（DrawingML 的 `w:drawing`、VML 的 `w:pict`）：替代文字和图片路径；里面的文本框另成块。
fn drawing(
    r: &mut Reader<'_>,
    ctx: &mut Ctx<'_>,
    segs: &mut Vec<Seg>,
    extra: &mut Vec<Block>,
) -> Result<(), Error> {
    let mut alt = String::new();
    let mut target = None;
    let mut depth = 1;
    while let Some(t) = r.next()? {
        match t {
            Tok::Start(name, a) => match local_name(name) {
                "docPr" | "cNvPr" => {
                    if alt.is_empty() {
                        alt = a
                            .get("descr")
                            .or_else(|| a.get("title"))
                            .unwrap_or_default()
                            .into_owned();
                    }
                    depth += 1;
                }
                "blip" | "imagedata" => {
                    let id = a.get("embed").or_else(|| a.get("id"));
                    if target.is_none() {
                        target = id
                            .and_then(|id| ctx.rels.get(&id))
                            .map(|rel| rel.target.clone());
                    }
                    if alt.is_empty()
                        && let Some(t) = a.get("title")
                    {
                        alt = t.into_owned();
                    }
                    depth += 1;
                }
                "txbxContent" => extra.extend(container(r, ctx)?),
                "Fallback" => r.skip()?,
                _ => depth += 1,
            },
            Tok::End(_) => {
                depth -= 1;
                if depth == 0 {
                    break;
                }
            }
            Tok::Text(_) => {}
        }
    }
    if let Some(src) = target {
        let alt = crate::md::squeeze(&alt.replace(['[', ']', '\n'], " "));
        segs.push(Seg {
            text: format!("![{}]({})", escape(&alt), src.replace(' ', "%20")),
            raw: true,
            ..Seg::default()
        });
    }
    Ok(())
}

/// 把行内片段拼成 Markdown：相邻同格式的片段合并，强调记号贴着文字（空白移到记号外面）。
fn inline(segs: &[Seg], plain: bool) -> String {
    let mut out = String::new();
    let mut i = 0;
    while i < segs.len() {
        let s = &segs[i];
        let mut j = i + 1;
        let mut text = if s.raw {
            s.text.clone()
        } else {
            escape(&s.text)
        };
        while j < segs.len()
            && !segs[j].raw
            && !s.raw
            && segs[j].bold == s.bold
            && segs[j].italic == s.italic
            && segs[j].link == s.link
        {
            text.push_str(&escape(&segs[j].text));
            j += 1;
        }
        let marks = if plain || s.raw {
            ""
        } else {
            match (s.bold, s.italic) {
                (true, true) => "***",
                (true, false) => "**",
                (false, true) => "*",
                _ => "",
            }
        };
        let core = text.trim();
        let lead = &text[..text.len() - text.trim_start().len()];
        let trail = &text[text.trim_end().len()..];
        let mut piece = if core.is_empty() || marks.is_empty() || core.contains('\n') {
            core.to_string()
        } else {
            format!("{marks}{core}{marks}")
        };
        if let Some(url) = s.link.as_ref().filter(|_| !plain && !core.is_empty()) {
            piece = format!("[{piece}]({})", url.replace(' ', "%20").replace(')', "%29"));
        }
        out.push_str(lead);
        out.push_str(&piece);
        out.push_str(trail);
        i = j;
    }
    out
}

fn finish_paragraph(ctx: &mut Ctx<'_>, props: ParaProps, segs: &[Seg], out: &mut Vec<Block>) {
    let style = props.style.as_deref().or(ctx.default_style.as_deref());
    let level = match props.outline {
        Some(o) if o < 9 => Some(o + 1),
        _ => style.and_then(|s| ctx.styles.heading_level(s)),
    };
    if let Some(level) = level {
        let text = crate::md::squeeze(&inline(segs, true)).replace('\n', " ");
        if !text.is_empty() {
            out.push(Block::Heading(level.min(6), text));
        }
        return;
    }
    let text = crate::md::squeeze(&inline(segs, false));
    if text.is_empty() {
        return;
    }
    let text = text.replace('\n', "\\\n");
    let num = props
        .num
        .or_else(|| style.and_then(|s| ctx.styles.numbering(s)));
    if let Some((id, ilvl)) = num
        && let Some((ordered, number)) = ctx.numbering.next(&id, ilvl)
    {
        out.push(Block::Item {
            level: ilvl.min(8),
            ordered,
            number,
            text,
        });
        return;
    }
    out.push(Block::Para(text));
}

#[cfg(test)]
mod tests {
    use super::*;

    fn seg(text: &str, bold: bool, italic: bool) -> Seg {
        Seg {
            text: text.into(),
            bold,
            italic,
            ..Seg::default()
        }
    }

    #[test]
    fn merges_runs_and_keeps_spaces_outside_emphasis() {
        let s = [
            seg("Hello ", false, false),
            seg("bold ", true, false),
            seg("text", true, false),
            seg(" and *star*", false, true),
        ];
        assert_eq!(inline(&s, false), "Hello **bold text** *and \\*star\\**");
        assert_eq!(inline(&s, true), "Hello bold text and \\*star\\*");
        let link = Seg {
            text: "site".into(),
            link: Some("https://e.com/a b".into()),
            ..Seg::default()
        };
        assert_eq!(inline(&[link], false), "[site](https://e.com/a%20b)");
    }
}
