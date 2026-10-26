//! 工作表（worksheet，ECMA-376 18.3）：流式读单元格，只留有内容的，最后排成表。

use super::Book;
use super::pivot::{Area, format_at};
use crate::xml::{Reader, Tok, local_name};
use crate::{Error, Options};

/// 表里连续这么多个空行以上就不再原样输出（稀疏的大表）。
const MAX_EMPTY_RUN: u32 = 1000;

/// 读一个工作表，返回表格（第一行是表头）。`budget` 是剩下可输出的单元格数，用完就截断并警告。
pub(super) fn read(
    xml: &str,
    book: &Book,
    pivots: &[Area],
    opts: &Options<'_>,
    budget: &mut usize,
    warnings: &mut Vec<String>,
) -> Result<Vec<Vec<String>>, Error> {
    let max_cols = opts.limits.max_columns as u32;
    let mut cells: Vec<(u32, u32, String)> = Vec::new();
    let mut row = 0u32;
    let mut col = 0u32;
    let mut dropped_cols = false;
    let mut truncated = false;
    // 隐藏的列（cols/col@hidden）和行（row@hidden）：用户在表格软件里看不到，不输出
    let mut hidden_cols: Vec<(u32, u32)> = Vec::new();
    let mut hidden_row = false;
    let mut hidden_rows: Vec<u32> = Vec::new();
    let mut r = Reader::new(xml);
    while let Some(t) = r.next()? {
        let Tok::Start(name, a) = t else { continue };
        match local_name(name) {
            "col" => {
                if flag(a.get("hidden").as_deref())
                    && let (Some(lo), Some(hi)) = (
                        a.get("min").and_then(|v| v.parse().ok()),
                        a.get("max").and_then(|v| v.parse().ok()),
                    )
                {
                    hidden_cols.push((lo, hi));
                }
            }
            "row" => {
                row = a.get("r").and_then(|v| v.parse().ok()).unwrap_or(row + 1);
                col = 0;
                hidden_row = flag(a.get("hidden").as_deref());
                if hidden_row {
                    hidden_rows.push(row);
                }
                if row.is_multiple_of(4096) {
                    crate::check_deadline(opts)?;
                }
            }
            "c" => {
                let (rr, cc) = a
                    .get("r")
                    .and_then(|v| cell_ref(&v))
                    .unwrap_or((row, col + 1));
                row = rr;
                col = cc;
                let kind = a.get("t").map(|v| v.into_owned()).unwrap_or_default();
                let style: usize = a.get("s").and_then(|v| v.parse().ok()).unwrap_or(0);
                let (v, inline) = cell_content(&mut r)?;
                if hidden_row || hidden_cols.iter().any(|&(lo, hi)| (lo..=hi).contains(&cc)) {
                    continue;
                }
                if cc > max_cols {
                    dropped_cols = true;
                    continue;
                }
                let text = match kind.as_str() {
                    "s" => v
                        .trim()
                        .parse::<usize>()
                        .ok()
                        .and_then(|i| book.shared.get(i))
                        .cloned()
                        .unwrap_or_default(),
                    "inlineStr" => inline,
                    "b" => {
                        if v.trim() == "1" {
                            "TRUE".into()
                        } else {
                            "FALSE".into()
                        }
                    }
                    "e" | "str" | "d" => v,
                    _ if v.is_empty() => String::new(),
                    _ => book.number(&v, style, format_at(pivots, rr, cc)),
                };
                if text.trim().is_empty() {
                    continue;
                }
                if cells.len() >= *budget {
                    truncated = true;
                    break;
                }
                cells.push((rr, cc, text));
            }
            "mergeCells" | "conditionalFormatting" | "dataValidations" | "extLst" => r.skip()?,
            _ => {}
        }
    }
    if dropped_cols {
        warnings.push(format!("columns beyond {max_cols} were dropped"));
    }
    if cells.is_empty() {
        return Ok(Vec::new());
    }
    // 去掉隐藏的行列后，后面的往前挪，表里不留空行空列
    if !hidden_cols.is_empty() || !hidden_rows.is_empty() {
        hidden_rows.sort_unstable();
        for c in &mut cells {
            let shift: u32 = hidden_cols
                .iter()
                .map(|&(lo, hi)| if hi < c.1 { hi - lo + 1 } else { 0 })
                .sum();
            c.1 -= shift.min(c.1 - 1);
            c.0 -= hidden_rows.partition_point(|&r| r < c.0) as u32;
        }
    }
    let r0 = cells.iter().map(|c| c.0).min().unwrap_or(1);
    let c0 = cells.iter().map(|c| c.1).min().unwrap_or(1);
    let c1 = cells.iter().map(|c| c.1).max().unwrap_or(1);
    let width = (c1 - c0 + 1) as usize;
    cells.sort_by_key(|c| (c.0, c.1));

    let mut grid: Vec<Vec<String>> = Vec::new();
    let mut last_row = r0;
    let mut skipped = 0u64;
    let mut cut = false;
    for (rr, cc, text) in cells {
        if grid.is_empty() || rr != last_row {
            let gap = if grid.is_empty() {
                0
            } else {
                rr - last_row - 1
            };
            if gap > MAX_EMPTY_RUN {
                skipped += u64::from(gap);
            } else {
                for _ in 0..gap {
                    if !push_row(&mut grid, width, budget) {
                        cut = true;
                        break;
                    }
                }
            }
            if cut || !push_row(&mut grid, width, budget) {
                cut = true;
                break;
            }
            last_row = rr;
        }
        if let Some(cell) = grid.last_mut().and_then(|g| g.get_mut((cc - c0) as usize)) {
            *cell = text;
        }
    }
    if skipped > 0 {
        warnings.push(format!(
            "skipped {skipped} empty rows inside a sparse sheet"
        ));
    }
    if truncated || cut {
        warnings.push(format!(
            "sheet truncated at the cell limit ({} cells)",
            opts.limits.max_cells
        ));
    }
    Ok(grid)
}

fn flag(v: Option<&str>) -> bool {
    matches!(v, Some("1" | "true"))
}

fn push_row(grid: &mut Vec<Vec<String>>, width: usize, budget: &mut usize) -> bool {
    if *budget < width {
        return false;
    }
    *budget -= width;
    grid.push(vec![String::new(); width]);
    true
}

/// 单元格里的值 `v` 和行内字符串 `is`（刚读到 `c` 的开始标签）。公式 `f` 不要。
fn cell_content(r: &mut Reader<'_>) -> Result<(String, String), Error> {
    let mut v = String::new();
    let mut inline = String::new();
    let mut depth = 1;
    while let Some(t) = r.next()? {
        match t {
            Tok::Start(n, _) => match local_name(n) {
                "v" => v = r.text_until_end()?,
                "t" => inline.push_str(&r.text_until_end()?),
                "f" | "rPh" | "extLst" => r.skip()?,
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
    Ok((v, inline))
}

/// `AB12` → (行 12, 列 28)。
pub(super) fn cell_ref(s: &str) -> Option<(u32, u32)> {
    let letters = s.bytes().take_while(u8::is_ascii_alphabetic).count();
    if letters == 0 || letters > 3 {
        return None;
    }
    let col = s[..letters].bytes().fold(0u32, |n, b| {
        n * 26 + u32::from(b.to_ascii_uppercase() - b'A' + 1)
    });
    let row = s[letters..].parse().ok()?;
    Some((row, col))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_cell_references() {
        assert_eq!(cell_ref("A1"), Some((1, 1)));
        assert_eq!(cell_ref("AB12"), Some((12, 28)));
        assert_eq!(cell_ref("XFD1048576"), Some((1_048_576, 16_384)));
        assert_eq!(cell_ref("12"), None);
    }
}
