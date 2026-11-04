//! dBASE / FoxPro（.dbf）：32 字节文件头，字段描述表以 0x0D 结束，后面是定长记录。
//! 字符集看文件头的语言驱动字节；能按 UTF-8 解的字段按 UTF-8。备注字段在单独的文件里，只留空格。

use encoding_rs::Encoding;

use crate::md::{self, Block};
use crate::{Converted, Error, Options, budget, check_deadline};

struct Field {
    name: String,
    kind: u8,
    start: usize,
    len: usize,
}

fn charset(ldid: u8) -> &'static Encoding {
    match ldid {
        0x4D | 0x7A => encoding_rs::GBK,
        0x4E | 0x79 => encoding_rs::EUC_KR,
        0x4F | 0x78 => encoding_rs::BIG5,
        0x13 | 0x7B => encoding_rs::SHIFT_JIS,
        0x64 | 0xC8 => encoding_rs::WINDOWS_1250,
        0x65 | 0x66 | 0xC9 => encoding_rs::WINDOWS_1251,
        0xCA => encoding_rs::WINDOWS_1254,
        0xCB => encoding_rs::WINDOWS_1253,
        0x7C => encoding_rs::WINDOWS_874,
        0x7D => encoding_rs::WINDOWS_1255,
        0x7E => encoding_rs::WINDOWS_1256,
        _ => encoding_rs::WINDOWS_1252,
    }
}

fn read_fields(bytes: &[u8], header: usize, rec: usize) -> Result<Vec<Field>, Error> {
    let mut fields = Vec::new();
    let mut i = 32usize;
    let mut start = 1usize; // 第一个字节是删除标记
    while i + 32 <= header {
        let d = &bytes[i..i + 32];
        if d[0] == 0x0D {
            break;
        }
        let name_end = d[..11].iter().position(|&b| b == 0).unwrap_or(11);
        let name = String::from_utf8_lossy(&d[..name_end]).trim().to_owned();
        let mut len = usize::from(d[16]);
        // Clipper 和 FoxPro 的长字符字段：小数位字节是长度的高位
        if d[11] == b'C' {
            len |= usize::from(d[17]) << 8;
        }
        fields.push(Field {
            name,
            kind: d[11],
            start,
            len,
        });
        start += len;
        i += 32;
    }
    if fields.is_empty() || start > rec {
        return Err(Error::Malformed("broken DBF field table".into()));
    }
    Ok(fields)
}

pub(crate) fn convert(bytes: &[u8], opts: &Options<'_>) -> Result<Converted, Error> {
    let bad = || Error::Malformed("not a DBF file".into());
    let head = bytes.get(..32).ok_or_else(bad)?;
    if !matches!(head[0] & 0x07, 2..=5)
        && !matches!(
            head[0],
            0x30 | 0x31 | 0x32 | 0x43 | 0x63 | 0x83 | 0x8B | 0xCB | 0xF5
        )
    {
        return Err(bad());
    }
    if head[15] == 1 {
        return Err(Error::Encrypted);
    }
    let count = u32::from_le_bytes([head[4], head[5], head[6], head[7]]) as usize;
    let header = usize::from(u16::from_le_bytes([head[8], head[9]]));
    let rec = usize::from(u16::from_le_bytes([head[10], head[11]]));
    if header < 33 || rec == 0 || header > bytes.len() {
        return Err(bad());
    }
    let fields = read_fields(bytes, header, rec)?;
    let enc = charset(head[29]);
    let mut rows = vec![
        fields
            .iter()
            .map(|f| md::escape(&f.name))
            .collect::<Vec<_>>(),
    ];
    let max_rows = opts.limits.max_cells / fields.len();
    let mut warnings = Vec::new();
    let mut parts = 0usize;
    // 文件头的记录数可能不准：以实际数据为准，取较小的
    for (i, record) in bytes[header..].chunks_exact(rec).take(count).enumerate() {
        if i % 1024 == 0 {
            check_deadline(opts)?;
        }
        if budget::exhausted() {
            break;
        }
        if record[0] == b'*' {
            continue;
        }
        if rows.len() > max_rows {
            warnings.push(format!("cells_truncated: stopped at {max_rows} rows"));
            break;
        }
        parts += 1;
        let row: Vec<String> = fields
            .iter()
            .map(|f| {
                let raw = record.get(f.start..f.start + f.len).unwrap_or(&[]);
                let text = value(f.kind, raw, enc);
                budget::spend(text.len());
                md::escape(&md::squeeze(&text))
            })
            .collect();
        rows.push(row);
    }
    Ok(Converted {
        markdown: md::render(&[Block::Table(rows)]),
        warnings,
        parts,
        ..Default::default()
    })
}

fn value(kind: u8, raw: &[u8], enc: &'static Encoding) -> String {
    match kind {
        b'D' if raw.len() == 8 && raw.iter().all(u8::is_ascii_digit) => {
            let s = String::from_utf8_lossy(raw);
            format!("{}-{}-{}", &s[..4], &s[4..6], &s[6..8])
        }
        b'L' => match raw.first() {
            Some(b'T' | b't' | b'Y' | b'y') => "true".into(),
            Some(b'F' | b'f' | b'N' | b'n') => "false".into(),
            _ => String::new(),
        },
        b'I' if raw.len() == 4 => i32::from_le_bytes([raw[0], raw[1], raw[2], raw[3]]).to_string(),
        b'Y' if raw.len() == 8 => {
            let v = i64::from_le_bytes(raw.try_into().unwrap_or([0; 8]));
            format!("{}", v as f64 / 10_000.0)
        }
        b'O' if raw.len() == 8 => {
            format!("{}", f64::from_le_bytes(raw.try_into().unwrap_or([0; 8])))
        }
        b'B' if raw.len() == 8 => {
            format!("{}", f64::from_le_bytes(raw.try_into().unwrap_or([0; 8])))
        }
        // 备注和通用字段的内容在 .dbt/.fpt 里，这里只有块号
        b'M' | b'G' | b'P' | b'B' => String::new(),
        _ => text(raw, enc).trim().to_owned(),
    }
}

fn text(raw: &[u8], enc: &'static Encoding) -> String {
    let end = raw
        .iter()
        .rposition(|&b| b != 0 && b != b' ')
        .map_or(0, |i| i + 1);
    let raw = &raw[..end];
    match std::str::from_utf8(raw) {
        Ok(s) => s.to_owned(),
        Err(_) => enc.decode(raw).0.into_owned(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn dbf(ldid: u8, records: &[&[u8]]) -> Vec<u8> {
        let fields: [(&str, u8, u8); 4] = [
            ("NAME", b'C', 6),
            ("AGE", b'N', 3),
            ("WHEN", b'D', 8),
            ("OK", b'L', 1),
        ];
        let header = 32 + 32 * fields.len() + 1;
        let rec = 1 + 6 + 3 + 8 + 1;
        let mut out = vec![0u8; 32];
        out[0] = 3;
        out[4..8].copy_from_slice(&(records.len() as u32).to_le_bytes());
        out[8..10].copy_from_slice(&(header as u16).to_le_bytes());
        out[10..12].copy_from_slice(&(rec as u16).to_le_bytes());
        out[29] = ldid;
        for (name, kind, len) in fields {
            let mut d = [0u8; 32];
            d[..name.len()].copy_from_slice(name.as_bytes());
            d[11] = kind;
            d[16] = len;
            out.extend_from_slice(&d);
        }
        out.push(0x0D);
        for r in records {
            out.extend_from_slice(r);
        }
        out
    }

    #[test]
    fn reads_rows_skips_deleted_and_decodes() {
        let bytes = dbf(
            0x4D,
            &[
                b" \xD6\xD0\xCE\xC4  42 20240131T",
                b"*gone    120240101F",
                b" abc     7        F",
            ],
        );
        let d = convert(&bytes, &Options::default()).unwrap();
        assert_eq!(d.parts, 2);
        assert!(
            d.markdown.contains("| NAME | AGE | WHEN | OK |"),
            "{}",
            d.markdown
        );
        assert!(
            d.markdown
                .contains("| \u{4e2d}\u{6587} | 42 | 2024-01-31 | true |"),
            "{}",
            d.markdown
        );
        assert!(!d.markdown.contains("gone"));
        assert!(
            d.markdown.contains("| abc | 7 |  | false |"),
            "{}",
            d.markdown
        );
    }

    #[test]
    fn wrong_files_are_rejected() {
        assert!(convert(b"short", &Options::default()).is_err());
        let mut bytes = dbf(3, &[]);
        bytes[8] = 0xFF;
        bytes[9] = 0xFF;
        assert!(convert(&bytes, &Options::default()).is_err());
        let mut bytes = dbf(3, &[]);
        bytes[15] = 1;
        assert_eq!(convert(&bytes, &Options::default()), Err(Error::Encrypted));
    }
}
