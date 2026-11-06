//! 快速匹配器：level 3 到 8 用双哈希表（长 8 字节、短 5 字节，各一个槽），level 1 到 2 只用短表。
//! 每个位置先试重复偏移，再试长表、短表，取最长；匹配后用长表再看下一位置是否更长（一步前瞻）。
//! 槽内同时保存首 4 字节标签，过滤冲突后才读取远处原文；紧凑位置与标签合并为一个 u64。
use crate::enc::Seq;

const MIN: usize = 4;
const PRIME: u64 = 0x9E37_79B1_85EB_CA87;

pub(crate) trait Position: Copy {
    type Candidate: Copy;
    const EMPTY: Self;
    const WIDE: bool;
    fn from_usize(p: usize) -> Self;
    fn to_usize(self) -> usize;
    fn candidate(pos: usize, first: u32) -> Self::Candidate;
    fn unpack(candidate: Self::Candidate) -> (usize, u32);
}

impl Position for u32 {
    type Candidate = u64;
    const EMPTY: Self = u32::MAX;
    const WIDE: bool = false;
    fn from_usize(p: usize) -> Self {
        p as u32
    }
    fn to_usize(self) -> usize {
        self as usize
    }
    fn candidate(pos: usize, first: u32) -> u64 {
        pos as u32 as u64 | (first as u64) << 32
    }
    fn unpack(candidate: u64) -> (usize, u32) {
        (candidate as u32 as usize, (candidate >> 32) as u32)
    }
}

impl Position for usize {
    type Candidate = (usize, u32);
    const EMPTY: Self = usize::MAX;
    const WIDE: bool = true;
    fn from_usize(p: usize) -> Self {
        p
    }
    fn to_usize(self) -> usize {
        self
    }
    fn candidate(pos: usize, first: u32) -> Self::Candidate {
        (pos, first)
    }
    fn unpack(candidate: Self::Candidate) -> (usize, u32) {
        candidate
    }
}

pub(crate) enum FastMode {
    Compact(crate::compact::Compact<u32>),
    Tagged(Fast<u32>),
    WideCompact(crate::compact::Compact<usize>),
    WideTagged(Fast<usize>),
}

impl FastMode {
    pub fn new(dual: bool, bits: u32, tagged: bool, wide: bool) -> Self {
        match (tagged, wide) {
            (false, false) => Self::Compact(crate::compact::Compact::new(dual, bits)),
            (true, false) => Self::Tagged(Fast::new(dual, bits)),
            (false, true) => Self::WideCompact(crate::compact::Compact::new(dual, bits)),
            (true, true) => Self::WideTagged(Fast::new(dual, bits)),
        }
    }

    pub fn parse(
        &mut self,
        d: &[u8],
        range: (usize, usize),
        rep: &mut usize,
        seqs: &mut Vec<Seq>,
        lits: &mut Vec<u8>,
    ) -> usize {
        match self {
            Self::Compact(f) => f.parse(d, range, rep, seqs, lits),
            Self::Tagged(f) => f.parse(d, range, rep, seqs, lits),
            Self::WideCompact(f) => f.parse(d, range, rep, seqs, lits),
            Self::WideTagged(f) => f.parse(d, range, rep, seqs, lits),
        }
    }
}

pub(crate) fn entry_bytes(wide: bool) -> usize {
    if wide {
        std::mem::size_of::<<usize as Position>::Candidate>()
    } else {
        std::mem::size_of::<<u32 as Position>::Candidate>()
    }
}

pub(crate) struct Fast<P: Position> {
    long: Vec<P::Candidate>,
    short: Vec<P::Candidate>,
    bits: u32,
}

pub(crate) fn rd(d: &[u8], p: usize) -> u64 {
    u64::from_le_bytes(d[p..p + 8].try_into().expect("8 字节"))
}

pub(crate) fn h_long(v: u64, bits: u32) -> usize {
    (v.wrapping_mul(PRIME) >> (64 - bits)) as usize
}

pub(crate) fn h_short(v: u64, bits: u32) -> usize {
    h_long(v << 24, bits)
}

pub(crate) fn mlen(d: &[u8], a: usize, b: usize, end: usize) -> usize {
    let mut l = 0;
    while b + l + 8 <= end {
        let x = rd(d, a + l) ^ rd(d, b + l);
        if x != 0 {
            return l + (x.trailing_zeros() / 8) as usize;
        }
        l += 8;
    }
    while b + l < end && d[a + l] == d[b + l] {
        l += 1;
    }
    l
}

#[inline(always)]
fn match_first(d: &[u8], a: usize, b: usize, end: usize, first: u64) -> usize {
    let x = rd(d, a) ^ first;
    if x == 0 {
        8 + mlen(d, a + 8, b + 8, end)
    } else {
        (x.trailing_zeros() / 8) as usize
    }
}

impl<P: Position> Fast<P> {
    pub fn new(dual: bool, bits: u32) -> Self {
        let bits = bits.max(1);
        let empty = P::candidate(P::EMPTY.to_usize(), 0);
        Self {
            long: vec![empty; if dual { 1 << bits } else { 0 }],
            short: vec![empty; 1 << bits],
            bits,
        }
    }

    fn put<const DUAL: bool>(&mut self, d: &[u8], p: usize) {
        if p + 8 <= d.len() {
            let v = rd(d, p);
            let candidate = P::candidate(p, v as u32);
            self.short[h_short(v, self.bits)] = candidate;
            if DUAL {
                self.long[h_long(v, self.bits)] = candidate;
            }
        }
    }

    /// 在 i 处找最佳匹配：（评分长度, 偏移, 真实长度）。`store` 为真时顺手把 i 登记进表（哈希只算一次）。
    #[inline(always)]
    fn best<const DUAL: bool>(
        &mut self,
        d: &[u8],
        i: usize,
        end: usize,
        rep: usize,
        store: bool,
    ) -> (usize, usize, usize) {
        let v = rd(d, i);
        let hs = h_short(v, self.bits);
        let cs = self.short[hs];
        let (mut hl, mut cl) = (0, P::candidate(P::EMPTY.to_usize(), 0));
        if DUAL {
            hl = h_long(v, self.bits);
            cl = self.long[hl];
        }
        if store {
            let candidate = P::candidate(i, v as u32);
            self.short[hs] = candidate;
            if DUAL {
                self.long[hl] = candidate;
            }
        }
        let mut best = (0, 0, 0);
        let repeated = i.checked_sub(rep).unwrap_or(usize::MAX);
        if rep <= i {
            let l = match_first(d, i - rep, i, end, v);
            if l >= MIN {
                best = (l + 1, rep, l);
            }
        }
        for (slot, candidate) in [cl, cs].into_iter().enumerate() {
            let (c, first) = P::unpack(candidate);
            if first == v as u32
                && c != repeated
                && (slot == 0 || c != P::unpack(cl).0)
                && c < i
                && (!P::WIDE || i - c <= (u32::MAX - 3) as usize)
            {
                let l = match_first(d, c, i, end, v);
                if l >= MIN && l > best.0 {
                    best = (l, i - c, l);
                }
            }
        }
        best
    }

    /// 解析 [start, end)，把序列和字面量追加进去，返回最后一个匹配的结尾（即尾部字面量的起点）。
    pub fn parse(
        &mut self,
        d: &[u8],
        (start, end): (usize, usize),
        rep: &mut usize,
        seqs: &mut Vec<Seq>,
        lits: &mut Vec<u8>,
    ) -> usize {
        if self.long.is_empty() {
            self.parse_inner::<false>(d, (start, end), rep, seqs, lits)
        } else {
            self.parse_inner::<true>(d, (start, end), rep, seqs, lits)
        }
    }

    #[inline(never)]
    fn parse_inner<const DUAL: bool>(
        &mut self,
        d: &[u8],
        (start, end): (usize, usize),
        rep: &mut usize,
        seqs: &mut Vec<Seq>,
        lits: &mut Vec<u8>,
    ) -> usize {
        let mut i = start;
        let mut anchor = start;
        let mut miss = 0usize;
        while i + 8 <= end {
            let mut cur = self.best::<DUAL>(d, i, end, *rep, true);
            if cur.0 < MIN {
                // 连续未命中越久步长越大
                i += 1 + (miss >> 5);
                miss += 1;
                continue;
            }
            miss = 0;
            // 一步前瞻：下一个位置明显更长就先吐一个字面量
            let registered = if DUAL && cur.0 < 24 && i + 9 <= end {
                let registered = i + 1;
                let nx = self.best::<DUAL>(d, i + 1, end, *rep, true);
                if nx.0 > cur.0 + 1 {
                    i += 1;
                    cur = nx;
                }
                registered
            } else {
                0
            };
            let (_, off, l) = cur;
            let ll = i - anchor;
            lits.extend_from_slice(&d[anchor..i]);
            seqs.push(Seq {
                ll: ll as u32,
                ml: l as u32,
                off: off as u32,
                rep: off == *rep && ll > 0,
            });
            *rep = off;
            // 匹配体内只登记开头和结尾几个位置
            // 标签表的未采用前瞻已经登记 i + 1，无须再次算哈希和写表。
            if registered != i + 1 {
                self.put::<DUAL>(d, i + 1);
            }
            if l > 4 {
                self.put::<DUAL>(d, i + l - 2);
                self.put::<DUAL>(d, i + l - 1);
            }
            i += l;
            anchor = i;
        }
        anchor
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn compact_and_wide_tables_agree() {
        let data = b"<div class=\"entry\">alpha beta gamma</div>\n".repeat(1000);
        for bits in [0, 8, 14] {
            for dual in [false, true] {
                let (mut a, mut b) = (Fast::<u32>::new(dual, bits), Fast::<usize>::new(dual, bits));
                let (mut sa, mut sb, mut la, mut lb) =
                    (Vec::new(), Vec::new(), Vec::new(), Vec::new());
                let (mut ra, mut rb) = (1, 1);
                assert_eq!(
                    a.parse(&data, (0, data.len()), &mut ra, &mut sa, &mut la),
                    b.parse(&data, (0, data.len()), &mut rb, &mut sb, &mut lb)
                );
                assert_eq!(la, lb);
                assert_eq!(ra, rb);
                let values = |s: Vec<Seq>| {
                    s.into_iter()
                        .map(|s| (s.ll, s.ml, s.off, s.rep))
                        .collect::<Vec<_>>()
                };
                let wanted = values(sa);
                assert_eq!(wanted, values(sb));
                for wide in [false, true] {
                    let mut compact = FastMode::new(dual, bits, false, wide);
                    let (mut seqs, mut lits, mut rep) = (Vec::new(), Vec::new(), 1);
                    compact.parse(&data, (0, data.len()), &mut rep, &mut seqs, &mut lits);
                    assert_eq!(lits, la);
                    assert_eq!(rep, ra);
                    assert_eq!(values(seqs), wanted);
                }
            }
        }
    }
}
