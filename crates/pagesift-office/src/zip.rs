//! 只读的 ZIP 容器（APPNOTE 6.3.10）：OOXML 和 EPUB 都是 ZIP。
//!
//! 只认中央目录：本地文件头里的大小可能是 0（流式写入时写在数据描述符里），中央目录的才可靠。
//! 支持“不压缩”和 deflate 两种方法、ZIP64。条目按需解压，解压量受 [`Limits`] 约束：
//! 单个条目、整个文件累计、压缩比三道限额，防 ZIP 炸弹。

use std::cell::Cell;
use std::collections::HashMap;

use crate::{Error, Limits};

const EOCD_SIG: u32 = 0x0605_4b50;
const EOCD64_SIG: u32 = 0x0606_4b50;
const EOCD64_LOCATOR_SIG: u32 = 0x0706_4b50;
const CENTRAL_SIG: u32 = 0x0201_4b50;
const LOCAL_SIG: u32 = 0x0403_4b50;
/// 解压后大于这么多字节的条目才检查压缩比：小文件压缩比高很正常（全是空格的 XML）。
const RATIO_CHECK_BYTES: u64 = 4 << 20;

struct Entry {
    name: String,
    method: u16,
    flags: u16,
    compressed: u64,
    size: u64,
    offset: u64,
}

pub(crate) struct Zip<'a> {
    data: &'a [u8],
    entries: Vec<Entry>,
    /// 小写名字 → 下标：OPC 的部件名不分大小写（ECMA-376 第 2 部分 9.1.1.1）。
    by_name: HashMap<String, usize>,
    limits: Limits,
    /// 还能解压的字节数（整个文件累计）。
    budget: Cell<u64>,
}

fn u16_at(b: &[u8], at: usize) -> Option<u16> {
    Some(u16::from_le_bytes(b.get(at..at + 2)?.try_into().ok()?))
}
fn u32_at(b: &[u8], at: usize) -> Option<u32> {
    Some(u32::from_le_bytes(b.get(at..at + 4)?.try_into().ok()?))
}
fn u64_at(b: &[u8], at: usize) -> Option<u64> {
    Some(u64::from_le_bytes(b.get(at..at + 8)?.try_into().ok()?))
}
fn bad(what: &str) -> Error {
    Error::Malformed(format!("ZIP: {what}"))
}

impl<'a> Zip<'a> {
    pub(crate) fn open(data: &'a [u8], limits: &Limits) -> Result<Self, Error> {
        let (count, cd_offset, cd_size) = end_of_directory(data)?;
        if count > limits.max_zip_entries as u64 {
            return Err(Error::TooLarge("number of ZIP entries"));
        }
        let start = usize::try_from(cd_offset).map_err(|_| bad("directory offset"))?;
        let end = start
            .checked_add(usize::try_from(cd_size).map_err(|_| bad("directory size"))?)
            .filter(|&e| e <= data.len())
            .ok_or_else(|| bad("central directory outside the file"))?;
        let cd = &data[start..end];
        let mut entries = Vec::with_capacity(count as usize);
        let mut at = 0;
        while at + 46 <= cd.len() && u32_at(cd, at) == Some(CENTRAL_SIG) {
            if entries.len() >= limits.max_zip_entries {
                return Err(Error::TooLarge("number of ZIP entries"));
            }
            let flags = u16_at(cd, at + 8).unwrap_or(0);
            let method = u16_at(cd, at + 10).unwrap_or(0);
            let mut compressed = u64::from(u32_at(cd, at + 20).unwrap_or(0));
            let mut size = u64::from(u32_at(cd, at + 24).unwrap_or(0));
            let name_len = usize::from(u16_at(cd, at + 28).unwrap_or(0));
            let extra_len = usize::from(u16_at(cd, at + 30).unwrap_or(0));
            let comment_len = usize::from(u16_at(cd, at + 32).unwrap_or(0));
            let mut offset = u64::from(u32_at(cd, at + 42).unwrap_or(0));
            let name_at = at + 46;
            let name = cd
                .get(name_at..name_at + name_len)
                .ok_or_else(|| bad("truncated entry name"))?;
            let extra = cd
                .get(name_at + name_len..name_at + name_len + extra_len)
                .unwrap_or_default();
            zip64_extra(extra, &mut size, &mut compressed, &mut offset);
            entries.push(Entry {
                name: String::from_utf8_lossy(name).into_owned(),
                method,
                flags,
                compressed,
                size,
                offset,
            });
            at = name_at + name_len + extra_len + comment_len;
        }
        let mut by_name = HashMap::with_capacity(entries.len());
        for (i, e) in entries.iter().enumerate() {
            by_name.entry(e.name.to_lowercase()).or_insert(i);
        }
        Ok(Zip {
            data,
            entries,
            by_name,
            limits: limits.clone(),
            budget: Cell::new(limits.max_total_bytes),
        })
    }

    pub(crate) fn contains(&self, name: &str) -> bool {
        self.by_name.contains_key(&name.to_lowercase())
    }

    /// 解压一个条目；没有这个条目时返回 `None`。名字不分大小写，开头的 `/` 可有可无。
    pub(crate) fn read(&self, name: &str) -> Result<Option<Vec<u8>>, Error> {
        let key = name.trim_start_matches('/').to_lowercase();
        let Some(&i) = self.by_name.get(&key) else {
            return Ok(None);
        };
        let e = &self.entries[i];
        if e.flags & 1 != 0 {
            return Err(Error::Encrypted);
        }
        if e.size > self.limits.max_entry_bytes {
            return Err(Error::TooLarge("ZIP entry size"));
        }
        if e.size > self.budget.get() {
            return Err(Error::TooLarge("total uncompressed size"));
        }
        if e.size > RATIO_CHECK_BYTES && e.size / e.compressed.max(1) > self.limits.max_ratio {
            return Err(Error::TooLarge("ZIP compression ratio"));
        }
        let at = usize::try_from(e.offset).map_err(|_| bad("entry offset"))?;
        let local = self
            .data
            .get(at..at + 30)
            .ok_or_else(|| bad("entry offset"))?;
        if u32_at(local, 0) != Some(LOCAL_SIG) {
            return Err(bad("missing local file header"));
        }
        let skip = 30
            + usize::from(u16_at(local, 26).unwrap_or(0))
            + usize::from(u16_at(local, 28).unwrap_or(0));
        let start = at + skip;
        let raw = usize::try_from(e.compressed)
            .ok()
            .and_then(|n| self.data.get(start..start.checked_add(n)?))
            .ok_or_else(|| bad("entry data outside the file"))?;
        // 声明的大小不可信，解压上限按声明大小，多出来就是坏文件（或炸弹）
        let want = e.size as usize;
        let out = match e.method {
            0 => raw.to_vec(),
            8 => miniz_oxide::inflate::decompress_to_vec_with_limit(raw, want.max(1))
                .map_err(|err| bad(&format!("could not inflate {}: {:?}", e.name, err.status)))?,
            m => return Err(Error::Unsupported(format!("ZIP compression method {m}"))),
        };
        if out.len() != want {
            return Err(bad(&format!(
                "{}: size does not match the directory",
                e.name
            )));
        }
        self.budget.set(self.budget.get() - e.size);
        Ok(Some(out))
    }

    /// 读一个文本部件；没有时是 `None`。UTF-16（带 BOM）转成 UTF-8，其余按 UTF-8，坏字节替换。
    pub(crate) fn read_text(&self, name: &str) -> Result<Option<String>, Error> {
        Ok(self.read(name)?.map(crate::xml::decode))
    }
}

/// 找中央目录结尾记录：(条目数, 目录偏移, 目录大小)。
fn end_of_directory(data: &[u8]) -> Result<(u64, u64, u64), Error> {
    if data.len() < 22 {
        return Err(bad("file too small"));
    }
    // 结尾记录后面最多跟 65535 字节的注释
    let floor = data.len().saturating_sub(22 + 65_535);
    let at = (floor..=data.len() - 22)
        .rev()
        .find(|&i| u32_at(data, i) == Some(EOCD_SIG))
        .ok_or_else(|| bad("end of central directory not found"))?;
    let count = u64::from(u16_at(data, at + 10).unwrap_or(0));
    let size = u64::from(u32_at(data, at + 12).unwrap_or(0));
    let offset = u64::from(u32_at(data, at + 16).unwrap_or(0));
    if count != 0xFFFF && size != 0xFFFF_FFFF && offset != 0xFFFF_FFFF {
        return Ok((count, offset, size));
    }
    // ZIP64：结尾记录前面 20 字节是定位记录，指向 ZIP64 结尾记录
    let loc = at
        .checked_sub(20)
        .ok_or_else(|| bad("ZIP64 locator missing"))?;
    if u32_at(data, loc) != Some(EOCD64_LOCATOR_SIG) {
        return Err(bad("ZIP64 locator missing"));
    }
    let rec = u64_at(data, loc + 8)
        .and_then(|v| usize::try_from(v).ok())
        .filter(|&r| u32_at(data, r) == Some(EOCD64_SIG))
        .ok_or_else(|| bad("ZIP64 end of directory missing"))?;
    Ok((
        u64_at(data, rec + 32).ok_or_else(|| bad("ZIP64 record"))?,
        u64_at(data, rec + 48).ok_or_else(|| bad("ZIP64 record"))?,
        u64_at(data, rec + 40).ok_or_else(|| bad("ZIP64 record"))?,
    ))
}

/// 中央目录里的 ZIP64 扩展字段（ID 0x0001）：按顺序只放取值为 0xFFFFFFFF 的那几项。
fn zip64_extra(mut extra: &[u8], size: &mut u64, compressed: &mut u64, offset: &mut u64) {
    while extra.len() >= 4 {
        let id = u16_at(extra, 0).unwrap_or(0);
        let len = usize::from(u16_at(extra, 2).unwrap_or(0));
        let body = extra.get(4..4 + len).unwrap_or_default();
        if id == 1 {
            let mut at = 0;
            for v in [size, compressed, offset] {
                if *v == 0xFFFF_FFFF
                    && let Some(x) = u64_at(body, at)
                {
                    *v = x;
                    at += 8;
                }
            }
            return;
        }
        extra = extra.get(4 + len..).unwrap_or_default();
    }
}

#[cfg(test)]
pub(crate) mod tests {
    use super::*;

    /// 测试用的最小 ZIP 写入：条目全部不压缩，或者用 miniz_oxide 压缩。
    pub(crate) fn build(entries: &[(&str, &[u8], bool)]) -> Vec<u8> {
        let mut out = Vec::new();
        let mut cd = Vec::new();
        for (name, data, deflate) in entries {
            let body = if *deflate {
                miniz_oxide::deflate::compress_to_vec(data, 6)
            } else {
                data.to_vec()
            };
            let method: u16 = if *deflate { 8 } else { 0 };
            let offset = out.len() as u32;
            for b in [
                &LOCAL_SIG.to_le_bytes()[..],
                &[20, 0, 0, 0],
                &method.to_le_bytes(),
                &[0; 8],
            ] {
                out.extend_from_slice(b);
            }
            out.extend_from_slice(&(body.len() as u32).to_le_bytes());
            out.extend_from_slice(&(data.len() as u32).to_le_bytes());
            out.extend_from_slice(&(name.len() as u16).to_le_bytes());
            out.extend_from_slice(&[0, 0]);
            out.extend_from_slice(name.as_bytes());
            out.extend_from_slice(&body);
            cd.extend_from_slice(&CENTRAL_SIG.to_le_bytes());
            cd.extend_from_slice(&[20, 0, 20, 0, 0, 0]);
            cd.extend_from_slice(&method.to_le_bytes());
            cd.extend_from_slice(&[0; 8]);
            cd.extend_from_slice(&(body.len() as u32).to_le_bytes());
            cd.extend_from_slice(&(data.len() as u32).to_le_bytes());
            cd.extend_from_slice(&(name.len() as u16).to_le_bytes());
            cd.extend_from_slice(&[0; 12]);
            cd.extend_from_slice(&offset.to_le_bytes());
            cd.extend_from_slice(name.as_bytes());
        }
        let cd_offset = out.len() as u32;
        out.extend_from_slice(&cd);
        out.extend_from_slice(&EOCD_SIG.to_le_bytes());
        out.extend_from_slice(&[0; 4]);
        out.extend_from_slice(&(entries.len() as u16).to_le_bytes());
        out.extend_from_slice(&(entries.len() as u16).to_le_bytes());
        out.extend_from_slice(&(cd.len() as u32).to_le_bytes());
        out.extend_from_slice(&cd_offset.to_le_bytes());
        out.extend_from_slice(&[0, 0]);
        out
    }

    #[test]
    fn reads_stored_and_deflated_entries_case_insensitively() {
        let z = build(&[
            ("a.txt", b"hello", false),
            ("Dir/B.xml", &[b'x'; 1000], true),
        ]);
        let zip = Zip::open(&z, &Limits::default()).unwrap();
        assert_eq!(zip.read("a.txt").unwrap().unwrap(), b"hello");
        assert_eq!(zip.read("/dir/b.XML").unwrap().unwrap(), vec![b'x'; 1000]);
        assert!(zip.read("missing").unwrap().is_none());
        assert!(zip.contains("DIR/b.xml"));
    }

    #[test]
    fn zip_bombs_hit_the_limits() {
        let big = vec![0u8; 8 << 20];
        let z = build(&[("bomb.xml", &big, true)]);
        // 压缩比约 1000:1，超过默认上限
        let err = Zip::open(&z, &Limits::default()).unwrap().read("bomb.xml");
        assert_eq!(err.unwrap_err(), Error::TooLarge("ZIP compression ratio"));
        let small = Limits {
            max_entry_bytes: 1 << 20,
            ..Limits::default()
        };
        let err = Zip::open(&z, &small).unwrap().read("bomb.xml");
        assert_eq!(err.unwrap_err(), Error::TooLarge("ZIP entry size"));
        // 累计限额：两个各 600 KB 的条目，总额 1 MB
        let part = vec![b'a'; 600 << 10];
        let z = build(&[("1", &part, false), ("2", &part, false)]);
        let tight = Limits {
            max_total_bytes: 1 << 20,
            ..Limits::default()
        };
        let zip = Zip::open(&z, &tight).unwrap();
        assert!(zip.read("1").is_ok());
        assert_eq!(
            zip.read("2").unwrap_err(),
            Error::TooLarge("total uncompressed size")
        );
    }

    #[test]
    fn lying_sizes_and_garbage_are_errors_not_panics() {
        let mut z = build(&[("a", b"hello world", true)]);
        // 把中央目录里声明的解压大小改小：解压到上限就停，报大小不符
        let cd = z
            .windows(4)
            .rposition(|w| w == CENTRAL_SIG.to_le_bytes())
            .unwrap();
        z[cd + 24] = 3;
        assert!(matches!(
            Zip::open(&z, &Limits::default()).unwrap().read("a"),
            Err(Error::Malformed(_))
        ));
        for junk in [&b""[..], b"PK\x03\x04", &[0u8; 100]] {
            assert!(Zip::open(junk, &Limits::default()).is_err());
        }
        for cut in [10, 40, 60] {
            let z = build(&[("a", b"hello", false)]);
            if let Ok(zip) = Zip::open(&z[cut..], &Limits::default()) {
                let _ = zip.read("a");
            }
        }
    }
}
