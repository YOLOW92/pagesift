//! tar 归档（POSIX ustar、GNU 长名、PAX 路径）和 gzip（RFC 1952）。只读，不落盘，
//! 路径只用来显示。只产出普通文件；链接、目录、设备一律不要。

use std::borrow::Cow;

use crate::Error;

const BLOCK: usize = 512;

/// 归档里的一个普通文件。
pub(crate) struct Entry<'a> {
    pub(crate) path: String,
    pub(crate) data: &'a [u8],
}

/// 有 `ustar` 标记，或者老式 tar 的校验和对得上。
pub(crate) fn looks_like_tar(bytes: &[u8]) -> bool {
    let Some(h) = bytes.get(..BLOCK) else {
        return false;
    };
    if &h[257..262] == b"ustar" {
        return true;
    }
    let Some(stored) = octal(&h[148..156]) else {
        return false;
    };
    let sum: u64 = h
        .iter()
        .enumerate()
        .map(|(i, &b)| {
            if (148..156).contains(&i) {
                32
            } else {
                u64::from(b)
            }
        })
        .sum();
    stored != 0 && stored == sum
}

/// 列出普通文件。`truncated` 表示文件在中途结束，已读到的照常返回。
pub(crate) fn entries(bytes: &[u8], max: usize) -> (Vec<Entry<'_>>, bool) {
    let mut out = Vec::new();
    let mut pos = 0usize;
    let mut long_name: Option<String> = None;
    let mut pax_path: Option<String> = None;
    while let Some(h) = bytes.get(pos..pos + BLOCK) {
        if h.iter().all(|&b| b == 0) {
            return (out, false);
        }
        let Some(size) = size(&h[124..136]) else {
            return (out, true);
        };
        let body = pos + BLOCK;
        let Some(next) = usize::try_from(size)
            .ok()
            .and_then(|s| s.checked_add(BLOCK - 1))
            .map(|s| s / BLOCK * BLOCK)
            .and_then(|s| body.checked_add(s))
        else {
            return (out, true);
        };
        let data = bytes.get(body..body + size as usize);
        match h[156] {
            b'L' => long_name = data.map(cstr).map(Cow::into_owned),
            b'x' => pax_path = data.and_then(pax_path_of),
            b'g' => {}
            b'0' | 0 | b'7' => {
                let Some(data) = data else {
                    return (out, true);
                };
                let path = pax_path
                    .take()
                    .or_else(|| long_name.take())
                    .unwrap_or_else(|| header_path(h));
                // 老式 tar 用结尾的斜杠表示目录
                if !path.ends_with('/') {
                    out.push(Entry { path, data });
                    if out.len() >= max {
                        return (out, false);
                    }
                }
            }
            _ => {
                long_name = None;
                pax_path = None;
            }
        }
        pos = next;
    }
    (out, pos < bytes.len())
}

fn header_path(h: &[u8]) -> String {
    let name = cstr(&h[..100]);
    if &h[257..262] == b"ustar" {
        let prefix = cstr(&h[345..500]);
        if !prefix.is_empty() {
            return format!("{prefix}/{name}");
        }
    }
    name.into_owned()
}

fn cstr(b: &[u8]) -> Cow<'_, str> {
    let end = b.iter().position(|&c| c == 0).unwrap_or(b.len());
    String::from_utf8_lossy(&b[..end])
}

fn octal(field: &[u8]) -> Option<u64> {
    let s = std::str::from_utf8(field).ok()?;
    let s = s.trim_matches(|c: char| c == '\0' || c == ' ');
    if s.is_empty() {
        return Some(0);
    }
    u64::from_str_radix(s, 8).ok()
}

/// 大小字段：八进制，或者 GNU 的 base-256（最高位置 1）。
fn size(field: &[u8]) -> Option<u64> {
    if field[0] & 0x80 != 0 {
        let mut n = u64::from(field[0] & 0x7f);
        for &b in &field[1..] {
            n = n.checked_mul(256)?.checked_add(u64::from(b))?;
        }
        return Some(n);
    }
    octal(field)
}

/// PAX 扩展头：一行一条 `长度 键=值\n`，只要 `path`。
fn pax_path_of(data: &[u8]) -> Option<String> {
    let mut rest = data;
    while !rest.is_empty() {
        let sp = rest.iter().position(|&b| b == b' ')?;
        let len: usize = std::str::from_utf8(&rest[..sp]).ok()?.parse().ok()?;
        let record = rest.get(sp + 1..len)?;
        if let Some(v) = record.strip_prefix(b"path=") {
            let v = v.strip_suffix(b"\n").unwrap_or(v);
            return Some(String::from_utf8_lossy(v).into_owned());
        }
        rest = &rest[len..];
    }
    None
}

/// gzip 的结果：解压后的字节和头里记的原文件名。
pub(crate) fn gunzip(bytes: &[u8], max: usize) -> Result<(Vec<u8>, Option<String>), Error> {
    let bad = || Error::Malformed("broken gzip header".into());
    let h = bytes.get(..10).ok_or_else(bad)?;
    if h[0] != 0x1f || h[1] != 0x8b || h[2] != 8 {
        return Err(bad());
    }
    let flags = h[3];
    let mut pos = 10usize;
    if flags & 0x04 != 0 {
        let n = bytes.get(pos..pos + 2).ok_or_else(bad)?;
        pos += 2 + usize::from(u16::from_le_bytes([n[0], n[1]]));
    }
    let mut name = None;
    if flags & 0x08 != 0 {
        let rest = bytes.get(pos..).ok_or_else(bad)?;
        let end = rest.iter().position(|&b| b == 0).ok_or_else(bad)?;
        name = Some(String::from_utf8_lossy(&rest[..end]).into_owned());
        pos += end + 1;
    }
    if flags & 0x10 != 0 {
        let rest = bytes.get(pos..).ok_or_else(bad)?;
        pos += rest.iter().position(|&b| b == 0).ok_or_else(bad)? + 1;
    }
    if flags & 0x02 != 0 {
        pos += 2;
    }
    let body = bytes.get(pos..).ok_or_else(bad)?;
    let out = miniz_oxide::inflate::decompress_to_vec_with_limit(body, max).map_err(|e| {
        if matches!(e.status, miniz_oxide::inflate::TINFLStatus::HasMoreOutput) {
            Error::TooLarge("decompressed size")
        } else {
            Error::Malformed("broken gzip data".into())
        }
    })?;
    Ok((out, name))
}

#[cfg(test)]
pub(crate) mod tests {
    use super::*;

    /// 测试用：写一个带校验和的 tar 头加内容。
    pub(crate) fn file(name: &str, body: &[u8], kind: u8) -> Vec<u8> {
        let mut h = [0u8; BLOCK];
        h[..name.len()].copy_from_slice(name.as_bytes());
        h[100..107].copy_from_slice(b"0000644");
        h[124..135].copy_from_slice(format!("{:011o}", body.len()).as_bytes());
        h[156] = kind;
        h[257..263].copy_from_slice(b"ustar\0");
        h[148..156].fill(b' ');
        let sum: u32 = h.iter().map(|&b| u32::from(b)).sum();
        h[148..155].copy_from_slice(format!("{sum:06o}\0").as_bytes());
        let mut out = h.to_vec();
        out.extend_from_slice(body);
        out.resize(out.len().div_ceil(BLOCK) * BLOCK, 0);
        out
    }

    #[test]
    fn reads_files_long_names_and_pax_paths() {
        let long = "d/".repeat(80) + "long.txt";
        let pax_rec = "21 path=pax/name.txt\n";
        let mut t = file("a.txt", b"hello", b'0');
        t.extend(file("dir/", b"", b'5'));
        t.extend(file("././@LongLink", long.as_bytes(), b'L'));
        t.extend(file("short", b"L", b'0'));
        t.extend(file("PaxHeader", pax_rec.as_bytes(), b'x'));
        t.extend(file("ignored", b"P", b'0'));
        t.extend(file("link", b"", b'2'));
        t.extend([0u8; BLOCK * 2]);
        assert!(looks_like_tar(&t));
        let (e, truncated) = entries(&t, 100);
        assert!(!truncated);
        let names: Vec<&str> = e.iter().map(|x| x.path.as_str()).collect();
        assert_eq!(names, ["a.txt", long.as_str(), "pax/name.txt"]);
        assert_eq!(e[0].data, b"hello");
    }

    #[test]
    fn truncated_and_hostile_headers_stop_cleanly() {
        let t = file("a.txt", &[b'x'; 2000], b'0');
        let (e, truncated) = entries(&t[..700], 10);
        assert!(e.is_empty() && truncated);
        // 声明的大小超过文件
        let mut huge = file("a", b"", b'0');
        huge[124..135].copy_from_slice(b"77777777777");
        assert!(entries(&huge, 10).0.is_empty());
        assert!(!looks_like_tar(b"short"));
        assert!(!looks_like_tar(&[0u8; 600]));
        let two = [file("a", b"1", b'0'), file("b", b"2", b'0')].concat();
        let (e, _) = entries(&two, 1);
        assert_eq!(e.len(), 1);
    }

    #[test]
    fn gunzip_reads_headers_and_enforces_the_limit() {
        // `printf 'hello' | gzip -n` 的固定字节，另手工加文件名
        let body = [0xcb, 0x48, 0xcd, 0xc9, 0xc9, 0x07, 0x00];
        let mut gz = vec![0x1f, 0x8b, 8, 0x08, 0, 0, 0, 0, 0, 3];
        gz.extend_from_slice(b"a.txt\0");
        gz.extend_from_slice(&body);
        gz.extend_from_slice(&[0x86, 0xa6, 0x10, 0x36, 5, 0, 0, 0]);
        let (out, name) = gunzip(&gz, 100).unwrap();
        assert_eq!(out, b"hello");
        assert_eq!(name.as_deref(), Some("a.txt"));
        assert_eq!(
            gunzip(&gz, 2).unwrap_err(),
            Error::TooLarge("decompressed size")
        );
        assert!(gunzip(b"\x1f\x8b", 10).is_err());
    }
}
