//! 数据透视表（ECMA-376 18.10）的数字格式：透视表数据区的单元格在工作表里常常不带格式
//! （样式是“常规”），Excel 显示时用透视表定义里值字段的格式。这里只读格式，不重算透视表。
//!
//! 值字段的格式：`dataField/@numFmtId`，没有时用它对应的 `pivotField/@numFmtId`。
//! 数据区是 `location/@ref` 去掉前 `firstDataRow` 行、前 `firstDataCol` 列。
//! 有多个值字段时，`colFields` 里的 `field x="-2"` 表示值字段排在列上：第 j 列的值字段是
//! `colItems` 第 j 项的 `@i`；排在行上（`rowFields` 里有 -2）时按 `rowItems` 逐行对应。

use super::sheet::cell_ref;
use crate::Error;
use crate::opc::Rels;
use crate::xml::{Reader, Tok, local_name};
use crate::zip::Zip;

/// 一块区域（行 r0..=r1，列 c0..=c1，从 1 起）和它的数字格式 id。
#[derive(Debug, Clone, PartialEq)]
pub(super) struct Area {
    rows: (u32, u32),
    cols: (u32, u32),
    pub fmt: u32,
}

/// 某个单元格落在哪块透视表数据区里，返回那里的格式 id。
pub(super) fn format_at(areas: &[Area], row: u32, col: u32) -> Option<u32> {
    areas
        .iter()
        .find(|a| (a.rows.0..=a.rows.1).contains(&row) && (a.cols.0..=a.cols.1).contains(&col))
        .map(|a| a.fmt)
}

/// 工作表 `sheet` 关联的所有透视表的数据区格式。
pub(super) fn areas(zip: &Zip<'_>, sheet: &str) -> Result<Vec<Area>, Error> {
    let mut out = Vec::new();
    let rels = Rels::of(zip, sheet)?;
    for rel in rels.all("pivotTable") {
        if let Some(xml) = zip.read_text(&rel.target)? {
            out.extend(definition(&xml)?);
        }
    }
    Ok(out)
}

/// 一个 `pivotTableDefinition` 的数据区格式。
fn definition(xml: &str) -> Result<Vec<Area>, Error> {
    let mut loc = None;
    let (mut first_row, mut first_col) = (0u32, 0u32);
    let mut field_fmt: Vec<Option<u32>> = Vec::new();
    let mut data: Vec<(usize, Option<u32>)> = Vec::new();
    // 值字段在列上还是行上（-2 出现在哪个轴）
    let (mut on_cols, mut on_rows) = (false, false);
    let mut col_items: Vec<usize> = Vec::new();
    let mut row_items: Vec<usize> = Vec::new();
    let mut axis = "";
    let mut r = Reader::new(xml);
    while let Some(t) = r.next()? {
        match t {
            Tok::Start(name, a) => match local_name(name) {
                "location" => {
                    loc = a.get("ref").and_then(|v| range(&v));
                    first_row = num(a.get("firstDataRow").as_deref()).unwrap_or(0);
                    first_col = num(a.get("firstDataCol").as_deref()).unwrap_or(0);
                }
                "pivotField" => {
                    field_fmt.push(num(a.get("numFmtId").as_deref()));
                    // 跳过下面的 items：那里的 item 不是 rowItems/colItems 的项
                    r.skip()?;
                }
                "dataField" => data.push((
                    num(a.get("fld").as_deref()).unwrap_or(0) as usize,
                    num(a.get("numFmtId").as_deref()),
                )),
                "rowFields" | "colFields" | "rowItems" | "colItems" => axis = local_name(name),
                "field" if num_i(a.get("x").as_deref()) == Some(-2) => match axis {
                    "colFields" => on_cols = true,
                    "rowFields" => on_rows = true,
                    _ => {}
                },
                "i" => {
                    let k = num(a.get("i").as_deref()).unwrap_or(0) as usize;
                    // 一项可以用 @r 表示和上一项共用前几层，但值字段下标总写在 @i 上
                    match axis {
                        "colItems" => col_items.push(k),
                        "rowItems" => row_items.push(k),
                        _ => {}
                    }
                }
                "extLst" | "formats" | "conditionalFormats" | "pivotTableStyleInfo" => r.skip()?,
                _ => {}
            },
            Tok::End(name) => {
                if matches!(
                    local_name(name),
                    "rowFields" | "colFields" | "rowItems" | "colItems"
                ) {
                    axis = "";
                }
            }
            Tok::Text(_) => {}
        }
    }
    let Some(((r0, c0), (r1, c1))) = loc else {
        return Ok(Vec::new());
    };
    let fmts: Vec<Option<u32>> = data
        .iter()
        .map(|&(fld, own)| own.or_else(|| field_fmt.get(fld).copied().flatten()))
        .collect();
    let (dr, dc) = (r0.saturating_add(first_row), c0.saturating_add(first_col));
    if fmts.is_empty() || dr > r1 || dc > c1 {
        return Ok(Vec::new());
    }
    let mut out = Vec::new();
    if fmts.len() == 1 || !(on_cols || on_rows) {
        if let Some(f) = fmts[0] {
            out.push(Area {
                rows: (dr, r1),
                cols: (dc, c1),
                fmt: f,
            });
        }
    } else if on_cols {
        for (j, &k) in col_items.iter().enumerate() {
            let c = dc.saturating_add(j as u32);
            if let (true, Some(Some(f))) = (c <= c1, fmts.get(k)) {
                out.push(Area {
                    rows: (dr, r1),
                    cols: (c, c),
                    fmt: *f,
                });
            }
        }
    } else {
        for (j, &k) in row_items.iter().enumerate() {
            let rr = dr.saturating_add(j as u32);
            if let (true, Some(Some(f))) = (rr <= r1, fmts.get(k)) {
                out.push(Area {
                    rows: (rr, rr),
                    cols: (dc, c1),
                    fmt: *f,
                });
            }
        }
    }
    Ok(out)
}

fn num(v: Option<&str>) -> Option<u32> {
    v?.trim().parse().ok()
}

fn num_i(v: Option<&str>) -> Option<i64> {
    v?.trim().parse().ok()
}

/// `B5:E18` → ((5, 2), (18, 5))；单个单元格的引用也行。
fn range(s: &str) -> Option<((u32, u32), (u32, u32))> {
    let (a, b) = s.split_once(':').unwrap_or((s, s));
    let (a, b) = (cell_ref(a.trim())?, cell_ref(b.trim())?);
    Some(((a.0.min(b.0), a.1.min(b.1)), (a.0.max(b.0), a.1.max(b.1))))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn data_fields_on_columns_get_their_formats() {
        let xml = r#"<pivotTableDefinition><location ref="B5:E8" firstHeaderRow="0" firstDataRow="1" firstDataCol="1"/>
<pivotFields count="4"><pivotField axis="axisRow"><items count="2"><item x="0"/><item t="default"/></items></pivotField>
<pivotField dataField="1" numFmtId="176"/><pivotField dataField="1"/><pivotField dataField="1" numFmtId="3"/></pivotFields>
<rowFields count="1"><field x="0"/></rowFields><rowItems count="3"><i><x/></i><i><x v="1"/></i><i t="grand"><x/></i></rowItems>
<colFields count="1"><field x="-2"/></colFields><colItems count="3"><i><x/></i><i i="1"><x v="1"/></i><i i="2"><x v="2"/></i></colItems>
<dataFields count="3"><dataField name="a" fld="1"/><dataField name="b" fld="2" numFmtId="10"/><dataField name="c" fld="3"/></dataFields></pivotTableDefinition>"#;
        let areas = definition(xml).unwrap();
        assert_eq!(format_at(&areas, 6, 3), Some(176));
        assert_eq!(format_at(&areas, 8, 4), Some(10));
        assert_eq!(format_at(&areas, 7, 5), Some(3));
        // 表头行、行标签列不在数据区
        assert_eq!(format_at(&areas, 5, 3), None);
        assert_eq!(format_at(&areas, 6, 2), None);
    }

    #[test]
    fn single_data_field_covers_whole_data_area() {
        let xml = r#"<pivotTableDefinition><location ref="A3:C6" firstHeaderRow="1" firstDataRow="2" firstDataCol="1"/>
<pivotFields><pivotField axis="axisRow"/><pivotField dataField="1" numFmtId="4"/></pivotFields>
<dataFields count="1"><dataField fld="1"/></dataFields></pivotTableDefinition>"#;
        let areas = definition(xml).unwrap();
        assert_eq!(format_at(&areas, 5, 2), Some(4));
        assert_eq!(format_at(&areas, 6, 3), Some(4));
        assert_eq!(format_at(&areas, 4, 2), None);
    }
}
