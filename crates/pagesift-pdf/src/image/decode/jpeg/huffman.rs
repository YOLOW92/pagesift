//! 霍夫曼表与熵编码数据的读位器（ITU-T T.81 附录 C、§F.2.2、§F.1.2.3）。

/// [协议] 霍夫曼码最长 16 位（T.81 附录 C）
const MAX_LEN: usize = 16;
/// 查表的位数：码长不超过它的码一次查到，更长的按 T.81 §F.2.2.3 的 MAXCODE 逐位比较。
/// [推导] 典型表里绝大多数码不超过 9 位（libjpeg 的 HUFF_LOOKAHEAD 也取 9）；表大小 2^9 项
const LOOKUP_BITS: u32 = 9;

/// 一张霍夫曼表。
#[derive(Debug, Clone)]
pub(super) struct Huffman {
    /// 下标是码流里接下来的 LOOKUP_BITS 位；值的高 8 位是码长（0 表示要走慢路径），低 8 位是码值。
    lookup: Vec<u16>,
    /// 每个码长的最大码（没有这个长度的码时是 -1），T.81 图 F.16 的 MAXCODE。
    maxcode: [i32; MAX_LEN + 1],
    /// 每个码长第一个码的值与它在 `values` 里的下标之差（MINCODE / VALPTR 合并）。
    offset: [i32; MAX_LEN + 1],
    values: Vec<u8>,
}

impl Huffman {
    /// 按 T.81 §C.2（图 C.1、C.2）由码长计数和码值生成码表。
    pub(super) fn new(counts: &[u8], values: &[u8]) -> Result<Huffman, String> {
        let mut maxcode = [-1i32; MAX_LEN + 1];
        let mut offset = [0i32; MAX_LEN + 1];
        let mut lookup = vec![0u16; 1 << LOOKUP_BITS];
        let mut code: i32 = 0;
        let mut k = 0usize;
        for len in 1..=MAX_LEN {
            let n = usize::from(counts[len - 1]);
            // 先查再填：计数过多时码字会超出 len 位，查找表的下标也会越界
            if code + n as i32 > 1 << len {
                return Err("霍夫曼码长计数超出该长度能容纳的码数".into());
            }
            if n > 0 {
                offset[len] = k as i32 - code;
                for _ in 0..n {
                    if len as u32 <= LOOKUP_BITS {
                        // 以这个码开头的所有 LOOKUP_BITS 位前缀都指向它
                        let shift = LOOKUP_BITS - len as u32;
                        let first = (code as usize) << shift;
                        for e in &mut lookup[first..first + (1 << shift)] {
                            *e = ((len as u16) << 8) | u16::from(values[k]); // [协议] 码长存在高 8 位
                        }
                    }
                    code += 1;
                    k += 1;
                }
                maxcode[len] = code - 1;
            }
            code <<= 1;
        }
        Ok(Huffman {
            lookup,
            maxcode,
            offset,
            values: values.to_vec(),
        })
    }
}

/// 熵编码段的读位器：去掉填充字节（FF 00），遇到标记就停（之后补 0 位）。
pub(super) struct Bits<'a> {
    data: &'a [u8],
    pub(super) pos: usize,
    acc: u64,
    count: u32,
    /// 碰到了标记（或数据末尾）：后面只补 0 位。
    stopped: bool,
}

impl<'a> Bits<'a> {
    pub(super) fn new(data: &'a [u8], pos: usize) -> Bits<'a> {
        Bits {
            data,
            pos,
            acc: 0,
            count: 0,
            stopped: false,
        }
    }

    fn fill(&mut self) {
        // [协议] 累加器 64 位，每次补一字节（8 位）
        while self.count <= 56 {
            let byte = if self.stopped {
                0
            } else {
                match self.data.get(self.pos) {
                    None => {
                        self.stopped = true;
                        0
                    }
                    // [协议] FF 开头的是标记，或数据里的 FF 加填充 00（T.81 §B.1.1.2、§F.1.2.3）
                    Some(&0xFF) => match self.data.get(self.pos + 1) {
                        // [协议] FF 00 是数据里的 FF（T.81 §F.1.2.3）
                        Some(0x00) => {
                            self.pos += 2;
                            0xFF // [协议] FF 00 还原成数据字节 FF
                        }
                        _ => {
                            self.stopped = true;
                            0
                        }
                    },
                    Some(&b) => {
                        self.pos += 1;
                        b
                    }
                }
            };
            self.acc |= u64::from(byte) << (56 - self.count); // [协议] 64 位累加器高位对齐
            self.count += 8; // [协议] 一字节 8 位
        }
    }

    /// 取 n 位（1 ≤ n ≤ 16）。
    pub(super) fn bits(&mut self, n: u32) -> u32 {
        if n == 0 {
            return 0;
        }
        if self.count < n {
            self.fill();
        }
        let v = (self.acc >> (64 - n)) as u32; // [协议] 64 位累加器
        self.acc <<= n;
        self.count -= n;
        v
    }

    pub(super) fn bit(&mut self) -> bool {
        self.bits(1) == 1
    }

    /// 解一个霍夫曼码（T.81 §F.2.2.3 DECODE）。
    pub(super) fn decode(&mut self, t: &Huffman) -> Result<u8, String> {
        if self.count < MAX_LEN as u32 {
            self.fill();
        }
        let peek = (self.acc >> (64 - LOOKUP_BITS)) as usize; // [协议] 64 位累加器
        let e = t.lookup[peek];
        // [推导] 查找表项自定的打包：高 8 位是码长（0 表示走慢路径），低 8 位是一字节码值
        if e >> 8 != 0 {
            let len = u32::from(e >> 8); // [推导] 同上，高 8 位是码长
            self.acc <<= len;
            self.count -= len;
            return Ok(e as u8);
        }
        let top = (self.acc >> (64 - MAX_LEN)) as i32; // [协议] 64 位累加器
        for len in LOOKUP_BITS as usize + 1..=MAX_LEN {
            let code = top >> (MAX_LEN - len);
            if code <= t.maxcode[len] {
                self.acc <<= len;
                self.count -= len as u32;
                let i = (code + t.offset[len]) as usize;
                return t
                    .values
                    .get(i)
                    .copied()
                    .ok_or_else(|| "霍夫曼码值越界".into());
            }
        }
        Err("码流里出现了表中没有的霍夫曼码".into())
    }

    /// 取 s 位差值并按 T.81 §F.2.2.1 的 EXTEND 还原符号。
    pub(super) fn receive_extend(&mut self, s: u32) -> i32 {
        if s == 0 {
            return 0;
        }
        let v = self.bits(s) as i32;
        if v < 1 << (s - 1) {
            v - (1 << s) + 1
        } else {
            v
        }
    }

    /// 重启标记（RSTm，T.81 §B.2.1、§F.1.2.3）：丢掉没用完的位，跳过标记。
    pub(super) fn restart(&mut self) {
        self.acc = 0;
        self.count = 0;
        self.stopped = false;
        // [协议] 标记前可能有填充的 FF（T.81 §B.1.1.2）
        while self.data.get(self.pos) == Some(&0xFF) && self.data.get(self.pos + 1) == Some(&0xFF) {
            self.pos += 1;
        }
        // [协议] RST0–RST7 是 FFD0–FFD7
        if self.data.get(self.pos) == Some(&0xFF)
            && self
                .data
                .get(self.pos + 1)
                .is_some_and(|m| (0xD0..=0xD7).contains(m))
        {
            self.pos += 2;
        }
    }
}
