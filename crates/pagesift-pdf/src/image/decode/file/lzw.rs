//! 变长 LZW，GIF（低位在前，码宽涨得晚）和 TIFF（高位在前，提前一个码涨）共用。

/// 字典上限：码最长 12 位。
const MAX_CODES: usize = 4096;

#[derive(Clone, Copy, PartialEq, Eq)]
pub(super) enum Order {
    /// GIF：低位在前。
    Lsb,
    /// TIFF：高位在前，码宽提前一个码切换。
    Msb,
}

/// 解压。`min_width` 是初始根表的位数（GIF 的 LZW Minimum Code Size；TIFF 固定 8）。
/// 输出到 `max` 字节就停；码流坏了也返回已解出的部分（受损的图能读多少读多少）。
pub(super) fn decode(data: &[u8], min_width: u32, order: Order, max: usize) -> Vec<u8> {
    let clear = 1usize << min_width;
    let end = clear + 1;
    let early = usize::from(order == Order::Msb);
    let mut prefix = vec![0u16; MAX_CODES];
    let mut suffix = vec![0u8; MAX_CODES];
    let mut lengths = vec![0u32; MAX_CODES];
    for i in 0..clear {
        suffix[i] = i as u8;
        lengths[i] = 1;
    }
    let mut out = Vec::new();
    let mut width = min_width + 1;
    let mut next = end + 1;
    let mut prev: Option<usize> = None;
    let (mut buf, mut bits, mut pos) = (0u32, 0u32, 0usize);
    loop {
        while bits < width {
            let Some(&b) = data.get(pos) else {
                return out;
            };
            pos += 1;
            match order {
                Order::Lsb => buf |= u32::from(b) << bits,
                Order::Msb => buf = buf << 8 | u32::from(b),
            }
            bits += 8;
        }
        let mask = (1u32 << width) - 1;
        let code = match order {
            Order::Lsb => {
                let c = buf & mask;
                buf >>= width;
                c
            }
            Order::Msb => (buf >> (bits - width)) & mask,
        } as usize;
        bits -= width;
        if order == Order::Msb {
            buf &= (1u32 << bits) - 1;
        }
        if code == clear {
            width = min_width + 1;
            next = end + 1;
            prev = None;
            continue;
        }
        if code == end {
            return out;
        }
        let first = match prev {
            None => {
                if code >= clear {
                    return out;
                }
                out.push(suffix[code]);
                prev = Some(code);
                if out.len() >= max {
                    return out;
                }
                continue;
            }
            Some(p) => {
                // KwKwK：码正好是下一个要加的
                let source = if code < next {
                    code
                } else if code == next && next < MAX_CODES {
                    p
                } else {
                    return out;
                };
                let start = out.len();
                let len = lengths[source] as usize;
                if start + len > max {
                    return out;
                }
                out.resize(start + len, 0);
                let mut c = source;
                for i in (0..len).rev() {
                    out[start + i] = suffix[c];
                    c = usize::from(prefix[c]);
                }
                let first = out[start];
                if code == next {
                    if out.len() >= max {
                        return out;
                    }
                    out.push(first);
                }
                first
            }
        };
        if next < MAX_CODES {
            let p = prev.unwrap_or(0);
            prefix[next] = p as u16;
            suffix[next] = first;
            lengths[next] = lengths[p] + 1;
            next += 1;
            if next + early >= 1 << width && width < 12 {
                width += 1;
            }
        }
        prev = Some(code);
        if out.len() >= max {
            return out;
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn gif_style_reference_stream() {
        // GIF 规范示例的 10×10 图的前段：最小码宽 2
        let data = [
            0x8C, 0x2D, 0x99, 0x87, 0x2A, 0x1C, 0xDC, 0x33, 0xA0, 0x02, 0x75, 0xEC, 0x95, 0xFA,
            0xA8, 0xDE, 0x60, 0x8C, 0x04, 0x91, 0x4C, 0x01,
        ];
        let out = decode(&data, 2, Order::Lsb, 1_000);
        assert_eq!(out.len(), 100);
        assert_eq!(&out[..10], &[1, 1, 1, 1, 1, 2, 2, 2, 2, 2]);
        assert_eq!(&out[90..], &[2, 2, 2, 2, 2, 1, 1, 1, 1, 1]);
    }

    #[test]
    fn tiff_style_reference_stream() {
        // TIFF 6.0 第 13 节的例子：7 7 7 8 8 7 7 6 6
        let data = [
            0x80, 0x01, 0xE0, 0x40, 0x80, 0x44, 0x08, 0x0C, 0x06, 0x80, 0x80,
        ];
        assert_eq!(
            decode(&data, 8, Order::Msb, 100),
            [7, 7, 7, 8, 8, 7, 7, 6, 6]
        );
    }

    #[test]
    fn hostile_streams_stop() {
        assert!(decode(&[0xFF; 64], 2, Order::Lsb, 1_000).len() <= 1_000);
        assert!(decode(&[], 8, Order::Msb, 10).is_empty());
        let data = [0x8C, 0x2D, 0x99, 0x87, 0x2A, 0x1C, 0xDC, 0x33];
        assert!(decode(&data, 2, Order::Lsb, 3).len() <= 3);
    }
}
