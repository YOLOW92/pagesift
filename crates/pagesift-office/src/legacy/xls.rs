//! Excel 97–2003（.xls，BIFF8）：记录流里取工作表名、共享字符串和单元格，每张表一个 Markdown 表格。
//! 日期按格式号判断后写成 ISO 日期，其他数字按原值。公式取缓存的结果。

use std::collections::{BTreeMap, HashMap, HashSet};

use crate::md::{self, Block};
use crate::ole::Ole;
use crate::{Converted, Error, Options, budget, check_deadline};

const BOF: u16 = 0x0809;
const EOF: u16 = 0x000A;
const FILEPASS: u16 = 0x002F;
const CONTINUE: u16 = 0x003C;
const BOUNDSHEET: u16 = 0x0085;
const SST: u16 = 0x00FC;
const LABELSST: u16 = 0x00FD;
const LABEL: u16 = 0x0204;
const NUMBER: u16 = 0x0203;
const RK: u16 = 0x027E;
const MULRK: u16 = 0x00BD;
const FORMULA: u16 = 0x0006;
const STRING: u16 = 0x0207;
const BOOLERR: u16 = 0x0205;
const FORMAT: u16 = 0x041E;
const XF: u16 = 0x00E0;
const DATEMODE: u16 = 0x0022;

fn u16_at(d: &[u8], i: usize) -> Option<u16> {
    Some(u16::from_le_bytes(d.get(i..i + 2)?.try_into().ok()?))
}

fn u32_at(d: &[u8], i: usize) -> Option<u32> {
    Some(u32::from_le_bytes(d.get(i..i + 4)?.try_into().ok()?))
}

struct Record<'a> {
    id: u16,
    body: &'a [u8],
}

/// 逐条读记录。CONTINUE 紧跟在 SST 后时由调用方接上。
fn records(stream: &[u8]) -> Vec<Record<'_>> {
    let mut out = Vec::new();
    let mut p = 0usize;
    while let (Some(id), Some(len)) = (u16_at(stream, p), u16_at(stream, p + 2)) {
        let end = (p + 4 + usize::from(len)).min(stream.len());
        out.push(Record {
            id,
            body: &stream[p + 4..end],
        });
        p = end;
    }
    out
}

/// 跨 CONTINUE 的读取器：字符串断在记录边界时，下一条记录开头多一个选项字节。
struct Parts<'a> {
    parts: Vec<&'a [u8]>,
    i: usize,
    p: usize,
}

impl Parts<'_> {
    fn left(&self) -> usize {
        self.parts
            .get(self.i)
            .map_or(0, |b| b.len().saturating_sub(self.p))
    }

    fn advance(&mut self) -> bool {
        if self.i + 1 < self.parts.len() {
            self.i += 1;
            self.p = 0;
            true
        } else {
            false
        }
    }

    fn bytes(&mut self, n: usize) -> Option<Vec<u8>> {
        let mut out = Vec::with_capacity(n);
        while out.len() < n {
            if self.left() == 0 && !self.advance() {
                return None;
            }
            let take = (n - out.len()).min(self.left());
            let part = self.parts[self.i];
            out.extend_from_slice(&part[self.p..self.p + take]);
            self.p += take;
        }
        Some(out)
    }

    fn skip(&mut self, mut n: usize) -> Option<()> {
        while n > 0 {
            if self.left() == 0 && !self.advance() {
                return None;
            }
            let take = n.min(self.left());
            self.p += take;
            n -= take;
        }
        Some(())
    }

    /// XLUnicodeRichExtendedString。
    fn rich_string(&mut self) -> Option<String> {
        let head = self.bytes(3)?;
        let chars = usize::from(u16::from_le_bytes([head[0], head[1]]));
        let mut flags = head[2];
        let runs = if flags & 8 != 0 {
            let b = self.bytes(2)?;
            usize::from(u16::from_le_bytes([b[0], b[1]]))
        } else {
            0
        };
        let ext = if flags & 4 != 0 {
            let b = self.bytes(4)?;
            u32::from_le_bytes([b[0], b[1], b[2], b[3]]) as usize
        } else {
            0
        };
        let mut out = String::with_capacity(chars);
        let mut left = chars;
        while left > 0 {
            if self.left() == 0 {
                if !self.advance() {
                    return None;
                }
                // 新记录开头的选项字节决定后面按单字节还是双字节
                flags = self.bytes(1)?[0];
            }
            let wide = flags & 1 != 0;
            let width = if wide { 2 } else { 1 };
            let n = left.min(self.left() / width).max(1);
            let raw = self.bytes(n * width)?;
            if wide {
                let units: Vec<u16> = raw
                    .as_chunks::<2>()
                    .0
                    .iter()
                    .map(|c| u16::from_le_bytes([c[0], c[1]]))
                    .collect();
                out.push_str(&String::from_utf16_lossy(&units));
            } else {
                // 单字节是 UTF-16 高字节为 0 的压缩写法，即 Latin-1
                out.extend(raw.iter().map(|&b| char::from(b)));
            }
            left -= n;
        }
        self.skip(runs * 4)?;
        self.skip(ext)?;
        Some(out)
    }
}

/// 短字符串（BOUNDSHEET 的表名）：1 字节长度。
fn short_string(b: &[u8]) -> Option<String> {
    let chars = usize::from(*b.first()?);
    let wide = b.get(1)? & 1 != 0;
    Some(if wide {
        let raw = b.get(2..2 + chars * 2)?;
        let units: Vec<u16> = raw
            .as_chunks::<2>()
            .0
            .iter()
            .map(|c| u16::from_le_bytes([c[0], c[1]]))
            .collect();
        String::from_utf16_lossy(&units)
    } else {
        b.get(2..2 + chars)?
            .iter()
            .map(|&c| char::from(c))
            .collect()
    })
}

/// 单元格里的长字符串（LABEL、STRING）：2 字节长度。
fn long_string(b: &[u8]) -> Option<String> {
    let chars = usize::from(u16_at(b, 0)?);
    let wide = b.get(2)? & 1 != 0;
    Some(if wide {
        let raw = b.get(3..3 + chars * 2)?;
        let units: Vec<u16> = raw
            .as_chunks::<2>()
            .0
            .iter()
            .map(|c| u16::from_le_bytes([c[0], c[1]]))
            .collect();
        String::from_utf16_lossy(&units)
    } else {
        b.get(3..3 + chars)?
            .iter()
            .map(|&c| char::from(c))
            .collect()
    })
}

/// RK：低 2 位是标志（bit0 除以 100，bit1 整数），其余是值。
pub(super) fn rk(v: u32) -> f64 {
    let n = if v & 2 != 0 {
        f64::from((v as i32) >> 2)
    } else {
        f64::from_bits(u64::from(v & 0xFFFF_FFFC) << 32)
    };
    if v & 1 != 0 { n / 100.0 } else { n }
}

pub(super) fn number_text(v: f64, is_date: bool) -> String {
    if is_date && (1.0..2_958_466.0).contains(&v) {
        return date_text(v);
    }
    if v == v.trunc() && v.abs() < 1e15 {
        format!("{}", v as i64)
    } else {
        format!("{v}")
    }
}

/// 1900 日期系统的序列号转 ISO（Excel 把 1900 当闰年，60 之后减一天）。
fn date_text(v: f64) -> String {
    let serial = v.trunc() as i64;
    let days = serial - 25_569 + i64::from(serial < 60); // 1970-01-01 是 25569
    let z = days + 719_468;
    let era = z.div_euclid(146_097);
    let doe = z.rem_euclid(146_097);
    let yoe = (doe - doe / 1460 + doe / 36_524 - doe / 146_096) / 365;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let d = doy - (153 * mp + 2) / 5 + 1;
    let m = if mp < 10 { mp + 3 } else { mp - 9 };
    let y = yoe + era * 400 + i64::from(m <= 2);
    let secs = ((v - v.trunc()) * 86_400.0).round() as i64;
    if secs == 0 {
        format!("{y:04}-{m:02}-{d:02}")
    } else {
        format!(
            "{y:04}-{m:02}-{d:02} {:02}:{:02}:{:02}",
            secs / 3600,
            secs % 3600 / 60,
            secs % 60
        )
    }
}

/// 内置日期格式号和自定义格式里有日期记号的。
pub(super) fn is_date_format(id: u16, custom: Option<&str>) -> bool {
    if matches!(id, 14..=22 | 27..=36 | 45..=47 | 50..=58) {
        return true;
    }
    let Some(f) = custom else { return false };
    let mut in_quote = false;
    let mut in_bracket = false;
    for c in f.chars() {
        match c {
            '"' => in_quote = !in_quote,
            '[' => in_bracket = true,
            ']' => in_bracket = false,
            'y' | 'Y' | 'd' | 'D' | 'h' | 'H' | 's' | 'S' | 'm' | 'M'
                if !in_quote && !in_bracket =>
            {
                return true;
            }
            _ => {}
        }
    }
    false
}

pub(super) type Sheet = BTreeMap<u32, BTreeMap<u32, String>>;

pub(crate) fn convert(bytes: &[u8], opts: &Options<'_>) -> Result<Converted, Error> {
    let ole = Ole::open(bytes)?;
    let entry = ole
        .children(None)
        .into_iter()
        .find(|e| {
            e.is_stream
                && (e.name.eq_ignore_ascii_case("Workbook") || e.name.eq_ignore_ascii_case("Book"))
        })
        .ok_or_else(|| Error::Malformed("no Workbook stream".into()))?;
    let stream = ole.read(entry)?;
    let recs = records(&stream);
    let mut names = Vec::new();
    let mut strings: Vec<String> = Vec::new();
    let mut formats: HashMap<u16, String> = HashMap::new();
    let mut xf_formats: Vec<u16> = Vec::new();
    let mut date1904 = false;
    // 第一段（工作簿全局）
    let mut i = 0usize;
    while i < recs.len() {
        let r = &recs[i];
        match r.id {
            FILEPASS => return Err(Error::Encrypted),
            BOUNDSHEET => {
                // 只要工作表（类型 0），图表表和宏表没有单元格
                if r.body.get(5) == Some(&0)
                    && let Some(n) = r.body.get(6..).and_then(short_string)
                {
                    names.push(n);
                }
            }
            FORMAT => {
                if let (Some(id), Some(s)) =
                    (u16_at(r.body, 0), r.body.get(2..).and_then(long_string))
                {
                    formats.insert(id, s);
                }
            }
            XF => xf_formats.push(u16_at(r.body, 2).unwrap_or(0)),
            DATEMODE => date1904 = u16_at(r.body, 0) == Some(1),
            SST => {
                let mut parts = vec![r.body.get(8..).unwrap_or(&[])];
                let mut j = i + 1;
                while recs.get(j).is_some_and(|c| c.id == CONTINUE) {
                    parts.push(recs[j].body);
                    j += 1;
                }
                let total = u32_at(r.body, 4).unwrap_or(0).min(1 << 24) as usize;
                let mut reader = Parts { parts, i: 0, p: 0 };
                for _ in 0..total {
                    match reader.rich_string() {
                        Some(s) => strings.push(s),
                        None => break,
                    }
                }
                i = j;
                continue;
            }
            EOF => {
                i += 1;
                break;
            }
            _ => {}
        }
        i += 1;
    }
    let dates: HashSet<u16> = xf_formats
        .iter()
        .enumerate()
        .filter(|(_, id)| is_date_format(**id, formats.get(id).map(String::as_str)))
        .map(|(x, _)| x as u16)
        .collect();
    let offset = if date1904 { 1462.0 } else { 0.0 };
    let num = |v: f64, xf: u16| {
        if dates.contains(&xf) {
            number_text(v + offset, true)
        } else {
            number_text(v, false)
        }
    };
    let max_cells = opts.limits.max_cells;
    let mut cells = 0usize;
    let mut truncated = false;
    let mut blocks = Vec::new();
    let mut sheet_index = 0usize;
    let mut sheet: Sheet = BTreeMap::new();
    let mut in_sheet = false;
    // 工作表里嵌的图表有自己的 BOF/EOF：只有第 1 层是单元格
    let mut depth = 0usize;
    let mut pending: Option<(u32, u32)> = None;
    let mut steps = 0usize;
    for r in &recs[i.min(recs.len())..] {
        steps += 1;
        if steps.is_multiple_of(4096) {
            check_deadline(opts)?;
        }
        match r.id {
            BOF => {
                // 上一张表缺 EOF 时，新工作表的 BOF 也算结束：工作表不会嵌在工作表里
                if in_sheet && u16_at(r.body, 2) == Some(0x10) {
                    let name = names.get(sheet_index).cloned().unwrap_or_default();
                    push_sheet(
                        &name,
                        std::mem::take(&mut sheet),
                        opts.limits.max_columns,
                        &mut blocks,
                    );
                    sheet_index += 1;
                    depth = 0;
                }
                if depth == 0 {
                    in_sheet = u16_at(r.body, 2) == Some(0x10);
                    sheet = BTreeMap::new();
                }
                depth += 1;
            }
            EOF => {
                depth = depth.saturating_sub(1);
                if depth > 0 {
                    continue;
                }
                if in_sheet {
                    let name = names.get(sheet_index).cloned().unwrap_or_default();
                    push_sheet(
                        &name,
                        std::mem::take(&mut sheet),
                        opts.limits.max_columns,
                        &mut blocks,
                    );
                    sheet_index += 1;
                }
                in_sheet = false;
            }
            _ if !in_sheet || depth != 1 => {}
            _ => {
                let mut put = |row: u32, col: u32, text: String| {
                    if cells >= max_cells {
                        truncated = true;
                        return;
                    }
                    cells += 1;
                    budget::spend(text.len());
                    sheet.entry(row).or_default().insert(col, text);
                };
                let row = u32::from(u16_at(r.body, 0).unwrap_or(0));
                let col = u32::from(u16_at(r.body, 2).unwrap_or(0));
                let xf = u16_at(r.body, 4).unwrap_or(0);
                match r.id {
                    LABELSST => {
                        if let Some(s) = u32_at(r.body, 6).and_then(|x| strings.get(x as usize)) {
                            put(row, col, s.clone());
                        }
                    }
                    LABEL => {
                        if let Some(s) = r.body.get(6..).and_then(long_string) {
                            put(row, col, s);
                        }
                    }
                    NUMBER => {
                        if let Some(b) = r.body.get(6..14).and_then(|b| b.try_into().ok()) {
                            put(row, col, num(f64::from_le_bytes(b), xf));
                        }
                    }
                    RK => {
                        if let Some(v) = u32_at(r.body, 6) {
                            put(row, col, num(rk(v), xf));
                        }
                    }
                    MULRK => {
                        // row、firstCol、(xf, rk)…、lastCol
                        let items = r.body.get(4..r.body.len().saturating_sub(2)).unwrap_or(&[]);
                        for (k, item) in items.as_chunks::<6>().0.iter().enumerate() {
                            let x = u16::from_le_bytes([item[0], item[1]]);
                            let v = u32::from_le_bytes([item[2], item[3], item[4], item[5]]);
                            put(row, col + k as u32, num(rk(v), x));
                        }
                    }
                    BOOLERR => {
                        if r.body.get(7) == Some(&0) {
                            let t = if r.body.get(6) == Some(&1) {
                                "TRUE"
                            } else {
                                "FALSE"
                            };
                            put(row, col, t.to_owned());
                        }
                    }
                    FORMULA => {
                        let v = r.body.get(6..14).unwrap_or(&[]);
                        if v.len() == 8 && v[6] == 0xFF && v[7] == 0xFF {
                            match v[0] {
                                // 字符串结果在紧跟的 STRING 记录里
                                0 => pending = Some((row, col)),
                                1 => put(
                                    row,
                                    col,
                                    if v[2] == 1 { "TRUE" } else { "FALSE" }.to_owned(),
                                ),
                                _ => {}
                            }
                        } else if let Ok(b) = <[u8; 8]>::try_from(v) {
                            put(row, col, num(f64::from_le_bytes(b), xf));
                        }
                    }
                    STRING => {
                        if let (Some((rw, cl)), Some(s)) = (pending.take(), long_string(r.body)) {
                            put(rw, cl, s);
                        }
                    }
                    _ => {}
                }
            }
        }
    }
    if in_sheet && !sheet.is_empty() {
        let name = names.get(sheet_index).cloned().unwrap_or_default();
        push_sheet(
            &name,
            std::mem::take(&mut sheet),
            opts.limits.max_columns,
            &mut blocks,
        );
        sheet_index += 1;
    }
    let mut warnings = Vec::new();
    let mut title_meta = Converted::default();
    super::props::apply(&ole, &mut title_meta);
    if truncated {
        warnings.push(format!("cells_truncated: stopped at {max_cells} cells"));
    }
    Ok(Converted {
        markdown: if blocks.is_empty() {
            String::new()
        } else {
            md::render(&blocks)
        },
        title: title_meta.title,
        metadata: title_meta.metadata,
        warnings,
        parts: sheet_index,
    })
}

pub(super) fn push_sheet(name: &str, sheet: Sheet, max_columns: usize, blocks: &mut Vec<Block>) {
    if !name.is_empty() {
        blocks.push(Block::Heading(2, md::escape(name)));
    }
    if sheet.is_empty() {
        return;
    }
    // 从最左边有内容的列开始，行内空格保留；行和行之间的空行不展开
    let first = sheet
        .values()
        .filter_map(|r| r.keys().next())
        .min()
        .copied()
        .unwrap_or(0);
    let last = sheet
        .values()
        .filter_map(|r| r.keys().next_back())
        .max()
        .copied()
        .unwrap_or(0);
    let width = ((last - first + 1) as usize).min(max_columns);
    let rows = sheet
        .into_values()
        .map(|cols| {
            let mut row = vec![String::new(); width];
            for (c, v) in cols {
                if let Some(slot) = row.get_mut((c - first) as usize) {
                    *slot = md::escape(&md::squeeze(v.trim()));
                }
            }
            row
        })
        .collect();
    blocks.push(Block::Table(rows));
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn rk_numbers() {
        // 整数 5
        assert_eq!(rk(5 << 2 | 2), 5.0);
        // 整数除以 100：123 → 1.23
        assert_eq!(rk(123 << 2 | 3), 1.23);
        // 浮点高 30 位
        let bits = (1.5f64.to_bits() >> 32) as u32;
        assert_eq!(rk(bits), 1.5);
    }

    #[test]
    fn dates_and_numbers() {
        assert_eq!(number_text(42.0, false), "42");
        assert_eq!(number_text(1.25, false), "1.25");
        assert_eq!(number_text(45_292.0, true), "2024-01-01");
        assert_eq!(number_text(45_292.5, true), "2024-01-01 12:00:00");
        assert!(is_date_format(14, None));
        assert!(is_date_format(200, Some("yyyy-mm-dd")));
        assert!(!is_date_format(200, Some("0.00\"days\"")));
        assert!(!is_date_format(1, None));
    }

    #[test]
    fn strings_cross_continue_records() {
        // 3 个字符，第一条记录只放下 1 个；CONTINUE 开头的选项字节换成双字节
        let parts: Vec<&[u8]> = vec![&[3, 0, 0, b'a'], &[1, 0x2D, 0x4E, 0x87, 0x65]];
        let mut p = Parts { parts, i: 0, p: 0 };
        assert_eq!(p.rich_string().as_deref(), Some("a\u{4e2d}\u{6587}"));
        // 数据不够时返回 None，不崩溃
        let parts: Vec<&[u8]> = vec![&[9, 0, 0, b'a']];
        assert_eq!(Parts { parts, i: 0, p: 0 }.rich_string(), None);
    }

    #[test]
    fn short_and_long_strings() {
        assert_eq!(short_string(&[2, 0, b'h', b'i']).as_deref(), Some("hi"));
        assert_eq!(
            short_string(&[1, 1, 0x2D, 0x4E]).as_deref(),
            Some("\u{4e2d}")
        );
        assert_eq!(long_string(&[1, 0, 0, b'x']).as_deref(), Some("x"));
        assert_eq!(long_string(&[9, 0, 0, b'x']), None);
    }
}
