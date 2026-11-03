//! 高位在前的读位器。读过数据末尾时补 0：全 0 不是任何有效码，解码会在那里停下。

pub(super) struct Bits<'a> {
    data: &'a [u8],
    /// 已读的位数。
    pos: usize,
}

impl<'a> Bits<'a> {
    pub(super) fn new(data: &'a [u8]) -> Bits<'a> {
        Bits { data, pos: 0 }
    }

    /// 接下来的 `n` 位（n ≤ 32），不前进。
    #[inline]
    pub(super) fn peek(&self, n: u32) -> u32 {
        let byte = self.pos >> 3; // [协议] 一字节 8 位（单位换算）
        let mut v = 0u64;
        // [推导] 5 个字节 = 40 位，位偏移至多 7，够取 32 位
        for k in 0..5 {
            // [协议] 一字节 8 位，逐字节拼到低位
            v = (v << 8) | u64::from(self.data.get(byte + k).copied().unwrap_or(0));
        }
        let off = (self.pos & 7) as u32; // [协议] 字节内的位偏移（一字节 8 位）
        // [推导] 40 位在 u64 低位：左移 64 − 40 = 24 位再加位偏移，顶到最高位；右移 64 − n 留下前 n 位
        ((v << (24 + off)) >> (64 - n)) as u32
    }

    #[inline]
    pub(super) fn skip(&mut self, n: u32) {
        self.pos += n as usize;
    }

    pub(super) fn bit(&mut self) -> u32 {
        let b = self.peek(1);
        self.skip(1);
        b
    }

    /// 跳到下一个字节边界（/EncodedByteAlign）。
    pub(super) fn align(&mut self) {
        self.pos = self.pos.next_multiple_of(8); // [协议] 一字节 8 位
    }

    /// 已读的字节数（不足一字节的算一字节）。
    pub(super) fn bytes_read(&self) -> usize {
        self.pos.div_ceil(8).min(self.data.len()) // [协议] 一字节 8 位
    }

    pub(super) fn at_end(&self) -> bool {
        self.pos >= self.data.len() * 8 // [协议] 一字节 8 位
    }

    /// 读掉一个行尾码 EOL（000000000001，前面可以有任意多个填充 0，T.4 §4.1.2、§4.1.3）。
    /// 不是 EOL 时不前进。有效码最多以 7 个 0 开头，11 个以上的 0 只能是填充或 EOL。
    pub(super) fn eol(&mut self) -> bool {
        // [协议] EOL 是 11 个 0 加 1
        match self.peek(12) {
            1 => {
                self.skip(12); // [协议] EOL 共 12 位（T.4 §4.1.2）
                true
            }
            0 => {
                let start = self.pos;
                while !self.at_end() && self.peek(1) == 0 {
                    self.skip(1);
                }
                if self.at_end() {
                    self.pos = start;
                    return false;
                }
                self.skip(1);
                true
            }
            _ => false,
        }
    }
}
