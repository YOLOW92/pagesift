//! Excel 二进制工作簿（.xlsb，MS-XLSB）：ZIP 外壳和 XLSX 一样，部件里是 BIFF12 记录。
//! 记录号和长度都是变长整数；工作表名、共享字符串、单元格各有自己的记录。

use std::collections::{BTreeMap, HashMap, HashSet};

use super::xls::{Sheet, is_date_format, number_text, push_sheet, rk};
use crate::md::{self, Block};
use crate::opc::{self, Rels};
use crate::zip::Zip;
use crate::{Converted, Error, Options, budget, check_deadline};

const ROW: u32 = 0;
const CELL_RK: u32 = 2;
const CELL_BOOL: u32 = 4;
const CELL_REAL: u32 = 5;
const CELL_ST: u32 = 6;
const CELL_ISST: u32 = 7;
const FMLA_STRING: u32 = 8;
const FMLA_NUM: u32 = 9;
const FMLA_BOOL: u32 = 10;
const SST_ITEM: u32 = 19;
const FMT: u32 = 44;
const XF: u32 = 47;
const WB_PROP: u32 = 153;
const BUNDLE_SH: u32 = 156;
const BEGIN_CELL_XFS: u32 = 617;
const END_CELL_XFS: u32 = 618;

/// 记录迭代器。声明的长度超过剩余数据时停下。
struct Records<'a> {
    data: &'a [u8],
    pos: usize,
}

impl<'a> Records<'a> {
    fn varint(&mut self, max: usize) -> Option<u32> {
        let mut v = 0u32;
        for i in 0..max {
            let b = *self.data.get(self.pos)?;
            self.pos += 1;
            v |= u32::from(b & 0x7F) << (7 * i);
            if b & 0x80 == 0 {
                return Some(v);
            }
        }
        Some(v)
    }
}

impl<'a> Iterator for Records<'a> {
    type Item = (u32, &'a [u8]);

    fn next(&mut self) -> Option<Self::Item> {
        let id = self.varint(4)?;
        let len = self.varint(4)? as usize;
        let body = self.data.get(self.pos..self.pos.checked_add(len)?)?;
        self.pos += len;
        Some((id, body))
    }
}

fn records(data: &[u8]) -> Records<'_> {
    Records { data, pos: 0 }
}

fn u32_at(d: &[u8], i: usize) -> Option<u32> {
    Some(u32::from_le_bytes(d.get(i..i + 4)?.try_into().ok()?))
}

/// XLWideString：字符数加 UTF-16。可空串的字符数是全 1，当空串。返回文字和读完的位置。
fn wide(d: &[u8], at: usize) -> Option<(String, usize)> {
    let n = u32_at(d, at)?;
    if n == u32::MAX {
        return Some((String::new(), at + 4));
    }
    let end = (at + 4).checked_add((n as usize).checked_mul(2)?)?;
    let raw = d.get(at + 4..end)?;
    let units: Vec<u16> = raw
        .as_chunks::<2>()
        .0
        .iter()
        .map(|c| u16::from_le_bytes([c[0], c[1]]))
        .collect();
    Some((String::from_utf16_lossy(&units), end))
}

struct Book {
    sheets: Vec<(String, String)>,
    date1904: bool,
}

fn workbook(data: &[u8]) -> Book {
    let mut book = Book {
        sheets: Vec::new(),
        date1904: false,
    };
    for (id, b) in records(data) {
        match id {
            WB_PROP => book.date1904 = u32_at(b, 0).is_some_and(|f| f & 1 != 0),
            BUNDLE_SH => {
                if let Some((rid, at)) = wide(b, 8)
                    && let Some((name, _)) = wide(b, at)
                {
                    book.sheets.push((name, rid));
                }
            }
            _ => {}
        }
    }
    book
}

fn shared_strings(data: &[u8]) -> Vec<String> {
    records(data)
        .filter(|(id, _)| *id == SST_ITEM)
        // RichStr：1 字节标志，后面是字符串
        .map(|(_, b)| wide(b, 1).map(|(s, _)| s).unwrap_or_default())
        .collect()
}

/// 用了日期格式的单元格样式号。
fn date_styles(data: &[u8]) -> HashSet<u32> {
    let mut custom: HashMap<u16, String> = HashMap::new();
    let mut xfs: Vec<u16> = Vec::new();
    let mut in_xfs = false;
    for (id, b) in records(data) {
        match id {
            FMT => {
                if b.len() >= 2
                    && let Some((s, _)) = wide(b, 2)
                {
                    custom.insert(u16::from_le_bytes([b[0], b[1]]), s);
                }
            }
            BEGIN_CELL_XFS => in_xfs = true,
            END_CELL_XFS => in_xfs = false,
            XF if in_xfs && b.len() >= 4 => xfs.push(u16::from_le_bytes([b[2], b[3]])),
            _ => {}
        }
    }
    xfs.iter()
        .enumerate()
        .filter(|(_, f)| is_date_format(**f, custom.get(f).map(String::as_str)))
        .map(|(i, _)| i as u32)
        .collect()
}

pub(crate) fn convert(zip: &Zip<'_>, opts: &Options<'_>) -> Result<Converted, Error> {
    let main = opc::main_part(zip, "xl/workbook.bin")?;
    let data = zip
        .read(&main)?
        .ok_or_else(|| Error::Malformed("no xl/workbook.bin".into()))?;
    let book = workbook(&data);
    let rels = Rels::of(zip, &main)?;
    let part = |kind: &str| -> Result<Vec<u8>, Error> {
        Ok(match rels.first(kind) {
            Some(r) => zip.read(&r.target)?.unwrap_or_default(),
            None => Vec::new(),
        })
    };
    let strings = shared_strings(&part("sharedStrings")?);
    let dates = date_styles(&part("styles")?);
    let offset = if book.date1904 { 1462.0 } else { 0.0 };
    let mut blocks: Vec<Block> = Vec::new();
    let mut warnings = Vec::new();
    let mut cells = 0usize;
    let max_cells = opts.limits.max_cells;
    for (name, rid) in &book.sheets {
        check_deadline(opts)?;
        let Some(rel) = rels.get(rid).filter(|r| r.kind == "worksheet") else {
            if !name.is_empty() {
                blocks.push(Block::Heading(2, md::escape(name)));
            }
            continue;
        };
        let Some(data) = zip.read(&rel.target)? else {
            continue;
        };
        let mut sheet: Sheet = BTreeMap::new();
        let mut row = 0u32;
        for (steps, (id, b)) in records(&data).enumerate() {
            if steps % 4096 == 0 {
                check_deadline(opts)?;
            }
            if id == ROW {
                row = u32_at(b, 0).unwrap_or(row);
                continue;
            }
            let (Some(col), Some(style)) = (u32_at(b, 0), u32_at(b, 4).map(|s| s & 0xFF_FFFF))
            else {
                continue;
            };
            let num = |v: f64| {
                if dates.contains(&style) {
                    number_text(v + offset, true)
                } else {
                    number_text(v, false)
                }
            };
            let text = match id {
                CELL_RK => u32_at(b, 8).map(|v| num(rk(v))),
                CELL_REAL | FMLA_NUM => b
                    .get(8..16)
                    .and_then(|v| v.try_into().ok())
                    .map(|v| num(f64::from_le_bytes(v))),
                CELL_ST | FMLA_STRING => wide(b, 8).map(|(s, _)| s),
                CELL_ISST => u32_at(b, 8).and_then(|i| strings.get(i as usize).cloned()),
                CELL_BOOL | FMLA_BOOL => b
                    .get(8)
                    .map(|v| if *v == 1 { "TRUE" } else { "FALSE" }.to_owned()),
                _ => None,
            };
            let Some(text) = text else { continue };
            if cells >= max_cells {
                warnings.push(format!("cells_truncated: stopped at {max_cells} cells"));
                break;
            }
            cells += 1;
            budget::spend(text.len());
            sheet.entry(row).or_default().insert(col, text);
        }
        push_sheet(name, sheet, opts.limits.max_columns, &mut blocks);
        if cells >= max_cells || budget::exhausted() {
            break;
        }
    }
    let mut out = Converted {
        markdown: if blocks.is_empty() {
            String::new()
        } else {
            md::render(&blocks)
        },
        warnings,
        parts: book.sheets.len(),
        ..Default::default()
    };
    opc::core_properties(zip, &mut out)?;
    Ok(out)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::Limits;

    fn rec(id: u32, body: &[u8]) -> Vec<u8> {
        fn var(mut v: u32, out: &mut Vec<u8>) {
            loop {
                let b = (v & 0x7F) as u8;
                v >>= 7;
                if v == 0 {
                    out.push(b);
                    return;
                }
                out.push(b | 0x80);
            }
        }
        let mut out = Vec::new();
        var(id, &mut out);
        var(body.len() as u32, &mut out);
        out.extend_from_slice(body);
        out
    }

    fn ws(s: &str) -> Vec<u8> {
        let units: Vec<u16> = s.encode_utf16().collect();
        let mut out = (units.len() as u32).to_le_bytes().to_vec();
        out.extend(units.iter().flat_map(|u| u.to_le_bytes()));
        out
    }

    fn cell(col: u32, style: u32, value: &[u8]) -> Vec<u8> {
        [col.to_le_bytes().as_slice(), &style.to_le_bytes(), value].concat()
    }

    #[test]
    fn reads_sheets_strings_numbers_and_dates() {
        let wb = [
            rec(WB_PROP, &0u32.to_le_bytes()),
            rec(BUNDLE_SH, &[vec![0u8; 8], ws("rId1"), ws("Data")].concat()),
        ]
        .concat();
        let sst = rec(SST_ITEM, &[vec![0u8], ws("\u{4e2d}\u{6587}")].concat());
        let styles = [
            rec(BEGIN_CELL_XFS, &[]),
            rec(XF, &[0, 0, 0, 0]),
            rec(XF, &[0, 0, 14, 0]),
            rec(END_CELL_XFS, &[]),
        ]
        .concat();
        let sheet = [
            rec(ROW, &0u32.to_le_bytes()),
            rec(CELL_ISST, &cell(0, 0, &0u32.to_le_bytes())),
            rec(CELL_ST, &cell(1, 0, &ws("inline"))),
            rec(ROW, &1u32.to_le_bytes()),
            rec(CELL_REAL, &cell(0, 1, &45_292f64.to_le_bytes())),
            rec(CELL_RK, &cell(1, 0, &(123u32 << 2 | 3).to_le_bytes())),
            rec(FMLA_BOOL, &cell(2, 0, &[1])),
        ]
        .concat();
        let rels = "<Relationships xmlns='http://schemas.openxmlformats.org/package/2006/relationships'>\
            <Relationship Id='rId1' Type='http://x/worksheet' Target='worksheets/sheet1.bin'/>\
            <Relationship Id='rId2' Type='http://x/sharedStrings' Target='sharedStrings.bin'/>\
            <Relationship Id='rId3' Type='http://x/styles' Target='styles.bin'/></Relationships>";
        let bytes = crate::zip::tests::build(&[
            ("xl/workbook.bin", &wb, false),
            ("xl/_rels/workbook.bin.rels", rels.as_bytes(), false),
            ("xl/sharedStrings.bin", &sst, false),
            ("xl/styles.bin", &styles, false),
            ("xl/worksheets/sheet1.bin", &sheet, false),
        ]);
        let zip = Zip::open(&bytes, &Limits::default()).unwrap();
        let d = convert(&zip, &Options::default()).unwrap();
        assert!(d.markdown.contains("## Data"), "{}", d.markdown);
        assert!(d.markdown.contains("\u{4e2d}\u{6587}"), "{}", d.markdown);
        assert!(d.markdown.contains("inline"), "{}", d.markdown);
        assert!(d.markdown.contains("2024-01-01"), "{}", d.markdown);
        assert!(d.markdown.contains("1.23"), "{}", d.markdown);
        assert!(d.markdown.contains("TRUE"), "{}", d.markdown);
    }

    #[test]
    fn truncated_records_stop() {
        assert_eq!(records(&[0x05, 0x7F, 1, 2]).count(), 0);
        assert_eq!(wide(&[9, 0, 0, 0, 1], 0), None);
    }
}
