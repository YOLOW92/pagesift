//! OLE 文档属性（`\x05SummaryInformation`，MS-OLEPS）：标题、主题、作者。

use crate::Converted;
use crate::ole::Ole;

const VT_LPSTR: u32 = 30;
const VT_LPWSTR: u32 = 31;
const VT_I2: u32 = 2;
const PID_CODEPAGE: u32 = 1;
const PID_TITLE: u32 = 2;
const PID_SUBJECT: u32 = 3;
const PID_AUTHOR: u32 = 4;

fn u32_at(d: &[u8], i: usize) -> Option<u32> {
    Some(u32::from_le_bytes(d.get(i..i + 4)?.try_into().ok()?))
}

/// 读属性流，写进 `out`。没有或者坏了就什么都不写：属性不影响正文。
pub(super) fn apply(ole: &Ole<'_>, out: &mut Converted) {
    let root = ole.children(None);
    let Some(entry) = root
        .iter()
        .find(|e| e.is_stream && e.name.trim_start_matches('\u{5}') == "SummaryInformation")
    else {
        return;
    };
    let Ok(data) = ole.read(entry) else { return };
    let Some(props) = read_props(&data) else {
        return;
    };
    for (pid, value) in props {
        let value = value.trim().to_owned();
        if value.is_empty() {
            continue;
        }
        match pid {
            PID_TITLE => out.title = Some(value),
            PID_SUBJECT => out.metadata.push(("subject".into(), value)),
            PID_AUTHOR => out.metadata.push(("creator".into(), value)),
            _ => {}
        }
    }
}

fn read_props(d: &[u8]) -> Option<Vec<(u32, String)>> {
    if d.get(..2)? != [0xFE, 0xFF] {
        return None;
    }
    // 头 28 字节，第一节的 FMTID 16 字节，再是偏移
    let section = u32_at(d, 44)? as usize;
    let count = u32_at(d, section + 4)? as usize;
    if count > 4096 {
        return None;
    }
    let mut codepage = 1252u16;
    let mut raw = Vec::new();
    for i in 0..count {
        let pid = u32_at(d, section + 8 + i * 8)?;
        let at = section + u32_at(d, section + 12 + i * 8)? as usize;
        let kind = u32_at(d, at)?;
        if pid == PID_CODEPAGE && kind == VT_I2 {
            codepage = u16::from_le_bytes(d.get(at + 4..at + 6)?.try_into().ok()?);
        } else if matches!(pid, PID_TITLE | PID_SUBJECT | PID_AUTHOR) {
            raw.push((pid, kind, at));
        }
    }
    let mut out = Vec::new();
    for (pid, kind, at) in raw {
        let n = u32_at(d, at + 4)? as usize;
        let text = match kind {
            VT_LPSTR => decode(d.get(at + 8..at + 8 + n)?, codepage),
            VT_LPWSTR => {
                let units: Vec<u16> = d
                    .get(at + 8..at + 8 + n * 2)?
                    .as_chunks::<2>()
                    .0
                    .iter()
                    .map(|c| u16::from_le_bytes([c[0], c[1]]))
                    .collect();
                String::from_utf16_lossy(&units)
            }
            _ => continue,
        };
        out.push((pid, text.trim_end_matches('\0').to_owned()));
    }
    Some(out)
}

/// Windows 代码页解码。
pub(super) fn decode(raw: &[u8], codepage: u16) -> String {
    let enc = match codepage {
        65001 => encoding_rs::UTF_8,
        936 => encoding_rs::GBK,
        950 => encoding_rs::BIG5,
        932 => encoding_rs::SHIFT_JIS,
        949 => encoding_rs::EUC_KR,
        874 => encoding_rs::WINDOWS_874,
        1250 => encoding_rs::WINDOWS_1250,
        1251 => encoding_rs::WINDOWS_1251,
        1253 => encoding_rs::WINDOWS_1253,
        1254 => encoding_rs::WINDOWS_1254,
        1255 => encoding_rs::WINDOWS_1255,
        1256 => encoding_rs::WINDOWS_1256,
        1257 => encoding_rs::WINDOWS_1257,
        1258 => encoding_rs::WINDOWS_1258,
        _ => encoding_rs::WINDOWS_1252,
    };
    enc.decode(raw).0.into_owned()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn reads_title_and_author_in_their_codepage() {
        let mut d = vec![0xFE, 0xFF];
        d.resize(44, 0);
        d.extend_from_slice(&48u32.to_le_bytes());
        // 节：大小、3 个属性
        let start = d.len();
        d.extend_from_slice(&[0; 4]);
        d.extend_from_slice(&3u32.to_le_bytes());
        let values: Vec<(u32, Vec<u8>)> = vec![
            (
                1,
                [
                    2u32.to_le_bytes().as_slice(),
                    &936u16.to_le_bytes(),
                    &[0, 0],
                ]
                .concat(),
            ),
            (
                2,
                [
                    30u32.to_le_bytes().as_slice(),
                    &5u32.to_le_bytes(),
                    b"\xD6\xD0\xCE\xC4\0",
                ]
                .concat(),
            ),
            (
                4,
                [
                    31u32.to_le_bytes().as_slice(),
                    &2u32.to_le_bytes(),
                    &[b'A', 0, 0, 0],
                ]
                .concat(),
            ),
        ];
        let mut offset = 8 + 3 * 8;
        for (pid, v) in &values {
            d.extend_from_slice(&pid.to_le_bytes());
            d.extend_from_slice(&(offset as u32).to_le_bytes());
            offset += v.len();
        }
        for (_, v) in &values {
            d.extend_from_slice(v);
        }
        assert_eq!(d[start..].len(), offset);
        let p = read_props(&d).unwrap();
        assert_eq!(
            p,
            vec![(2, "\u{4e2d}\u{6587}".to_owned()), (4, "A".to_owned())]
        );
    }

    #[test]
    fn broken_streams_give_nothing() {
        assert_eq!(read_props(b"xx"), None);
        assert_eq!(read_props(&[0xFE, 0xFF, 0]), None);
    }
}
