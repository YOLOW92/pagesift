//! 幻灯片上的形状（PresentationML 19.3、DrawingML 20.1）：文字框、图片、表格、图表、组合。

use crate::Error;
use crate::md::{escape, squeeze};
use crate::xml::{Attrs, Reader, Tok, local_name};

/// 段落：层级、显式的项目符号设置（`Some(true)` 有、`Some(false)` 关掉、`None` 继承）、文字。
#[derive(Debug, Clone, PartialEq)]
pub(super) struct Para {
    pub level: u8,
    pub bullet: Option<bool>,
    pub numbered: bool,
    /// 带加粗、斜体记号的文字。
    pub text: String,
    /// 同样的文字，不带强调记号（标题用）。
    pub plain: String,
}

#[derive(Debug, Clone, PartialEq)]
pub(super) enum Content {
    Text(Vec<Para>),
    Picture {
        alt: String,
        embed: Option<String>,
    },
    Table(Vec<Vec<String>>),
    Chart(String),
    /// SmartArt：数据部件（`dgm:relIds/@r:dm`）的关系 id。
    Diagram(String),
}

#[derive(Debug, Clone, PartialEq)]
pub(super) struct Shape {
    /// 占位符类型（`title`、`body`……；没有 type 的占位符是 `obj`）。
    pub ph: Option<(String, Option<String>)>,
    pub pos: Option<(i64, i64)>,
    pub content: Content,
}

/// 读一棵形状树（`p:spTree` 或 `p:grpSp`，刚读到开始标签），按文档顺序返回形状；组合展开，
/// 组合里没有坐标的形状用组合的坐标。
pub(super) fn tree(r: &mut Reader<'_>, out: &mut Vec<Shape>) -> Result<(), Error> {
    let mut group_pos = None;
    let start = out.len();
    let mut depth = 1;
    while let Some(t) = r.next()? {
        match t {
            Tok::Start(name, _) => match local_name(name) {
                "sp" => out.push(shape(r, false)?),
                "pic" => out.push(shape(r, true)?),
                "graphicFrame" => out.push(shape(r, false)?),
                "grpSp" => tree(r, out)?,
                "grpSpPr" => group_pos = position(r)?,
                "cxnSp" | "Fallback" | "extLst" => r.skip()?,
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
    // 组合内的形状统一用组合的位置排序（子坐标系和幻灯片坐标不同）
    if group_pos.is_some() {
        for s in &mut out[start..] {
            s.pos = group_pos;
        }
    }
    Ok(())
}

/// `a:xfrm` 里的 `a:off`（刚读到包含它的属性元素的开始标签），读到该元素结束。
fn position(r: &mut Reader<'_>) -> Result<Option<(i64, i64)>, Error> {
    let mut pos = None;
    let mut depth = 1;
    while let Some(t) = r.next()? {
        match t {
            Tok::Start(name, a) => {
                if local_name(name) == "off" && pos.is_none() {
                    pos = off(&a);
                }
                depth += 1;
            }
            Tok::End(_) => {
                depth -= 1;
                if depth == 0 {
                    break;
                }
            }
            Tok::Text(_) => {}
        }
    }
    Ok(pos)
}

fn off(a: &Attrs<'_>) -> Option<(i64, i64)> {
    Some((a.get("y")?.parse().ok()?, a.get("x")?.parse().ok()?))
}

fn shape(r: &mut Reader<'_>, picture: bool) -> Result<Shape, Error> {
    let mut ph = None;
    let mut pos = None;
    let mut alt = String::new();
    let mut embed = None;
    let mut content = None;
    let mut depth = 1;
    while let Some(t) = r.next()? {
        match t {
            Tok::Start(name, a) => match local_name(name) {
                "ph" => {
                    let kind = a.get("type").map_or("obj".to_string(), |t| t.into_owned());
                    ph = Some((kind, a.get("idx").map(|v| v.into_owned())));
                    depth += 1;
                }
                "cNvPr" => {
                    alt = a
                        .get("descr")
                        .or_else(|| a.get("title"))
                        .unwrap_or_default()
                        .into_owned();
                    depth += 1;
                }
                "off" if pos.is_none() => {
                    pos = off(&a);
                    depth += 1;
                }
                "blip" => {
                    embed = a.get("embed").map(|v| v.into_owned());
                    depth += 1;
                }
                "txBody" => content = Some(Content::Text(text_body(r)?)),
                "tbl" => content = Some(Content::Table(table(r)?)),
                "chart" => {
                    content = a.get("id").map(|id| Content::Chart(id.into_owned()));
                    depth += 1;
                }
                "relIds" => {
                    content = a.get("dm").map(|id| Content::Diagram(id.into_owned()));
                    depth += 1;
                }
                "Fallback" | "extLst" => r.skip()?,
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
    let content = if picture {
        Content::Picture { alt, embed }
    } else {
        content.unwrap_or(Content::Text(Vec::new()))
    };
    Ok(Shape { ph, pos, content })
}

/// `p:txBody` / `a:txBody`：每个段落的层级、项目符号、文字（加粗、斜体照写）。
pub(super) fn text_body(r: &mut Reader<'_>) -> Result<Vec<Para>, Error> {
    let mut paras = Vec::new();
    let mut depth = 1;
    while let Some(t) = r.next()? {
        match t {
            Tok::Start(name, _) => match local_name(name) {
                "p" => paras.push(paragraph(r)?),
                "lstStyle" | "bodyPr" => r.skip()?,
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
    Ok(paras)
}

fn paragraph(r: &mut Reader<'_>) -> Result<Para, Error> {
    let mut p = Para {
        level: 0,
        bullet: None,
        numbered: false,
        text: String::new(),
        plain: String::new(),
    };
    let mut depth = 1;
    let (mut bold, mut italic) = (false, false);
    while let Some(t) = r.next()? {
        match t {
            Tok::Start(name, a) => match local_name(name) {
                "pPr" => {
                    p.level = a.get("lvl").and_then(|v| v.parse().ok()).unwrap_or(0);
                    depth += 1;
                }
                "buNone" => {
                    p.bullet = Some(false);
                    depth += 1;
                }
                "buChar" | "buBlip" => {
                    p.bullet = Some(true);
                    depth += 1;
                }
                "buAutoNum" => {
                    p.bullet = Some(true);
                    p.numbered = true;
                    depth += 1;
                }
                "rPr" => {
                    bold = a.get("b").is_some_and(|v| v == "1" || v == "true");
                    italic = a.get("i").is_some_and(|v| v == "1" || v == "true");
                    r.skip()?;
                }
                "t" => {
                    let text = r.text_until_end()?;
                    p.plain.push_str(&escape(&text));
                    let core = text.trim();
                    let marks = match (bold, italic) {
                        (true, true) => "***",
                        (true, false) => "**",
                        (false, true) => "*",
                        _ => "",
                    };
                    if core.is_empty() || marks.is_empty() {
                        p.text.push_str(&escape(&text));
                    } else {
                        let lead = &text[..text.len() - text.trim_start().len()];
                        let trail = &text[text.trim_end().len()..];
                        p.text
                            .push_str(&format!("{lead}{marks}{}{marks}{trail}", escape(core)));
                    }
                }
                "br" => {
                    p.text.push('\n');
                    p.plain.push('\n');
                    depth += 1;
                }
                "r" | "fld" => {
                    bold = false;
                    italic = false;
                    depth += 1;
                }
                "pPrChange" | "endParaRPr" | "extLst" => r.skip()?,
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
    // 相邻同格式的片段在源文件里经常被拆成几段（**a****b**），合并掉
    p.text = squeeze(&p.text.replace("****", "").replace("**  **", " "));
    p.plain = squeeze(&p.plain);
    Ok(p)
}

/// `a:tbl`：横向、纵向合并的续格留空。
fn table(r: &mut Reader<'_>) -> Result<Vec<Vec<String>>, Error> {
    let mut rows: Vec<Vec<String>> = Vec::new();
    let mut depth = 1;
    while let Some(t) = r.next()? {
        match t {
            Tok::Start(name, a) => match local_name(name) {
                "tr" => {
                    rows.push(Vec::new());
                    depth += 1;
                }
                "tc" => {
                    let merged = a.get("hMerge").is_some_and(|v| v == "1" || v == "true")
                        || a.get("vMerge").is_some_and(|v| v == "1" || v == "true");
                    let mut text = Vec::new();
                    let mut inner = 1;
                    while let Some(t) = r.next()? {
                        match t {
                            Tok::Start(n, _) => match local_name(n) {
                                "txBody" => text = text_body(r)?,
                                _ => inner += 1,
                            },
                            Tok::End(_) => {
                                inner -= 1;
                                if inner == 0 {
                                    break;
                                }
                            }
                            Tok::Text(_) => {}
                        }
                    }
                    let joined: Vec<String> = text
                        .into_iter()
                        .map(|p| p.text)
                        .filter(|t| !t.is_empty())
                        .collect();
                    if let Some(row) = rows.last_mut() {
                        row.push(if merged {
                            String::new()
                        } else {
                            joined.join("\n")
                        });
                    }
                }
                "tblPr" | "tblGrid" => r.skip()?,
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
    // 整表都是空的（版式用的表格）当作没有
    if rows.iter().all(|r| r.iter().all(String::is_empty)) {
        rows.clear();
    }
    Ok(rows)
}
