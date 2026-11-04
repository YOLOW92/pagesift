//! Outlook 压缩 RTF（MS-OXRTFCP）：16 字节头（压缩大小、原始大小、魔数、CRC），后面是 LZ77，
//! 字典 4096 字节，预置一段固定的 RTF 字符串。

/// §4.1 的预置字典。
const PREBUF: &[u8] = b"{\\rtf1\\ansi\\mac\\deff0\\deftab720{\\fonttbl;}{\\f0\\fnil \\froman \\fswiss \\fmodern \\fscript \\fdecor MS Sans SerifSymbolArialTimes New RomanCourier{\\colortbl\\red0\\green0\\blue0\r\n\\par \\pard\\plain\\f0\\fs20\\b\\i\\u\\tab\\tx";
const MAGIC_COMPRESSED: u32 = 0x7546_5A4C; // "LZFu"
const MAGIC_RAW: u32 = 0x414C_454D; // "MELA"
/// 原始大小上限：邮件正文不会更大。
const MAX_OUTPUT: usize = 64 << 20;

fn u32_at(d: &[u8], i: usize) -> Option<u32> {
    Some(u32::from_le_bytes(d.get(i..i + 4)?.try_into().ok()?))
}

pub(super) fn decompress(data: &[u8]) -> Option<Vec<u8>> {
    let raw_size = u32_at(data, 4)? as usize;
    if raw_size > MAX_OUTPUT {
        return None;
    }
    let body = data.get(16..)?;
    match u32_at(data, 8)? {
        MAGIC_RAW => return Some(body.get(..raw_size.min(body.len()))?.to_vec()),
        MAGIC_COMPRESSED => {}
        _ => return None,
    }
    let mut dict = [0u8; 4096];
    dict[..PREBUF.len()].copy_from_slice(PREBUF);
    let mut write = PREBUF.len();
    let mut out = Vec::with_capacity(raw_size);
    let mut p = 0usize;
    while out.len() < raw_size {
        let Some(&flags) = body.get(p) else { break };
        p += 1;
        for bit in 0..8 {
            if out.len() >= raw_size {
                break;
            }
            if flags >> bit & 1 == 0 {
                let Some(&b) = body.get(p) else {
                    return Some(out);
                };
                p += 1;
                out.push(b);
                dict[write] = b;
                write = (write + 1) % 4096;
            } else {
                let (Some(&hi), Some(&lo)) = (body.get(p), body.get(p + 1)) else {
                    return Some(out);
                };
                p += 2;
                let token = usize::from(hi) << 8 | usize::from(lo);
                let offset = token >> 4;
                let len = (token & 0xF) + 2;
                // 读位置等于写位置表示数据结束
                if offset == write {
                    return Some(out);
                }
                for k in 0..len {
                    let b = dict[(offset + k) % 4096];
                    out.push(b);
                    dict[write] = b;
                    write = (write + 1) % 4096;
                }
            }
        }
    }
    Some(out)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn spec_example_decompresses() {
        // MS-OXRTFCP §4.1.1 的例子
        let data: &[u8] = &[
            0x2d, 0x00, 0x00, 0x00, 0x2b, 0x00, 0x00, 0x00, 0x4c, 0x5a, 0x46, 0x75, 0xf1, 0xc5,
            0xc7, 0xa7, 0x03, 0x00, 0x0a, 0x00, 0x72, 0x63, 0x70, 0x67, 0x31, 0x32, 0x35, 0x42,
            0x32, 0x0a, 0xf3, 0x20, 0x68, 0x65, 0x6c, 0x09, 0x00, 0x20, 0x62, 0x77, 0x05, 0xb0,
            0x6c, 0x64, 0x7d, 0x0a, 0x80, 0x0f, 0xa0,
        ];
        assert_eq!(
            decompress(data).unwrap(),
            b"{\\rtf1\\ansi\\ansicpg1252\\pard hello world}\r\n".to_vec()
        );
    }

    #[test]
    fn uncompressed_and_broken() {
        let mut d = vec![0u8; 16];
        d[4] = 2;
        d[8..12].copy_from_slice(&MAGIC_RAW.to_le_bytes());
        d.extend_from_slice(b"hi!");
        assert_eq!(decompress(&d).unwrap(), b"hi");
        d[8] = 0;
        assert_eq!(decompress(&d), None);
        assert_eq!(decompress(b"short"), None);
    }
}
