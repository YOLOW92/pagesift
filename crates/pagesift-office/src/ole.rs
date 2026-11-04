//! OLE 复合文件（MS-CFB）的只读访问：目录树和流。链表都做了循环和越界保护，坏文件只会读不到，不会卡死。

use crate::Error;

const SIGNATURE: [u8; 8] = [0xD0, 0xCF, 0x11, 0xE0, 0xA1, 0xB1, 0x1A, 0xE1];
/// 目录项里“没有”的取值（NOSTREAM）。
const NONE: u32 = 0xFFFF_FFFF;
const END_OF_CHAIN: u32 = 0xFFFF_FFFE;
/// 单个流上限，和 ZIP 单个条目一致。
const MAX_STREAM: u64 = 256 << 20;

pub(crate) struct Ole<'a> {
    data: &'a [u8],
    sector: usize,
    mini_sector: usize,
    cutoff: u64,
    fat: Vec<u32>,
    mini_fat: Vec<u32>,
    mini_stream: Vec<u8>,
    entries: Vec<Entry>,
}

#[derive(Debug, Clone)]
pub(crate) struct Entry {
    pub(crate) name: String,
    pub(crate) is_stream: bool,
    start: u32,
    size: u64,
    left: u32,
    right: u32,
    child: u32,
}

fn u16_at(d: &[u8], i: usize) -> Option<u16> {
    Some(u16::from_le_bytes(d.get(i..i + 2)?.try_into().ok()?))
}

fn u32_at(d: &[u8], i: usize) -> Option<u32> {
    Some(u32::from_le_bytes(d.get(i..i + 4)?.try_into().ok()?))
}

fn u64_at(d: &[u8], i: usize) -> Option<u64> {
    Some(u64::from_le_bytes(d.get(i..i + 8)?.try_into().ok()?))
}

impl<'a> Ole<'a> {
    pub(crate) fn open(data: &'a [u8]) -> Result<Self, Error> {
        let bad = |m: &str| Error::Malformed(format!("OLE compound file: {m}"));
        if !data.starts_with(&SIGNATURE) {
            return Err(bad("wrong signature"));
        }
        let shift = u16_at(data, 30).ok_or_else(|| bad("short header"))?;
        let mini_shift = u16_at(data, 32).ok_or_else(|| bad("short header"))?;
        if !matches!(shift, 9 | 12) || mini_shift != 6 {
            return Err(bad("unsupported sector size"));
        }
        let sector = 1usize << shift;
        let num_fat = u32_at(data, 44).ok_or_else(|| bad("short header"))? as usize;
        let dir_start = u32_at(data, 48).ok_or_else(|| bad("short header"))?;
        let cutoff = u64::from(u32_at(data, 56).ok_or_else(|| bad("short header"))?);
        let mini_fat_start = u32_at(data, 60).ok_or_else(|| bad("short header"))?;
        let difat_start = u32_at(data, 68).ok_or_else(|| bad("short header"))?;
        let max_sectors = data.len() / sector + 1;

        // FAT 扇区号：头里 109 个，其余在 DIFAT 链上
        let mut fat_sectors: Vec<u32> = (0..109)
            .filter_map(|i| u32_at(data, 76 + i * 4))
            .filter(|&s| s < END_OF_CHAIN)
            .collect();
        let mut next = difat_start;
        let mut hops = 0;
        while next < END_OF_CHAIN && hops < max_sectors {
            hops += 1;
            let Some(body) = sector_slice(data, sector, next) else {
                break;
            };
            let per = sector / 4 - 1;
            fat_sectors.extend(
                (0..per)
                    .filter_map(|i| u32_at(body, i * 4))
                    .filter(|&s| s < END_OF_CHAIN),
            );
            next = u32_at(body, per * 4).unwrap_or(END_OF_CHAIN);
        }
        fat_sectors.truncate(num_fat.max(fat_sectors.len().min(max_sectors)));
        let mut fat = Vec::new();
        for s in fat_sectors {
            if let Some(body) = sector_slice(data, sector, s) {
                fat.extend(body.as_chunks::<4>().0.iter().filter_map(|c| u32_at(c, 0)));
            }
        }
        let mut ole = Ole {
            data,
            sector,
            mini_sector: 1 << mini_shift,
            cutoff,
            fat,
            mini_fat: Vec::new(),
            mini_stream: Vec::new(),
            entries: Vec::new(),
        };
        let dir = ole.chain(dir_start, u64::MAX)?;
        ole.entries = dir
            .as_chunks::<128>()
            .0
            .iter()
            .map(|e| parse_entry(e))
            .collect();
        if ole.entries.is_empty() {
            return Err(bad("empty directory"));
        }
        if mini_fat_start < END_OF_CHAIN {
            let table = ole.chain(mini_fat_start, u64::MAX)?;
            ole.mini_fat = table
                .as_chunks::<4>()
                .0
                .iter()
                .filter_map(|c| u32_at(c, 0))
                .collect();
        }
        let root = ole.entries[0].clone();
        if root.start < END_OF_CHAIN {
            ole.mini_stream = ole.chain(root.start, root.size)?;
        }
        Ok(ole)
    }

    fn chain(&self, start: u32, size: u64) -> Result<Vec<u8>, Error> {
        let mut out = Vec::new();
        let mut cur = start;
        let mut hops = 0usize;
        let cap = self.fat.len();
        while cur < END_OF_CHAIN && (out.len() as u64) < size {
            if hops > cap {
                return Err(Error::Malformed("OLE sector chain loops".into()));
            }
            hops += 1;
            if out.len() as u64 >= MAX_STREAM {
                return Err(Error::TooLarge("OLE stream"));
            }
            let Some(body) = sector_slice(self.data, self.sector, cur) else {
                break;
            };
            out.extend_from_slice(body);
            cur = self.fat.get(cur as usize).copied().unwrap_or(END_OF_CHAIN);
        }
        if size != u64::MAX {
            out.truncate(usize::try_from(size).unwrap_or(usize::MAX));
        }
        Ok(out)
    }

    /// 读一个流。不足 cutoff 的存在迷你流里（MS-CFB §2.6.1）。
    pub(crate) fn read(&self, entry: &Entry) -> Result<Vec<u8>, Error> {
        // 声明的大小不可能超过文件本身；v3 文件大小字段的高 32 位有时是垃圾（MS-CFB §2.6.1）
        let size = entry.size.min(self.data.len() as u64);
        if size > MAX_STREAM {
            return Err(Error::TooLarge("OLE stream"));
        }
        if size >= self.cutoff {
            return self.chain(entry.start, size);
        }
        let mut out = Vec::with_capacity(size as usize);
        let mut cur = entry.start;
        let mut hops = 0usize;
        while cur < END_OF_CHAIN && (out.len() as u64) < size {
            if hops > self.mini_fat.len() {
                return Err(Error::Malformed("OLE mini chain loops".into()));
            }
            hops += 1;
            let at = cur as usize * self.mini_sector;
            let Some(body) = self.mini_stream.get(at..at + self.mini_sector) else {
                break;
            };
            out.extend_from_slice(body);
            cur = self
                .mini_fat
                .get(cur as usize)
                .copied()
                .unwrap_or(END_OF_CHAIN);
        }
        out.truncate(size as usize);
        Ok(out)
    }

    /// 某个存储下的子项（红黑树中序展开；走过的不再走，坏树也不会死循环）。`None` 表示根。
    pub(crate) fn children(&self, parent: Option<&Entry>) -> Vec<&Entry> {
        let child = parent.map_or(self.entries[0].child, |p| p.child);
        let mut seen = vec![false; self.entries.len()];
        let mut out = Vec::new();
        let mut stack = vec![child];
        while let Some(i) = stack.pop() {
            if i == NONE {
                continue;
            }
            let Some(e) = self.entries.get(i as usize) else {
                continue;
            };
            if std::mem::replace(&mut seen[i as usize], true) {
                continue;
            }
            out.push(e);
            stack.push(e.left);
            stack.push(e.right);
        }
        out
    }
}

fn sector_slice(data: &[u8], sector: usize, n: u32) -> Option<&[u8]> {
    // 头占第一个“扇区”：4 KB 扇区的文件也是如此，第 n 个扇区从 (n + 1) * sector 开始
    let start = (n as usize).checked_add(1)?.checked_mul(sector)?;
    data.get(start..start.checked_add(sector)?)
}

fn parse_entry(d: &[u8]) -> Entry {
    let name_len = usize::from(u16_at(d, 64).unwrap_or(0)).min(64);
    let units: Vec<u16> = d[..name_len.saturating_sub(2)]
        .as_chunks::<2>()
        .0
        .iter()
        .map(|c| u16::from_le_bytes([c[0], c[1]]))
        .collect();
    Entry {
        name: String::from_utf16_lossy(&units),
        is_stream: d[66] == 2,
        left: u32_at(d, 68).unwrap_or(NONE),
        right: u32_at(d, 72).unwrap_or(NONE),
        child: u32_at(d, 76).unwrap_or(NONE),
        start: u32_at(d, 116).unwrap_or(END_OF_CHAIN),
        size: u64_at(d, 120).unwrap_or(0),
    }
}

#[cfg(test)]
pub(crate) mod tests {
    use super::*;

    const SECTOR: usize = 512;

    /// 测试用：造一个只有普通流（都不小于 4096 字节，或全部放进迷你流）的复合文件。
    /// `files`：名字、内容、父目录下标（目录项下标，0 是根）；内容空且 `storage` 为真时是存储。
    pub(crate) struct Node<'a> {
        pub(crate) name: &'a str,
        pub(crate) data: &'a [u8],
        pub(crate) storage: bool,
        pub(crate) parent: usize,
    }

    pub(crate) fn build(nodes: &[Node<'_>]) -> Vec<u8> {
        // 所有流都按普通扇区存：把 cutoff 写成 0，这样读取走 FAT 链，测试不用造迷你流
        let total: usize = nodes.iter().map(|n| n.data.len().div_ceil(SECTOR)).sum();
        let dir_sectors = (nodes.len() + 1).div_ceil(4);
        let fat_needed = (total + dir_sectors + 8).div_ceil(SECTOR / 4);
        let mut fat = vec![END_OF_CHAIN; fat_needed * SECTOR / 4];
        for f in fat.iter_mut().take(fat_needed) {
            *f = 0xFFFF_FFFD; // FATSECT
        }
        let dir_start = fat_needed as u32;
        for i in 0..dir_sectors {
            fat[fat_needed + i] = if i + 1 < dir_sectors {
                (fat_needed + i + 1) as u32
            } else {
                END_OF_CHAIN
            };
        }
        let mut next = fat_needed + dir_sectors;
        let mut body = Vec::new();
        let mut starts = Vec::new();
        for n in nodes {
            let count = n.data.len().div_ceil(SECTOR);
            starts.push(if count == 0 {
                END_OF_CHAIN
            } else {
                next as u32
            });
            for i in 0..count {
                fat[next + i] = if i + 1 < count {
                    (next + i + 1) as u32
                } else {
                    END_OF_CHAIN
                };
            }
            next += count;
            let mut d = n.data.to_vec();
            d.resize(count * SECTOR, 0);
            body.extend(d);
        }
        // 目录：每个父目录的子项用 right 串成一条（合法的退化树）
        let mut dir = vec![0u8; dir_sectors * SECTOR];
        let mut entries: Vec<[u32; 3]> = vec![[NONE; 3]; nodes.len() + 1];
        let mut last_child: Vec<Option<usize>> = vec![None; nodes.len() + 1];
        for (i, n) in nodes.iter().enumerate() {
            let id = i + 1;
            match last_child[n.parent] {
                Some(prev) => entries[prev][1] = id as u32,
                None => entries[n.parent][2] = id as u32,
            }
            last_child[n.parent] = Some(id);
        }
        let write =
            |dir: &mut [u8], id: usize, name: &str, ty: u8, start: u32, size: u64, l: [u32; 3]| {
                let e = &mut dir[id * 128..(id + 1) * 128];
                let units: Vec<u16> = name.encode_utf16().collect();
                for (k, u) in units.iter().enumerate() {
                    e[k * 2..k * 2 + 2].copy_from_slice(&u.to_le_bytes());
                }
                e[64..66].copy_from_slice(&((units.len() as u16 + 1) * 2).to_le_bytes());
                e[66] = ty;
                e[68..72].copy_from_slice(&l[0].to_le_bytes());
                e[72..76].copy_from_slice(&l[1].to_le_bytes());
                e[76..80].copy_from_slice(&l[2].to_le_bytes());
                e[116..120].copy_from_slice(&start.to_le_bytes());
                e[120..128].copy_from_slice(&size.to_le_bytes());
            };
        write(&mut dir, 0, "Root Entry", 5, END_OF_CHAIN, 0, entries[0]);
        for (i, n) in nodes.iter().enumerate() {
            let ty = if n.storage { 1 } else { 2 };
            write(
                &mut dir,
                i + 1,
                n.name,
                ty,
                starts[i],
                n.data.len() as u64,
                entries[i + 1],
            );
        }
        let mut h = vec![0u8; SECTOR];
        h[..8].copy_from_slice(&SIGNATURE);
        h[30..32].copy_from_slice(&9u16.to_le_bytes());
        h[32..34].copy_from_slice(&6u16.to_le_bytes());
        h[44..48].copy_from_slice(&(fat_needed as u32).to_le_bytes());
        h[48..52].copy_from_slice(&dir_start.to_le_bytes());
        h[56..60].copy_from_slice(&0u32.to_le_bytes());
        h[60..64].copy_from_slice(&END_OF_CHAIN.to_le_bytes());
        h[68..72].copy_from_slice(&END_OF_CHAIN.to_le_bytes());
        for i in 0..109usize {
            let v = if i < fat_needed { i as u32 } else { NONE };
            h[76 + i * 4..80 + i * 4].copy_from_slice(&v.to_le_bytes());
        }
        let mut out = h;
        out.extend(fat.iter().flat_map(|v| v.to_le_bytes()));
        out.extend(dir);
        out.extend(body);
        out
    }

    #[test]
    fn reads_tree_and_streams() {
        let long = vec![b'z'; 1500];
        let bytes = build(&[
            Node {
                name: "a",
                data: b"hello",
                storage: false,
                parent: 0,
            },
            Node {
                name: "dir",
                data: b"",
                storage: true,
                parent: 0,
            },
            Node {
                name: "b",
                data: &long,
                storage: false,
                parent: 2,
            },
        ]);
        let ole = Ole::open(&bytes).unwrap();
        let root = ole.children(None);
        let mut names: Vec<&str> = root.iter().map(|e| e.name.as_str()).collect();
        names.sort_unstable();
        assert_eq!(names, ["a", "dir"]);
        let a = root.iter().find(|e| e.name == "a").unwrap();
        assert!(a.is_stream);
        assert_eq!(ole.read(a).unwrap(), b"hello");
        let dir = root.iter().find(|e| e.name == "dir").unwrap();
        let inner = ole.children(Some(dir));
        assert_eq!(inner.len(), 1);
        assert_eq!(ole.read(inner[0]).unwrap(), long);
    }

    #[test]
    fn hostile_files_fail_cleanly() {
        assert!(Ole::open(b"not ole").is_err());
        let mut bytes = build(&[Node {
            name: "a",
            data: b"x",
            storage: false,
            parent: 0,
        }]);
        // 目录链指向自己：报错，不卡死
        let fat_entry = 512 + 4;
        bytes[fat_entry..fat_entry + 4].copy_from_slice(&1u32.to_le_bytes());
        assert!(Ole::open(&bytes).is_err());
        // 扇区大小非法
        let mut bad = build(&[]);
        bad[30] = 3;
        assert!(Ole::open(&bad).is_err());
    }
}
