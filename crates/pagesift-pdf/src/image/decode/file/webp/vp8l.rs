//! WebP 无损（VP8L，WebP Lossless Bitstream Specification）。解出 ARGB，再转灰度由上层做。

use super::tables::CODE_TO_PLANE;

const ORDER: [usize; 19] = [
    17, 18, 0, 1, 2, 3, 4, 5, 16, 6, 7, 8, 9, 10, 11, 12, 13, 14, 15,
];
const PRIMARY_BITS: u32 = 8;

struct Bits<'a> {
    data: &'a [u8],
    byte: usize,
    acc: u64,
    n: u32,
}

impl<'a> Bits<'a> {
    fn new(data: &'a [u8]) -> Self {
        Bits {
            data,
            byte: 0,
            acc: 0,
            n: 0,
        }
    }

    #[inline]
    fn fill(&mut self) {
        while self.n <= 56 {
            let b = self.data.get(self.byte).copied().unwrap_or(0);
            self.byte += 1;
            self.acc |= u64::from(b) << self.n;
            self.n += 8;
        }
    }

    #[inline]
    fn read(&mut self, count: u32) -> u32 {
        if count == 0 {
            return 0;
        }
        if self.n < count {
            self.fill();
        }
        let v = (self.acc & ((1u64 << count) - 1)) as u32;
        self.acc >>= count;
        self.n -= count;
        v
    }

    /// 读过了数据末尾很多：坏文件，别再解下去。
    fn exhausted(&self) -> bool {
        self.byte > self.data.len() + 8 + (self.n / 8) as usize
    }
}

/// 规范前缀码（最长 15 位）：短码查表，长码按规范码逐位走。
struct Huffman {
    primary: Vec<(u16, u8)>,
    /// 每个长度的（首码、首个符号下标、个数）。
    first: [(u32, u32, u32); 16],
    symbols: Vec<u16>,
    single: Option<u16>,
}

impl Huffman {
    fn new(lengths: &[u8]) -> Result<Huffman, String> {
        let mut counts = [0u32; 16];
        for &l in lengths {
            if l > 15 {
                return Err("WebP: wrong code length".into());
            }
            if l > 0 {
                counts[usize::from(l)] += 1;
            }
        }
        let total: u32 = counts.iter().sum();
        if total == 0 {
            return Ok(Huffman {
                primary: Vec::new(),
                first: [(0, 0, 0); 16],
                symbols: Vec::new(),
                single: Some(0),
            });
        }
        if total == 1 {
            let s = lengths.iter().position(|&l| l > 0).unwrap_or(0) as u16;
            return Ok(Huffman {
                primary: Vec::new(),
                first: [(0, 0, 0); 16],
                symbols: Vec::new(),
                single: Some(s),
            });
        }
        let mut first = [(0u32, 0u32, 0u32); 16];
        let (mut code, mut index) = (0u32, 0u32);
        for len in 1..16usize {
            code <<= 1;
            first[len] = (code, index, counts[len]);
            code += counts[len];
            if code > 1 << len {
                return Err("WebP: oversubscribed code".into());
            }
            index += counts[len];
        }
        if code != 1 << 15 {
            return Err("WebP: incomplete code".into());
        }
        let mut symbols = vec![0u16; total as usize];
        let mut next: Vec<u32> = first.iter().map(|f| f.1).collect();
        for (s, &l) in lengths.iter().enumerate() {
            if l > 0 {
                symbols[next[usize::from(l)] as usize] = s as u16;
                next[usize::from(l)] += 1;
            }
        }
        let mut primary = vec![(0u16, 0u8); 1 << PRIMARY_BITS];
        for (len, &(c, i, n)) in first
            .iter()
            .enumerate()
            .take(PRIMARY_BITS as usize + 1)
            .skip(1)
        {
            for k in 0..n {
                let rev = (c + k).reverse_bits() >> (32 - len as u32);
                let step = 1usize << len;
                let mut at = rev as usize;
                while at < primary.len() {
                    primary[at] = (symbols[(i + k) as usize], len as u8);
                    at += step;
                }
            }
        }
        Ok(Huffman {
            primary,
            first,
            symbols,
            single: None,
        })
    }

    #[inline]
    fn read(&self, b: &mut Bits<'_>) -> u16 {
        if let Some(s) = self.single {
            return s;
        }
        if b.n < 15 {
            b.fill();
        }
        let (sym, len) = self.primary[(b.acc & ((1 << PRIMARY_BITS) - 1)) as usize];
        if len > 0 {
            b.acc >>= len;
            b.n -= u32::from(len);
            return sym;
        }
        let mut code = 0u32;
        for len in 1..16usize {
            code = code << 1 | (b.acc & 1) as u32;
            b.acc >>= 1;
            b.n -= 1;
            let (c, i, n) = self.first[len];
            if code >= c && code - c < n {
                return self.symbols[(i + code - c) as usize];
            }
        }
        0
    }
}

fn read_code(b: &mut Bits<'_>, alphabet: usize) -> Result<Huffman, String> {
    let mut lengths = vec![0u8; alphabet];
    if b.read(1) == 1 {
        let two = b.read(1) == 1;
        let first_bits = if b.read(1) == 1 { 8 } else { 1 };
        let s0 = b.read(first_bits) as usize;
        *lengths.get_mut(s0).ok_or("WebP: wrong symbol")? = 1;
        if two {
            let s1 = b.read(8) as usize;
            *lengths.get_mut(s1).ok_or("WebP: wrong symbol")? = 1;
        }
        return Huffman::new(&lengths);
    }
    let count = b.read(4) as usize + 4;
    let mut cl = [0u8; 19];
    for &o in ORDER.iter().take(count) {
        cl[o] = b.read(3) as u8;
    }
    let meta = Huffman::new(&cl)?;
    let mut max_symbol = if b.read(1) == 1 {
        let bits = 2 + 2 * b.read(3);
        2 + b.read(bits) as usize
    } else {
        alphabet
    };
    if max_symbol > alphabet {
        return Err("WebP: wrong symbol count".into());
    }
    let (mut symbol, mut prev) = (0usize, 8u8);
    while symbol < alphabet && max_symbol > 0 {
        max_symbol -= 1;
        let c = meta.read(b);
        if c < 16 {
            lengths[symbol] = c as u8;
            symbol += 1;
            if c != 0 {
                prev = c as u8;
            }
        } else {
            let (extra, offset) = match c {
                16 => (2, 3),
                17 => (3, 3),
                _ => (7, 11),
            };
            let repeat = b.read(extra) as usize + offset;
            if symbol + repeat > alphabet {
                return Err("WebP: code lengths overflow".into());
            }
            let v = if c == 16 { prev } else { 0 };
            lengths[symbol..symbol + repeat].fill(v);
            symbol += repeat;
        }
    }
    Huffman::new(&lengths)
}

#[inline]
fn prefix_value(c: u32, b: &mut Bits<'_>) -> usize {
    if c < 4 {
        return c as usize + 1;
    }
    let extra = (c - 2) >> 1;
    let offset = (2 + (c & 1)) << extra;
    (offset + b.read(extra) + 1) as usize
}

fn subsampled(size: usize, bits: u32) -> usize {
    (size + (1 << bits) - 1) >> bits
}

/// 解一块图像的熵编码部分。`top` 为真时才可能有元前缀码（分块选码）。
fn decode_image(
    b: &mut Bits<'_>,
    width: usize,
    height: usize,
    top: bool,
) -> Result<Vec<u32>, String> {
    let cache_bits = if b.read(1) == 1 {
        let n = b.read(4);
        if !(1..=11).contains(&n) {
            return Err("WebP: wrong color cache size".into());
        }
        n
    } else {
        0
    };
    let mut meta_bits = 0u32;
    let mut meta_width = 1usize;
    let mut meta: Vec<u32> = Vec::new();
    if top && b.read(1) == 1 {
        meta_bits = b.read(3) + 2;
        meta_width = subsampled(width, meta_bits);
        let meta_height = subsampled(height, meta_bits);
        meta = decode_image(b, meta_width, meta_height, false)?;
    }
    let groups = meta
        .iter()
        .map(|p| ((p >> 8) & 0xFFFF) as usize + 1)
        .max()
        .unwrap_or(1);
    let cache_size = if cache_bits > 0 {
        1usize << cache_bits
    } else {
        0
    };
    let green_size = 256 + 24 + cache_size;
    let mut codes = Vec::with_capacity(groups.min(4096));
    for _ in 0..groups {
        if b.exhausted() {
            return Err("WebP: truncated data".into());
        }
        codes.push([
            read_code(b, green_size)?,
            read_code(b, 256)?,
            read_code(b, 256)?,
            read_code(b, 256)?,
            read_code(b, 40)?,
        ]);
    }
    let total = width * height;
    let mut out = vec![0u32; total];
    let mut cache = vec![0u32; cache_size];
    let (mut i, mut hashed) = (0usize, 0usize);
    let mut group = 0usize;
    while i < total {
        let (x, y) = (i % width, i / width);
        // 一次复制可能跨过分块边界，所以每步都查
        if !meta.is_empty() {
            group =
                ((meta[(y >> meta_bits) * meta_width + (x >> meta_bits)] >> 8) & 0xFFFF) as usize;
        }
        let code = &codes[group];
        let s = u32::from(code[0].read(b));
        if s < 256 {
            let r = u32::from(code[1].read(b));
            let bl = u32::from(code[2].read(b));
            let a = u32::from(code[3].read(b));
            out[i] = a << 24 | r << 16 | s << 8 | bl;
            i += 1;
        } else if s < 280 {
            let len = prefix_value(s - 256, b);
            let dcode = u32::from(code[4].read(b));
            let plane = prefix_value(dcode, b);
            let dist = if plane > 120 {
                plane - 120
            } else {
                let c = usize::from(CODE_TO_PLANE[plane - 1]);
                let d = (c >> 4) * width + 8 - (c & 0xF).min(8 + (c >> 4) * width);
                d.max(1)
            };
            if dist > i || i + len > total {
                return Err("WebP: copy out of range".into());
            }
            for k in i..i + len {
                out[k] = out[k - dist];
            }
            i += len;
        } else {
            let idx = (s - 280) as usize;
            // 用缓存前把落下的像素补进去
            while hashed < i {
                let p = out[hashed];
                cache[(0x1E35_A7BDu32.wrapping_mul(p) >> (32 - cache_bits)) as usize] = p;
                hashed += 1;
            }
            out[i] = *cache.get(idx).ok_or("WebP: wrong cache index")?;
            i += 1;
        }
        if b.exhausted() {
            return Err("WebP: truncated data".into());
        }
    }
    Ok(out)
}

enum Transform {
    Predictor {
        bits: u32,
        data: Vec<u32>,
        width: usize,
    },
    Color {
        bits: u32,
        data: Vec<u32>,
        width: usize,
    },
    SubtractGreen,
    Index {
        bits: u32,
        palette: Vec<u32>,
        width: usize,
    },
}

/// 解码。返回 ARGB、宽、高。`check` 在分配前校验尺寸。
pub(super) fn decode(
    data: &[u8],
    check: &dyn Fn(u64, u64) -> Result<(), String>,
) -> Result<(Vec<u32>, usize, usize), String> {
    if data.first() != Some(&0x2F) {
        return Err("WebP: wrong lossless signature".into());
    }
    let mut b = Bits::new(&data[1..]);
    let width = b.read(14) as usize + 1;
    let height = b.read(14) as usize + 1;
    let _alpha = b.read(1);
    if b.read(3) != 0 {
        return Err("WebP: unknown lossless version".into());
    }
    check(width as u64, height as u64)?;
    let mut transforms = Vec::new();
    let mut xsize = width;
    let mut seen = 0u8;
    while b.read(1) == 1 {
        let kind = b.read(2);
        if seen & (1 << kind) != 0 {
            return Err("WebP: repeated transform".into());
        }
        seen |= 1 << kind;
        transforms.push(match kind {
            0 | 1 => {
                let bits = b.read(3) + 2;
                let data = decode_image(
                    &mut b,
                    subsampled(xsize, bits),
                    subsampled(height, bits),
                    false,
                )?;
                if kind == 0 {
                    Transform::Predictor {
                        bits,
                        data,
                        width: xsize,
                    }
                } else {
                    Transform::Color {
                        bits,
                        data,
                        width: xsize,
                    }
                }
            }
            2 => Transform::SubtractGreen,
            _ => {
                let n = b.read(8) as usize + 1;
                let bits = match n {
                    1..=2 => 3,
                    3..=4 => 2,
                    5..=16 => 1,
                    _ => 0,
                };
                let mut palette = decode_image(&mut b, n, 1, false)?;
                for i in 1..palette.len() {
                    palette[i] = add(palette[i], palette[i - 1]);
                }
                let t = Transform::Index {
                    bits,
                    palette,
                    width: xsize,
                };
                xsize = subsampled(xsize, bits);
                t
            }
        });
    }
    let mut px = decode_image(&mut b, xsize, height, true)?;
    for t in transforms.iter().rev() {
        match t {
            Transform::SubtractGreen => {
                for p in &mut px {
                    let g = (*p >> 8) & 0xFF;
                    let r = (((*p >> 16) & 0xFF) + g) & 0xFF;
                    let bl = ((*p & 0xFF) + g) & 0xFF;
                    *p = (*p & 0xFF00_FF00) | r << 16 | bl;
                }
            }
            Transform::Color { bits, data, width } => color(&mut px, *width, *bits, data),
            Transform::Predictor { bits, data, width } => predictor(&mut px, *width, *bits, data),
            Transform::Index {
                bits,
                palette,
                width,
            } => {
                px = unpack(&px, *width, height, *bits, palette);
            }
        }
    }
    Ok((px, width, height))
}

#[inline]
fn add(a: u32, b: u32) -> u32 {
    let ag = (a & 0xFF00_FF00).wrapping_add(b & 0xFF00_FF00) & 0xFF00_FF00;
    let rb = (a & 0x00FF_00FF).wrapping_add(b & 0x00FF_00FF) & 0x00FF_00FF;
    ag | rb
}

fn unpack(px: &[u32], width: usize, height: usize, bits: u32, palette: &[u32]) -> Vec<u32> {
    let mut out = vec![0u32; width * height];
    let depth = 8u32 >> bits;
    let mask = (1u32 << depth) - 1;
    let packed_width = subsampled(width, bits);
    for y in 0..height {
        for x in 0..width {
            let p = px[y * packed_width + (x >> bits)];
            let g = (p >> 8) & 0xFF;
            let shift = (x & ((1 << bits) - 1)) as u32 * depth;
            let idx = if bits == 0 { g } else { (g >> shift) & mask };
            out[y * width + x] = palette.get(idx as usize).copied().unwrap_or(0);
        }
    }
    out
}

#[inline]
fn delta(t: u32, c: u32) -> u32 {
    ((i32::from(t as u8 as i8) * i32::from(c as u8 as i8)) >> 5) as u32
}

fn color(px: &mut [u32], width: usize, bits: u32, data: &[u32]) {
    let bw = subsampled(width, bits);
    for (i, p) in px.iter_mut().enumerate() {
        let (x, y) = (i % width, i / width);
        let e = data[(y >> bits) * bw + (x >> bits)];
        let (g2r, g2b, r2b) = (e & 0xFF, (e >> 8) & 0xFF, (e >> 16) & 0xFF);
        let g = (*p >> 8) & 0xFF;
        let r = ((*p >> 16) & 0xFF).wrapping_add(delta(g2r, g)) & 0xFF;
        let bl = (*p & 0xFF)
            .wrapping_add(delta(g2b, g))
            .wrapping_add(delta(r2b, r))
            & 0xFF;
        *p = (*p & 0xFF00_FF00) | r << 16 | bl;
    }
}

#[inline]
fn avg2(a: u32, b: u32) -> u32 {
    (((a ^ b) & 0xFEFE_FEFE) >> 1).wrapping_add(a & b)
}

#[inline]
fn channels(p: u32) -> [i32; 4] {
    [
        (p >> 24) as i32,
        ((p >> 16) & 0xFF) as i32,
        ((p >> 8) & 0xFF) as i32,
        (p & 0xFF) as i32,
    ]
}

fn pack(c: [i32; 4]) -> u32 {
    let k = |v: i32| v.clamp(0, 255) as u32;
    k(c[0]) << 24 | k(c[1]) << 16 | k(c[2]) << 8 | k(c[3])
}

fn select(l: u32, t: u32, tl: u32) -> u32 {
    let (cl, ct, ctl) = (channels(l), channels(t), channels(tl));
    let (mut pl, mut pt) = (0, 0);
    for i in 0..4 {
        let p = cl[i] + ct[i] - ctl[i];
        pl += (p - cl[i]).abs();
        pt += (p - ct[i]).abs();
    }
    if pl < pt { l } else { t }
}

fn clamp_full(a: u32, b: u32, c: u32) -> u32 {
    let (x, y, z) = (channels(a), channels(b), channels(c));
    pack(std::array::from_fn(|i| x[i] + y[i] - z[i]))
}

fn clamp_half(a: u32, b: u32) -> u32 {
    let (x, y) = (channels(a), channels(b));
    pack(std::array::from_fn(|i| x[i] + (x[i] - y[i]) / 2))
}

fn predictor(px: &mut [u32], width: usize, bits: u32, data: &[u32]) {
    let bw = subsampled(width, bits);
    let black = 0xFF00_0000u32;
    for i in 0..px.len() {
        let (x, y) = (i % width, i / width);
        let pred = if i == 0 {
            black
        } else if y == 0 {
            px[i - 1]
        } else if x == 0 {
            px[i - width]
        } else {
            let mode = (data[(y >> bits) * bw + (x >> bits)] >> 8) & 0xF;
            let l = px[i - 1];
            let t = px[i - width];
            let tl = px[i - width - 1];
            // 最右一列的右上是本行第一个像素：平铺的数组里正好是这个下标
            let tr = px[i - width + 1];
            match mode {
                1 => l,
                2 => t,
                3 => tr,
                4 => tl,
                5 => avg2(avg2(l, tr), t),
                6 => avg2(l, tl),
                7 => avg2(l, t),
                8 => avg2(tl, t),
                9 => avg2(t, tr),
                10 => avg2(avg2(l, tl), avg2(t, tr)),
                11 => select(l, t, tl),
                12 => clamp_full(l, t, tl),
                13 => clamp_half(avg2(l, t), tl),
                _ => black,
            }
        };
        px[i] = add(px[i], pred);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn code_tables() {
        // 两个等长的码：0 → 3、1 → 7
        let mut l = vec![0u8; 10];
        l[3] = 1;
        l[7] = 1;
        let h = Huffman::new(&l).unwrap();
        let mut b = Bits::new(&[0b0000_0010]);
        assert_eq!(h.read(&mut b), 3);
        assert_eq!(h.read(&mut b), 7);
        // 超额的码表报错
        assert!(Huffman::new(&[1, 1, 1]).is_err());
    }

    #[test]
    fn broken_data_does_not_panic() {
        let ok = |_: u64, _: u64| Ok(());
        assert!(decode(&[], &ok).is_err());
        assert!(decode(&[0x2F, 0xFF, 0xFF, 0xFF, 0x1F], &ok).is_err());
        let mut d = vec![0x2F, 3, 0, 0, 0];
        d.extend_from_slice(&[0xFF; 32]);
        let _ = decode(&d, &ok);
    }
}
