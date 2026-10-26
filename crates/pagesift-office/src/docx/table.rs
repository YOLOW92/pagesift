//! 表格（ECMA-376 17.4）：行、单元格、合并。

use super::Ctx;
use super::body::paragraph;
use crate::Error;
use crate::md::Block;
use crate::xml::{Reader, Tok, local_name};

/// 表格：合并单元格展开（横向合并补空格子，纵向合并的下面几格留空），嵌套表格压成文字。
pub(super) fn table(r: &mut Reader<'_>, ctx: &mut Ctx<'_>) -> Result<Vec<Vec<String>>, Error> {
    let mut rows = Vec::new();
    let mut depth = 1;
    while let Some(t) = r.next()? {
        match t {
            Tok::Start(name, _) => match local_name(name) {
                "tr" => rows.push(row(r, ctx)?),
                "tblPr" | "tblGrid" | "tblPrEx" => r.skip()?,
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
    Ok(rows)
}

fn row(r: &mut Reader<'_>, ctx: &mut Ctx<'_>) -> Result<Vec<String>, Error> {
    let mut cells = Vec::new();
    let mut depth = 1;
    let mut after = 0;
    while let Some(t) = r.next()? {
        match t {
            Tok::Start(name, a) => match local_name(name) {
                "gridBefore" => {
                    let n: usize = a.get("val").and_then(|v| v.parse().ok()).unwrap_or(0);
                    cells.extend(std::iter::repeat_n(String::new(), n.min(64)));
                    depth += 1;
                }
                "gridAfter" => {
                    after = a
                        .get("val")
                        .and_then(|v| v.parse().ok())
                        .unwrap_or(0usize)
                        .min(64);
                    depth += 1;
                }
                "tc" => {
                    let (text, span) = cell(r, ctx)?;
                    cells.push(text);
                    cells.extend(std::iter::repeat_n(String::new(), span.saturating_sub(1)));
                }
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
    cells.extend(std::iter::repeat_n(String::new(), after));
    Ok(cells)
}

/// 一个单元格：文字（段落之间换行）和横向跨的列数。纵向合并的续格内容为空。
fn cell(r: &mut Reader<'_>, ctx: &mut Ctx<'_>) -> Result<(String, usize), Error> {
    let mut span = 1usize;
    let mut merged = false;
    let mut lines = Vec::new();
    let mut depth = 1;
    while let Some(t) = r.next()? {
        match t {
            Tok::Start(name, a) => match local_name(name) {
                "gridSpan" => {
                    span = a
                        .get("val")
                        .and_then(|v| v.parse().ok())
                        .unwrap_or(1)
                        .clamp(1, 64);
                    depth += 1;
                }
                "vMerge" => {
                    merged = a.get("val").is_none_or(|v| v != "restart");
                    depth += 1;
                }
                "p" => {
                    let mut blocks = Vec::new();
                    paragraph(r, ctx, &mut blocks)?;
                    lines.extend(blocks.iter().map(flat));
                }
                "tbl" => {
                    for row in table(r, ctx)? {
                        let joined: Vec<&str> = row
                            .iter()
                            .map(String::as_str)
                            .filter(|c| !c.is_empty())
                            .collect();
                        lines.push(joined.join(" "));
                    }
                }
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
    if merged {
        return Ok((String::new(), span));
    }
    let lines: Vec<String> = lines.into_iter().filter(|l| !l.trim().is_empty()).collect();
    Ok((lines.join("\n"), span))
}

/// 单元格里的块压成一行文字。
fn flat(b: &Block) -> String {
    match b {
        Block::Heading(_, t) | Block::Para(t) | Block::Raw(t) => t.replace("\\\n", "\n"),
        Block::Item {
            ordered,
            number,
            text,
            ..
        } => {
            if *ordered {
                format!("{number}. {text}")
            } else {
                format!("- {text}")
            }
        }
        Block::Table(rows) => rows
            .iter()
            .map(|r| r.join(" "))
            .collect::<Vec<_>>()
            .join("\n"),
    }
}
