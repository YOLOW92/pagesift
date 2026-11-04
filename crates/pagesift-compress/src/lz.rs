//! 快速匹配器：level 3 到 8 用双哈希表（长 8 字节、短 5 字节，各一个槽），level 1 到 2 只用短表。
//! 每个位置先试重复偏移，再试长表、短表，取最长；匹配后用长表再看下一位置是否更长（一步前瞻）。
use crate::enc::Seq;

const BITS: u32 = 16;
const MIN: usize = 4;
const PRIME: u64 = 0x9E37_79B1_85EB_CA87;

pub(crate) struct Fast {
    long: Vec<u32>,
    short: Vec<u32>,
    dual: bool,
}

fn rd(d: &[u8], p: usize) -> u64 {
    u64::from_le_bytes(d[p..p + 8].try_into().expect("8 字节"))
}

fn h_long(v: u64) -> usize {
    (v.wrapping_mul(PRIME) >> (64 - BITS)) as usize
}

fn h_short(v: u64) -> usize {
    ((v << 24).wrapping_mul(PRIME) >> (64 - BITS)) as usize
}

fn mlen(d: &[u8], a: usize, b: usize, end: usize) -> usize {
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

impl Fast {
    pub fn new(dual: bool) -> Self {
        Self {
            long: vec![u32::MAX; if dual { 1 << BITS } else { 0 }],
            short: vec![u32::MAX; 1 << BITS],
            dual,
        }
    }

    fn put(&mut self, d: &[u8], p: usize) {
        if p + 8 <= d.len() {
            let v = rd(d, p);
            self.short[h_short(v)] = p as u32;
            if self.dual {
                self.long[h_long(v)] = p as u32;
            }
        }
    }

    /// 在 i 处找最佳匹配：（评分长度, 偏移, 真实长度）。`store` 为真时顺手把 i 登记进表（哈希只算一次）。
    #[inline(always)]
    fn best(
        &mut self,
        d: &[u8],
        i: usize,
        end: usize,
        rep: usize,
        store: bool,
    ) -> (usize, usize, usize) {
        let v = rd(d, i);
        let hs = h_short(v);
        let cs = self.short[hs];
        let (mut hl, mut cl) = (0, u32::MAX);
        if self.dual {
            hl = h_long(v);
            cl = self.long[hl];
        }
        if store {
            self.short[hs] = i as u32;
            if self.dual {
                self.long[hl] = i as u32;
            }
        }
        let mut best = (0, 0, 0);
        if rep <= i {
            let l = mlen(d, i - rep, i, end);
            if l >= MIN {
                best = (l + 1, rep, l);
            }
        }
        for c in [cl, cs] {
            let c = c as usize;
            if c < i {
                let l = mlen(d, c, i, end);
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
        let mut i = start;
        let mut anchor = start;
        let mut miss = 0usize;
        while i + 8 <= end {
            let mut cur = self.best(d, i, end, *rep, true);
            if cur.0 < MIN {
                // 连续未命中越久步长越大
                i += 1 + (miss >> 5);
                miss += 1;
                continue;
            }
            miss = 0;
            // 一步前瞻：下一个位置明显更长就先吐一个字面量
            if self.dual && cur.0 < 24 && i + 9 <= end {
                let nx = self.best(d, i + 1, end, *rep, true);
                if nx.0 > cur.0 + 1 {
                    i += 1;
                    cur = nx;
                }
            }
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
            self.put(d, i + 1);
            if l > 4 {
                self.put(d, i + l - 2);
                self.put(d, i + l - 1);
            }
            i += l;
            anchor = i;
        }
        anchor
    }
}
