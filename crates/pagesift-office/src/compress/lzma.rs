//! LZMA 和 LZMA2 解压（照 LZMA SDK 的 LzmaSpec 和 xz 格式文档的 LZMA2 一节）。
//! 输出放在一个 `Vec` 里，整段都当字典，所以不用环形缓冲；距离超出已有数据就报错。

use crate::Error;

const NUM_STATES: usize = 12;
const POS_STATES_MAX: usize = 16;
const PROB_INIT: u16 = 1024;
const TOP: u32 = 1 << 24;

fn corrupt() -> Error {
    Error::Malformed("corrupt LZMA data".into())
}

struct Range<'a> {
    input: &'a [u8],
    pos: usize,
    range: u32,
    code: u32,
}

impl<'a> Range<'a> {
    fn new(input: &'a [u8]) -> Result<Self, Error> {
        // 第一个字节规定是 0，后面 4 个是初始码
        let head = input.get(..5).ok_or_else(corrupt)?;
        if head[0] != 0 {
            return Err(corrupt());
        }
        Ok(Range {
            input,
            pos: 5,
            range: u32::MAX,
            code: u32::from_be_bytes([head[1], head[2], head[3], head[4]]),
        })
    }

    #[inline]
    fn normalize(&mut self) {
        if self.range < TOP {
            self.range <<= 8;
            // 读过头按 0：截断的数据后面会因为距离或长度不对停下，不会越界
            let b = self.input.get(self.pos).copied().unwrap_or(0);
            self.pos += 1;
            self.code = self.code << 8 | u32::from(b);
        }
    }

    #[inline]
    fn bit(&mut self, prob: &mut u16) -> usize {
        let bound = (self.range >> 11) * u32::from(*prob);
        let out = if self.code < bound {
            self.range = bound;
            *prob += (2048 - *prob) >> 5;
            0
        } else {
            self.code -= bound;
            self.range -= bound;
            *prob -= *prob >> 5;
            1
        };
        self.normalize();
        out
    }

    fn direct(&mut self, count: u32) -> u32 {
        let mut out = 0u32;
        for _ in 0..count {
            self.range >>= 1;
            let t = u32::from(self.code >= self.range);
            if t == 1 {
                self.code -= self.range;
            }
            out = out << 1 | t;
            self.normalize();
        }
        out
    }

    fn tree(&mut self, probs: &mut [u16], bits: u32) -> usize {
        let mut m = 1usize;
        for _ in 0..bits {
            m = m << 1 | self.bit(&mut probs[m]);
        }
        m - (1 << bits)
    }

    fn reverse(&mut self, probs: &mut [u16], bits: u32) -> u32 {
        let mut m = 1usize;
        let mut sym = 0u32;
        for i in 0..bits {
            let b = self.bit(&mut probs[m]);
            m = m << 1 | b;
            sym |= (b as u32) << i;
        }
        sym
    }
}

#[derive(Clone)]
struct Len {
    choice: u16,
    choice2: u16,
    low: [[u16; 8]; POS_STATES_MAX],
    mid: [[u16; 8]; POS_STATES_MAX],
    high: [u16; 256],
}

impl Len {
    fn new() -> Self {
        Len {
            choice: PROB_INIT,
            choice2: PROB_INIT,
            low: [[PROB_INIT; 8]; POS_STATES_MAX],
            mid: [[PROB_INIT; 8]; POS_STATES_MAX],
            high: [PROB_INIT; 256],
        }
    }

    fn decode(&mut self, rc: &mut Range<'_>, pos_state: usize) -> usize {
        if rc.bit(&mut self.choice) == 0 {
            return rc.tree(&mut self.low[pos_state], 3);
        }
        if rc.bit(&mut self.choice2) == 0 {
            return 8 + rc.tree(&mut self.mid[pos_state], 3);
        }
        16 + rc.tree(&mut self.high, 8)
    }
}

/// LZMA 解码器的状态。LZMA2 的多个块之间要保留。
pub(super) struct Decoder {
    lc: u32,
    lp: u32,
    pb: u32,
    state: usize,
    rep: [u32; 4],
    is_match: Vec<u16>,
    is_rep: [u16; NUM_STATES],
    rep_g0: [u16; NUM_STATES],
    rep_g1: [u16; NUM_STATES],
    rep_g2: [u16; NUM_STATES],
    rep0_long: Vec<u16>,
    slot: Vec<[u16; 64]>,
    special: [u16; 115],
    align: [u16; 16],
    len: Len,
    rep_len: Len,
    literal: Vec<u16>,
}

impl Decoder {
    /// `props` 是 `(pb*5+lp)*9+lc`。
    pub(super) fn new(props: u8) -> Result<Self, Error> {
        let mut d = Decoder {
            lc: 0,
            lp: 0,
            pb: 0,
            state: 0,
            rep: [0; 4],
            is_match: vec![PROB_INIT; NUM_STATES * POS_STATES_MAX],
            is_rep: [PROB_INIT; NUM_STATES],
            rep_g0: [PROB_INIT; NUM_STATES],
            rep_g1: [PROB_INIT; NUM_STATES],
            rep_g2: [PROB_INIT; NUM_STATES],
            rep0_long: vec![PROB_INIT; NUM_STATES * POS_STATES_MAX],
            slot: vec![[PROB_INIT; 64]; 4],
            special: [PROB_INIT; 115],
            align: [PROB_INIT; 16],
            len: Len::new(),
            rep_len: Len::new(),
            literal: Vec::new(),
        };
        d.set_props(props)?;
        Ok(d)
    }

    fn set_props(&mut self, props: u8) -> Result<(), Error> {
        let p = u32::from(props);
        if p >= 9 * 5 * 5 {
            return Err(corrupt());
        }
        self.lc = p % 9;
        let rest = p / 9;
        self.lp = rest % 5;
        self.pb = rest / 5;
        // lc + lp 最大 12 时字面量表约 6 MB；LZMA2 规定不超过 4，但 7z 的 LZMA 可以更大
        self.reset_state();
        Ok(())
    }

    fn reset_state(&mut self) {
        self.state = 0;
        self.rep = [0; 4];
        self.is_match.fill(PROB_INIT);
        self.is_rep.fill(PROB_INIT);
        self.rep_g0.fill(PROB_INIT);
        self.rep_g1.fill(PROB_INIT);
        self.rep_g2.fill(PROB_INIT);
        self.rep0_long.fill(PROB_INIT);
        self.slot.iter_mut().for_each(|s| s.fill(PROB_INIT));
        self.special.fill(PROB_INIT);
        self.align.fill(PROB_INIT);
        self.len = Len::new();
        self.rep_len = Len::new();
        self.literal.clear();
        self.literal
            .resize(0x300usize << (self.lc + self.lp), PROB_INIT);
    }

    /// 解到 `out` 有 `target` 字节，或者遇到结束标记。`dict_start` 之前的数据不能被引用。
    pub(super) fn run(
        &mut self,
        input: &[u8],
        out: &mut Vec<u8>,
        dict_start: usize,
        target: usize,
    ) -> Result<(), Error> {
        let mut rc = Range::new(input)?;
        let pos_mask = (1usize << self.pb) - 1;
        let lp_mask = (1usize << self.lp) - 1;
        while out.len() < target {
            if rc.pos > input.len() + 8 {
                return Err(corrupt()); // 输入用完了还没解够
            }
            let pos = out.len() - dict_start;
            let pos_state = pos & pos_mask;
            if rc.bit(&mut self.is_match[self.state * POS_STATES_MAX + pos_state]) == 0 {
                let prev = if pos > 0 { out[out.len() - 1] } else { 0 };
                let at = ((pos & lp_mask) << self.lc) + (usize::from(prev) >> (8 - self.lc));
                let lit = &mut self.literal[0x300 * at..0x300 * (at + 1)];
                let mut sym = 1usize;
                if self.state >= 7 {
                    let dist = self.rep[0] as usize + 1;
                    if dist > pos {
                        return Err(corrupt());
                    }
                    let mut matched = usize::from(out[out.len() - dist]);
                    while sym < 0x100 {
                        let mbit = (matched >> 7) & 1;
                        matched <<= 1;
                        let bit = rc.bit(&mut lit[((1 + mbit) << 8) + sym]);
                        sym = sym << 1 | bit;
                        if mbit != bit {
                            break;
                        }
                    }
                }
                while sym < 0x100 {
                    sym = sym << 1 | rc.bit(&mut lit[sym]);
                }
                out.push(sym as u8);
                self.state = match self.state {
                    0..=3 => 0,
                    4..=9 => self.state - 3,
                    _ => self.state - 6,
                };
                continue;
            }
            let mut len;
            if rc.bit(&mut self.is_rep[self.state]) == 1 {
                if pos == 0 {
                    return Err(corrupt());
                }
                if rc.bit(&mut self.rep_g0[self.state]) == 0 {
                    if rc.bit(&mut self.rep0_long[self.state * POS_STATES_MAX + pos_state]) == 0 {
                        self.state = if self.state < 7 { 9 } else { 11 };
                        let dist = self.rep[0] as usize + 1;
                        if dist > pos {
                            return Err(corrupt());
                        }
                        out.push(out[out.len() - dist]);
                        continue;
                    }
                } else {
                    let dist;
                    if rc.bit(&mut self.rep_g1[self.state]) == 0 {
                        dist = self.rep[1];
                    } else {
                        if rc.bit(&mut self.rep_g2[self.state]) == 0 {
                            dist = self.rep[2];
                        } else {
                            dist = self.rep[3];
                            self.rep[3] = self.rep[2];
                        }
                        self.rep[2] = self.rep[1];
                    }
                    self.rep[1] = self.rep[0];
                    self.rep[0] = dist;
                }
                len = self.rep_len.decode(&mut rc, pos_state);
                self.state = if self.state < 7 { 8 } else { 11 };
            } else {
                self.rep[3] = self.rep[2];
                self.rep[2] = self.rep[1];
                self.rep[1] = self.rep[0];
                len = self.len.decode(&mut rc, pos_state);
                self.state = if self.state < 7 { 7 } else { 10 };
                let dist = self.distance(&mut rc, len);
                self.rep[0] = dist;
                if dist == u32::MAX {
                    return Ok(()); // 结束标记
                }
            }
            len += 2;
            let dist = self.rep[0] as usize + 1;
            if dist > pos {
                return Err(corrupt());
            }
            let n = len.min(target - out.len());
            // 距离可能比长度短（重复模式），只能一个字节一个字节复制
            for _ in 0..n {
                out.push(out[out.len() - dist]);
            }
        }
        Ok(())
    }

    fn distance(&mut self, rc: &mut Range<'_>, len: usize) -> u32 {
        let slot = rc.tree(&mut self.slot[len.min(3)], 6) as u32;
        if slot < 4 {
            return slot;
        }
        let bits = (slot >> 1) - 1;
        let mut dist = (2 | (slot & 1)) << bits;
        if slot < 14 {
            let base = (dist - slot) as usize;
            dist = dist.wrapping_add(rc.reverse(&mut self.special[base..], bits));
        } else {
            dist = dist.wrapping_add(rc.direct(bits - 4) << 4);
            dist = dist.wrapping_add(rc.reverse(&mut self.align, 4));
        }
        dist
    }
}

/// 裸 LZMA（7z 的 LZMA 编码器）：属性 5 字节（1 字节 lc/lp/pb、4 字节字典大小），解压后大小已知。
pub(super) fn decode_raw(input: &[u8], props: &[u8], size: usize) -> Result<Vec<u8>, Error> {
    let p = *props.first().ok_or_else(corrupt)?;
    let mut d = Decoder::new(p)?;
    let mut out = Vec::with_capacity(size.min(64 << 20));
    d.run(input, &mut out, 0, size)?;
    if out.len() != size {
        return Err(corrupt());
    }
    Ok(out)
}

/// LZMA2 分块流，到控制字节 0 结束。返回用掉的输入字节数。
pub(super) fn decode_lzma2(input: &[u8], out: &mut Vec<u8>, limit: usize) -> Result<usize, Error> {
    let mut pos = 0usize;
    let mut decoder: Option<Decoder> = None;
    let mut dict_start = out.len();
    loop {
        let control = *input.get(pos).ok_or_else(corrupt)?;
        pos += 1;
        if control == 0 {
            return Ok(pos);
        }
        if control < 0x80 {
            // 未压缩块：1 同时重置字典，2 不重置，其他无效
            if control > 2 {
                return Err(corrupt());
            }
            let h = input.get(pos..pos + 2).ok_or_else(corrupt)?;
            let size = usize::from(u16::from_be_bytes([h[0], h[1]])) + 1;
            pos += 2;
            let data = input.get(pos..pos + size).ok_or_else(corrupt)?;
            pos += size;
            if out.len() + size > limit {
                return Err(Error::TooLarge("decompressed size"));
            }
            if control == 1 {
                dict_start = out.len();
            }
            out.extend_from_slice(data);
            continue;
        }
        let h = input.get(pos..pos + 4).ok_or_else(corrupt)?;
        let unpacked = ((usize::from(control) & 0x1F) << 16)
            + usize::from(u16::from_be_bytes([h[0], h[1]]))
            + 1;
        let packed = usize::from(u16::from_be_bytes([h[2], h[3]])) + 1;
        pos += 4;
        let reset = (control >> 5) & 3;
        if reset == 3 {
            dict_start = out.len();
        }
        if reset >= 2 {
            let props = *input.get(pos).ok_or_else(corrupt)?;
            pos += 1;
            match &mut decoder {
                Some(d) => d.set_props(props)?,
                None => decoder = Some(Decoder::new(props)?),
            }
        } else if reset == 1 {
            decoder.as_mut().ok_or_else(corrupt)?.reset_state();
        }
        let d = decoder.as_mut().ok_or_else(corrupt)?;
        let chunk = input.get(pos..pos + packed).ok_or_else(corrupt)?;
        pos += packed;
        let target = out.len() + unpacked;
        if target > limit {
            return Err(Error::TooLarge("decompressed size"));
        }
        d.run(chunk, out, dict_start, target)?;
        if out.len() != target {
            return Err(corrupt());
        }
    }
}
