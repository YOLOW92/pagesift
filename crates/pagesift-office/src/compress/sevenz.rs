//! 7z 容器（照 7-Zip 的 7zFormat.txt）：目录可以压缩（EncodedHeader），每个 folder 一个编码器，
//! 支持 Copy、LZMA、LZMA2。加密报 [`Error::Encrypted`]，BCJ 这类多编码器链报不支持。CRC 不验证。

use super::lzma;
use crate::Error;

pub(crate) const MAGIC: [u8; 6] = [b'7', b'z', 0xBC, 0xAF, 0x27, 0x1C];

const K_END: u8 = 0x00;
const K_HEADER: u8 = 0x01;
const K_MAIN_STREAMS: u8 = 0x04;
const K_FILES: u8 = 0x05;
const K_PACK_INFO: u8 = 0x06;
const K_UNPACK_INFO: u8 = 0x07;
const K_SUBSTREAMS: u8 = 0x08;
const K_SIZE: u8 = 0x09;
const K_CRC: u8 = 0x0A;
const K_FOLDER: u8 = 0x0B;
const K_UNPACK_SIZE: u8 = 0x0C;
const K_NUM_UNPACK: u8 = 0x0D;
const K_EMPTY_STREAM: u8 = 0x0E;
const K_NAMES: u8 = 0x11;
const K_ENCODED_HEADER: u8 = 0x17;

/// 目录里声明的个数不可能超过这个：挡住恶意的超大计数。
const MAX_ITEMS: u64 = 1 << 20;

fn bad(m: &str) -> Error {
    Error::Malformed(format!("7z: {m}"))
}

struct Reader<'a> {
    b: &'a [u8],
    pos: usize,
}

impl Reader<'_> {
    fn byte(&mut self) -> Result<u8, Error> {
        let v = *self.b.get(self.pos).ok_or_else(|| bad("truncated"))?;
        self.pos += 1;
        Ok(v)
    }

    /// 7z 的变长整数：第一个字节高位的 1 的个数表示后面还有几个字节。
    fn number(&mut self) -> Result<u64, Error> {
        let first = self.byte()?;
        let mut mask = 0x80u8;
        let mut value = 0u64;
        for i in 0..8 {
            if first & mask == 0 {
                let high = u64::from(first & (mask.wrapping_sub(1)));
                return Ok(value | high << (8 * i));
            }
            value |= u64::from(self.byte()?) << (8 * i);
            mask >>= 1;
        }
        Ok(value)
    }

    fn count(&mut self) -> Result<usize, Error> {
        let n = self.number()?;
        if n > MAX_ITEMS {
            return Err(Error::TooLarge("7z entry count"));
        }
        Ok(n as usize)
    }

    fn skip(&mut self, n: usize) -> Result<(), Error> {
        self.pos = self.pos.checked_add(n).ok_or_else(|| bad("truncated"))?;
        if self.pos > self.b.len() {
            return Err(bad("truncated"));
        }
        Ok(())
    }

    /// 位向量：高位在前。
    fn bits(&mut self, n: usize) -> Result<Vec<bool>, Error> {
        let mut out = Vec::with_capacity(n);
        let (mut cur, mut mask) = (0u8, 0u8);
        for _ in 0..n {
            if mask == 0 {
                cur = self.byte()?;
                mask = 0x80;
            }
            out.push(cur & mask != 0);
            mask >>= 1;
        }
        Ok(out)
    }

    /// 可选的“全部定义”位向量。
    fn optional_bits(&mut self, n: usize) -> Result<Vec<bool>, Error> {
        if self.byte()? != 0 {
            Ok(vec![true; n])
        } else {
            self.bits(n)
        }
    }

    fn skip_digests(&mut self, n: usize) -> Result<usize, Error> {
        let defined = self.optional_bits(n)?;
        let known = defined.iter().filter(|d| **d).count();
        self.skip(known * 4)?;
        Ok(known)
    }
}

#[derive(Default)]
struct Folder {
    id: Vec<u8>,
    props: Vec<u8>,
    size: u64,
    coders: usize,
}

#[derive(Default)]
struct Streams {
    pack_pos: u64,
    pack_sizes: Vec<u64>,
    folders: Vec<Folder>,
    /// 每个 folder 里每个文件的大小。
    sizes: Vec<Vec<u64>>,
}

fn read_streams(r: &mut Reader<'_>) -> Result<Streams, Error> {
    let mut s = Streams::default();
    loop {
        match r.byte()? {
            K_END => break,
            K_PACK_INFO => {
                s.pack_pos = r.number()?;
                let n = r.count()?;
                loop {
                    match r.byte()? {
                        K_END => break,
                        K_SIZE => {
                            s.pack_sizes = (0..n).map(|_| r.number()).collect::<Result<_, _>>()?;
                        }
                        K_CRC => {
                            r.skip_digests(n)?;
                        }
                        _ => return Err(bad("wrong pack info")),
                    }
                }
            }
            K_UNPACK_INFO => read_unpack(r, &mut s)?,
            K_SUBSTREAMS => read_substreams(r, &mut s)?,
            _ => return Err(bad("wrong streams info")),
        }
    }
    if s.sizes.is_empty() {
        s.sizes = s.folders.iter().map(|f| vec![f.size]).collect();
    }
    Ok(s)
}

fn read_unpack(r: &mut Reader<'_>, s: &mut Streams) -> Result<(), Error> {
    if r.byte()? != K_FOLDER {
        return Err(bad("wrong unpack info"));
    }
    let n = r.count()?;
    if r.byte()? != 0 {
        return Err(Error::Unsupported("external 7z folder info".into()));
    }
    let mut out_counts = Vec::with_capacity(n);
    for _ in 0..n {
        let coders = r.count()?;
        if coders == 0 || coders > 32 {
            return Err(bad("wrong coder count"));
        }
        let mut f = Folder {
            coders,
            ..Default::default()
        };
        let (mut total_in, mut total_out) = (0usize, 0usize);
        for _ in 0..coders {
            let flags = r.byte()?;
            let id_len = usize::from(flags & 0x0F);
            let start = r.pos;
            r.skip(id_len)?;
            f.id = r.b[start..start + id_len].to_vec();
            let (mut ins, mut outs) = (1usize, 1usize);
            if flags & 0x10 != 0 {
                ins = r.count()?;
                outs = r.count()?;
            }
            total_in += ins;
            total_out += outs;
            if flags & 0x20 != 0 {
                let len = r.count()?;
                let start = r.pos;
                r.skip(len)?;
                f.props = r.b[start..start + len].to_vec();
            }
        }
        let binds = total_out.saturating_sub(1);
        for _ in 0..binds {
            r.number()?;
            r.number()?;
        }
        let packed = total_in.saturating_sub(binds);
        if packed > 1 {
            for _ in 0..packed {
                r.number()?;
            }
        }
        out_counts.push(total_out);
        s.folders.push(f);
    }
    if r.byte()? != K_UNPACK_SIZE {
        return Err(bad("wrong unpack sizes"));
    }
    for (f, outs) in s.folders.iter_mut().zip(&out_counts) {
        for i in 0..*outs {
            let v = r.number()?;
            // 单编码器时只有一个输出，多编码器时后面会报不支持
            if i + 1 == *outs {
                f.size = v;
            }
        }
    }
    loop {
        match r.byte()? {
            K_END => return Ok(()),
            K_CRC => {
                r.skip_digests(n)?;
            }
            _ => return Err(bad("wrong unpack info")),
        }
    }
}

fn read_substreams(r: &mut Reader<'_>, s: &mut Streams) -> Result<(), Error> {
    let mut counts = vec![1usize; s.folders.len()];
    let mut id = r.byte()?;
    if id == K_NUM_UNPACK {
        for c in &mut counts {
            *c = r.count()?;
        }
        id = r.byte()?;
    }
    for (f, &n) in s.folders.iter().zip(&counts) {
        let mut sizes = Vec::with_capacity(n);
        if n > 0 {
            let mut sum = 0u64;
            for _ in 1..n {
                if id != K_SIZE {
                    return Err(bad("wrong substream sizes"));
                }
                let v = r.number()?;
                sum = sum.checked_add(v).ok_or_else(|| bad("wrong sizes"))?;
                sizes.push(v);
            }
            sizes.push(f.size.checked_sub(sum).ok_or_else(|| bad("wrong sizes"))?);
        }
        s.sizes.push(sizes);
    }
    if id == K_SIZE {
        id = r.byte()?;
    }
    let total: usize = counts.iter().sum();
    loop {
        match id {
            K_END => return Ok(()),
            K_CRC => {
                // 只有文件数不为 1 的 folder 和没有 CRC 的 folder 才在这里；个数按最大可能读
                r.skip_digests(total)?;
            }
            _ => return Err(bad("wrong substreams")),
        }
        id = r.byte()?;
    }
}

fn unpack(input: &[u8], f: &Folder, limit: usize) -> Result<Vec<u8>, Error> {
    if f.coders != 1 {
        return Err(Error::Unsupported(
            "7z coder chains (BCJ) are not supported".into(),
        ));
    }
    let size = usize::try_from(f.size).map_err(|_| Error::TooLarge("decompressed size"))?;
    if size > limit {
        return Err(Error::TooLarge("decompressed size"));
    }
    match f.id.as_slice() {
        [0] => input
            .get(..size)
            .map(<[u8]>::to_vec)
            .ok_or_else(|| bad("truncated data")),
        [3, 1, 1] => lzma::decode_raw(input, &f.props, size),
        [0x21] => {
            let mut out = Vec::with_capacity(size.min(64 << 20));
            lzma::decode_lzma2(input, &mut out, limit)?;
            Ok(out)
        }
        [6, 0xF1, 7, 1] => Err(Error::Encrypted),
        id => Err(Error::Unsupported(format!("7z coder {id:02x?}"))),
    }
}

/// 解出全部文件：（路径，内容）。空文件和目录不返回。
pub(crate) fn decode(input: &[u8], limit: usize) -> Result<Vec<(String, Vec<u8>)>, Error> {
    let head = input.get(..32).ok_or_else(|| bad("truncated header"))?;
    if !head.starts_with(&MAGIC) {
        return Err(bad("wrong signature"));
    }
    let u64_at = |i: usize| u64::from_le_bytes(head[i..i + 8].try_into().unwrap_or([0; 8]));
    let (offset, size) = (u64_at(12), u64_at(20));
    if size == 0 {
        return Ok(Vec::new());
    }
    let base = 32usize;
    let at = usize::try_from(offset)
        .ok()
        .and_then(|o| o.checked_add(base));
    let end = at.and_then(|a| usize::try_from(size).ok().and_then(|s| a.checked_add(s)));
    let mut header = at
        .zip(end)
        .and_then(|(a, e)| input.get(a..e))
        .ok_or_else(|| bad("truncated directory"))?
        .to_vec();
    // 目录本身可能是压缩的，可以套几层
    for _ in 0..4 {
        if header.first() != Some(&K_ENCODED_HEADER) {
            break;
        }
        let mut r = Reader { b: &header, pos: 1 };
        let s = read_streams(&mut r)?;
        let f = s.folders.first().ok_or_else(|| bad("no header folder"))?;
        let start = base.saturating_add(s.pack_pos as usize);
        let pack = input
            .get(start..)
            .ok_or_else(|| bad("truncated header data"))?;
        header = unpack(pack, f, 256 << 20)?;
    }
    let mut r = Reader { b: &header, pos: 0 };
    if r.byte()? != K_HEADER {
        return Err(bad("wrong directory"));
    }
    let mut streams = Streams::default();
    let mut id = r.byte()?;
    if id == K_MAIN_STREAMS {
        streams = read_streams(&mut r)?;
        id = r.byte()?;
    }
    let mut names: Vec<String> = Vec::new();
    let mut empty: Vec<bool> = Vec::new();
    if id == K_FILES {
        let n = r.count()?;
        loop {
            let kind = r.byte()?;
            if kind == K_END {
                break;
            }
            let len = r.count()?;
            let start = r.pos;
            r.skip(len)?;
            let body = &r.b[start..start + len];
            match kind {
                K_NAMES => {
                    let units: Vec<u16> = body
                        .get(1..)
                        .ok_or_else(|| bad("empty names"))?
                        .as_chunks::<2>()
                        .0
                        .iter()
                        .map(|c| u16::from_le_bytes([c[0], c[1]]))
                        .collect();
                    names = units
                        .split(|&u| u == 0)
                        .take(n)
                        .map(String::from_utf16_lossy)
                        .collect();
                }
                K_EMPTY_STREAM => {
                    empty = Reader { b: body, pos: 0 }.bits(n)?;
                }
                _ => {}
            }
        }
    }
    // 把有内容的文件按顺序对应到 folder 里的子流
    let mut out = Vec::new();
    let mut files = names
        .iter()
        .enumerate()
        .filter(|(i, _)| !empty.get(*i).copied().unwrap_or(false))
        .map(|(_, n)| n);
    let mut pack_at = base.saturating_add(streams.pack_pos as usize);
    let mut left = limit;
    for (i, f) in streams.folders.iter().enumerate() {
        let packed = streams.pack_sizes.get(i).copied().unwrap_or(0) as usize;
        let pack = input
            .get(pack_at..pack_at.saturating_add(packed))
            .ok_or_else(|| bad("truncated data"))?;
        pack_at += packed;
        let data = unpack(pack, f, left)?;
        left = left.saturating_sub(data.len());
        let mut pos = 0usize;
        for &size in streams.sizes.get(i).map_or(&[][..], Vec::as_slice) {
            let size = size as usize;
            let chunk = data
                .get(pos..pos.saturating_add(size))
                .ok_or_else(|| bad("wrong file size"))?;
            pos += size;
            let Some(name) = files.next() else {
                return Err(bad("more data than files"));
            };
            out.push((name.replace('\\', "/"), chunk.to_vec()));
        }
    }
    Ok(out)
}
