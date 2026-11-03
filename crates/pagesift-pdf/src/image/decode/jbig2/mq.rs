//! MQ 算术解码器（T.88 附录 E，与 JPEG 2000 的 MQ 编码器相同），以及在它之上的
//! 整数解码过程（附录 A.2，IAx）和符号 ID 解码过程（附录 A.3，IAID）。

/// 一个上下文的状态：概率估计表的下标和当前的大概率符号。
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub(super) struct Cx {
    index: u8,
    mps: u8,
}

/// 一组上下文，全部从状态 0、MPS = 0 开始（T.88 §E.3.7 INITDEC 之前的约定）。
pub(super) fn contexts(n: usize) -> Vec<Cx> {
    vec![Cx::default(); n]
}

/// [协议] T.88 表 E.1：Qe、NMPS、NLPS、SWITCH
const QE: [(u32, u8, u8, bool); 47] = [
    (0x5601, 1, 1, true),
    (0x3401, 2, 6, false),
    (0x1801, 3, 9, false),
    (0x0AC1, 4, 12, false),
    (0x0521, 5, 29, false),
    (0x0221, 38, 33, false),
    (0x5601, 7, 6, true),
    (0x5401, 8, 14, false),
    (0x4801, 9, 14, false),
    (0x3801, 10, 14, false),
    (0x3001, 11, 17, false),
    (0x2401, 12, 18, false),
    (0x1C01, 13, 20, false),
    (0x1601, 29, 21, false),
    (0x5601, 15, 14, true),
    (0x5401, 16, 14, false),
    (0x5101, 17, 15, false),
    (0x4801, 18, 16, false),
    (0x3801, 19, 17, false),
    (0x3401, 20, 18, false),
    (0x3001, 21, 19, false),
    (0x2801, 22, 19, false),
    (0x2401, 23, 20, false),
    (0x2201, 24, 21, false),
    (0x1C01, 25, 22, false),
    (0x1801, 26, 23, false),
    (0x1601, 27, 24, false),
    (0x1401, 28, 25, false),
    (0x1201, 29, 26, false),
    (0x1101, 30, 27, false),
    (0x0AC1, 31, 28, false),
    (0x09C1, 32, 29, false),
    (0x08A1, 33, 30, false),
    (0x0521, 34, 31, false),
    (0x0441, 35, 32, false),
    (0x02A1, 36, 33, false),
    (0x0221, 37, 34, false),
    (0x0141, 38, 35, false),
    (0x0111, 39, 36, false),
    (0x0085, 40, 37, false),
    (0x0049, 41, 38, false),
    (0x0025, 42, 39, false),
    (0x0015, 43, 40, false),
    (0x0009, 44, 41, false),
    (0x0005, 45, 42, false),
    (0x0001, 45, 43, false),
    (0x5601, 46, 46, false),
];

/// 解码器状态（T.88 §E.3.1 的 A、C、CT、BP）。C 用 32 位寄存器：高 16 位是 Chigh。
pub(super) struct Mq<'a> {
    data: &'a [u8],
    bp: usize,
    a: u32,
    c: u32,
    ct: u32,
    /// 停在标记上（数据末尾之后也当作标记）之后又做了多少次判定（见 [`Mq::starved`]）。
    /// 解码器遇到标记就不再前进、只补 1，标记可能在数据中间，所以不能只看读到了哪里。
    past_end: u32,
    stalled: bool,
}

/// [策略] 停在标记上（含数据读完）之后最多再做的判定次数。合法码流读过末尾只是为了解完最后一点内容
/// （编码器 FLUSH 之后的尾巴、被删掉的末尾 0xFF 0x7F），2^20 次判定足够解完数万个文本实例的
/// 字段，远超这点尾巴所需；恶意码流（例如声明十几亿个实例）在数据读完后会无限空转，这个上限
/// 把空转限制在毫秒级。通用区域的逐像素解码不受此限（像素数已由分配上限约束）。
const STARVE_LIMIT: u32 = 1 << 20;

/// [`Mq::starved`] 为真时的错误信息。
pub(super) const STARVED: &str = "算术编码数据已读完，码流里的计数和数据对不上";

impl<'a> Mq<'a> {
    /// INITDEC（§E.3.5，图 E.20）。
    pub(super) fn new(data: &'a [u8]) -> Mq<'a> {
        let mut d = Mq {
            data,
            bp: 0,
            a: 0,
            c: 0,
            ct: 0,
            past_end: 0,
            stalled: false,
        };
        // [协议] INITDEC：C = B << 16，BYTEIN，再 C <<= 7、CT −= 7（图 E.20）
        d.c = u32::from(d.byte(0)) << 16;
        d.byte_in();
        d.c <<= 7; // [协议] 图 E.20
        d.ct -= 7; // [协议] 图 E.20
        d.a = 0x8000; // [协议] A 的初值（图 E.20）
        d
    }

    /// 数据末尾之后当 0xFF（§E.3.4：解码器读过末尾时相当于读到标记）。
    #[inline]
    fn byte(&self, i: usize) -> u8 {
        self.data.get(i).copied().unwrap_or(0xFF) // [协议] 读过末尾当 0xFF（§E.3.4）
    }

    /// BYTEIN（§E.3.4，图 E.19）。
    #[inline]
    fn byte_in(&mut self) {
        // [协议] 0xFF 之后大于 0x8F 的字节是标记：不前进，补 1（图 E.19）
        if self.byte(self.bp) == 0xFF {
            // [协议] 下一字节大于 0x8F：FF 是标记的开头（图 E.19）
            if self.byte(self.bp + 1) > 0x8F {
                self.c = self.c.wrapping_add(0xFF00); // [协议] 补 1 位（图 E.19）
                self.ct = 8; // [协议] 图 E.19
                self.stalled = true;
            } else {
                self.bp += 1;
                // [协议] FF 之后的字节填充过一位，只取 7 位：左移 9、CT = 7（图 E.19）
                self.c = self.c.wrapping_add(u32::from(self.byte(self.bp)) << 9);
                self.ct = 7; // [协议] 图 E.19
            }
        } else {
            self.bp += 1;
            // [协议] 普通字节：左移 8、CT = 8（图 E.19）
            self.c = self.c.wrapping_add(u32::from(self.byte(self.bp)) << 8);
            self.ct = 8; // [协议] 图 E.19
        }
    }

    /// DECODE（§E.3.2，图 E.15–E.18）：用上下文 `cx` 解一位。
    #[inline]
    pub(super) fn decode(&mut self, cx: &mut Cx) -> u8 {
        if self.stalled {
            self.past_end = self.past_end.saturating_add(1);
        }
        let (qe, nmps, nlps, switch) = QE[cx.index as usize];
        self.a -= qe;
        let d;
        // [协议] 比较 C 的高 16 位 Chigh 与 Qe（图 E.15）
        if (self.c >> 16) < qe {
            // LPS_EXCHANGE（图 E.17）
            if self.a < qe {
                d = cx.mps;
                cx.index = nmps;
            } else {
                d = 1 - cx.mps;
                if switch {
                    cx.mps = 1 - cx.mps;
                }
                cx.index = nlps;
            }
            self.a = qe;
            self.renorm();
        } else {
            self.c -= qe << 16; // [协议] Chigh −= Qe（图 E.15）
            // [协议] A 不小于 0x8000 时不用重新归一（图 E.15）
            if self.a & 0x8000 != 0 {
                return cx.mps;
            }
            // MPS_EXCHANGE（图 E.16）
            if self.a < qe {
                d = 1 - cx.mps;
                if switch {
                    cx.mps = 1 - cx.mps;
                }
                cx.index = nlps;
            } else {
                d = cx.mps;
                cx.index = nmps;
            }
            self.renorm();
        }
        d
    }

    /// 数据早已读完、只靠补的 0xFF 在解：符号字典、文本区域这类按码流里的计数循环的地方，
    /// 每轮查一次，为真就停（计数和数据对不上，码流损坏或是恶意构造）。
    pub(super) fn starved(&self) -> bool {
        self.past_end > STARVE_LIMIT
    }

    /// RENORMD（§E.3.3，图 E.18）。
    #[inline]
    fn renorm(&mut self) {
        loop {
            if self.ct == 0 {
                self.byte_in();
            }
            self.a <<= 1;
            self.c <<= 1;
            self.ct -= 1;
            // [协议] A 保持在 0x8000 以上
            if self.a & 0x8000 != 0 {
                break;
            }
        }
    }
}

/// 整数解码过程 IAx（附录 A.2）：每种整数（IADH、IADW、IAEX……）各用一个，512 个上下文。
pub(super) struct IntDecoder {
    cx: Vec<Cx>,
}

impl IntDecoder {
    pub(super) fn new() -> IntDecoder {
        // [协议] PREV 是 9 位（附录 A.2）
        IntDecoder { cx: contexts(512) }
    }

    /// 解一个整数；None 是 OOB（越界值，附录 A.2 的 S = 1、V = 0）。
    pub(super) fn decode(&mut self, mq: &mut Mq) -> Option<i32> {
        let mut prev = 1usize;
        let mut bit = |mq: &mut Mq| {
            let d = mq.decode(&mut self.cx[prev]);
            // [协议] PREV 只保留最近 8 位，再置第 9 位（附录 A.2 的步骤 3）
            prev = if prev < 256 {
                (prev << 1) | usize::from(d)
            } else {
                (((prev << 1) | usize::from(d)) & 511) | 256 // [协议] 留低 9 位，再置第 9 位
            };
            d
        };
        let s = bit(mq);
        // [协议] 附录 A.2 表 A.1：前缀位决定 V 的位数和偏移
        let (bits, offset) = if bit(mq) == 0 {
            (2, 0)
        } else if bit(mq) == 0 {
            (4, 4) // [协议] 前缀 10：4 位，从 4 起
        } else if bit(mq) == 0 {
            (6, 20) // [协议] 前缀 110：6 位，从 20 起
        } else if bit(mq) == 0 {
            (8, 84) // [协议] 前缀 1110：8 位，从 84 起
        } else if bit(mq) == 0 {
            (12, 340) // [协议] 前缀 11110：12 位，从 340 起
        } else {
            (32, 4436) // [协议] 前缀 11111：32 位，从 4436 起
        };
        let mut v: u32 = 0;
        for _ in 0..bits {
            v = (v << 1) | u32::from(bit(mq));
        }
        let v = i64::from(v) + offset;
        match (s, v) {
            (1, 0) => None,
            (1, v) => Some((-v).clamp(i64::from(i32::MIN), 0) as i32),
            (_, v) => Some(v.min(i64::from(i32::MAX)) as i32),
        }
    }
}

/// 符号 ID 解码过程 IAID（附录 A.3）：码长 SBSYMCODELEN 位，2^SBSYMCODELEN 个上下文。
pub(super) struct IdDecoder {
    cx: Vec<Cx>,
    len: u32,
}

impl IdDecoder {
    /// `len` 是 SBSYMCODELEN（T.88 §7.4.3.1.7 的 ⌈log2(符号数)⌉）。
    pub(super) fn new(len: u32) -> Result<IdDecoder, String> {
        // [推导] 符号总数是 32 位整数，码长至多 32
        if len > 31 {
            return Err(format!("符号 ID 码长 {len} 过大"));
        }
        super::bitmap::charge((1usize << len) * size_of::<Cx>())?;
        Ok(IdDecoder {
            cx: contexts(1 << len),
            len,
        })
    }

    pub(super) fn decode(&mut self, mq: &mut Mq) -> u32 {
        let mut prev = 1usize;
        for _ in 0..self.len {
            let d = mq.decode(&mut self.cx[prev]);
            prev = (prev << 1) | usize::from(d);
        }
        (prev - (1 << self.len)) as u32
    }
}
