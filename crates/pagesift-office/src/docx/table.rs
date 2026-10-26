//! 表格（ECMA-376 17.4）：行、单元格、合并。

use super::Ctx;
use super::body::paragraph;
use crate::Error;
use crate::md::Block;
use crate::xml::{Reader, Tok, local_name};

/// 一个单元格：里面的块（段落、内层表格）和横向跨的列数。纵向合并的续格没有块。
struct Cell {
    blocks: Vec<Block>,
    span: usize,
}

/// 表格：合并单元格展开（横向合并补空格子，纵向合并的下面几格留空）。
///
/// 单元格里还有表格的，外层是排版用的（Word 里常见：外层表摆位置，内层才是数据表），
/// 不输出成表：按行、按单元格的顺序把里面的块原样输出，内层表格照常成表（同样的规则逐层适用）。
/// 外层压成一张表的话，内层的行列只能挤成单元格里的几行文字，数据表的结构就丢了。
pub(super) fn table(r: &mut Reader<'_>, ctx: &mut Ctx<'_>) -> Result<Vec<Block>, Error> {
    let mut rows: Vec<Vec<Cell>> = Vec::new();
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
    let layout = rows
        .iter()
        .flatten()
        .any(|c| c.blocks.iter().any(|b| matches!(b, Block::Table(_))));
    if layout {
        return Ok(rows.into_iter().flatten().flat_map(|c| c.blocks).collect());
    }
    let mut grid = Vec::with_capacity(rows.len());
    for row in rows {
        let mut cells = Vec::with_capacity(row.len());
        for c in row {
            let lines: Vec<String> = c
                .blocks
                .iter()
                .map(flat)
                .filter(|l| !l.trim().is_empty())
                .collect();
            cells.push(lines.join("\n"));
            cells.extend(std::iter::repeat_n(String::new(), c.span.saturating_sub(1)));
        }
        grid.push(cells);
    }
    Ok(if grid.is_empty() {
        Vec::new()
    } else {
        vec![Block::Table(grid)]
    })
}

fn empty(n: usize) -> impl Iterator<Item = Cell> {
    std::iter::repeat_with(|| Cell {
        blocks: Vec::new(),
        span: 1,
    })
    .take(n)
}

fn row(r: &mut Reader<'_>, ctx: &mut Ctx<'_>) -> Result<Vec<Cell>, Error> {
    let mut cells = Vec::new();
    let mut depth = 1;
    let mut after = 0;
    while let Some(t) = r.next()? {
        match t {
            Tok::Start(name, a) => match local_name(name) {
                "gridBefore" => {
                    let n: usize = a.get("val").and_then(|v| v.parse().ok()).unwrap_or(0);
                    cells.extend(empty(n.min(64)));
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
                "tc" => cells.push(cell(r, ctx)?),
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
    cells.extend(empty(after));
    Ok(cells)
}

fn cell(r: &mut Reader<'_>, ctx: &mut Ctx<'_>) -> Result<Cell, Error> {
    let mut span = 1usize;
    let mut merged = false;
    let mut blocks = Vec::new();
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
                "p" => paragraph(r, ctx, &mut blocks)?,
                "tbl" => blocks.extend(table(r, ctx)?),
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
        blocks.clear();
    }
    Ok(Cell { blocks, span })
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
