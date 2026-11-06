//! 缓存驻留输入保留紧凑位置表，不为标签和远历史过滤增加热循环成本。
use crate::enc::Seq;
use crate::lz::{Position, h_long, h_short, mlen, rd};

pub(crate) struct Compact<P> {
    long: Vec<P>,
    short: Vec<P>,
    dual: bool,
    bits: u32,
}

impl<P: Position> Compact<P> {
    pub fn new(dual: bool, bits: u32) -> Self {
        let bits = bits.max(1);
        Self {
            long: vec![P::EMPTY; if dual { 1 << bits } else { 0 }],
            short: vec![P::EMPTY; 1 << bits],
            dual,
            bits,
        }
    }

    fn put(&mut self, d: &[u8], p: usize) {
        if p + 8 <= d.len() {
            let v = rd(d, p);
            self.short[h_short(v, self.bits)] = P::from_usize(p);
            if self.dual {
                self.long[h_long(v, self.bits)] = P::from_usize(p);
            }
        }
    }

    #[inline(always)]
    fn best(&mut self, d: &[u8], i: usize, end: usize, rep: usize) -> (usize, usize, usize) {
        let v = rd(d, i);
        let hs = h_short(v, self.bits);
        let cs = self.short[hs].to_usize();
        let (mut hl, mut cl) = (0, P::EMPTY.to_usize());
        if self.dual {
            hl = h_long(v, self.bits);
            cl = self.long[hl].to_usize();
        }
        self.short[hs] = P::from_usize(i);
        if self.dual {
            self.long[hl] = P::from_usize(i);
        }
        let mut best = (0, 0, 0);
        if rep <= i {
            let len = mlen(d, i - rep, i, end);
            if len >= 4 {
                best = (len + 1, rep, len);
            }
        }
        for c in [cl, cs] {
            if c < i && (!P::WIDE || i - c <= (u32::MAX - 3) as usize) {
                let len = mlen(d, c, i, end);
                if len >= 4 && len > best.0 {
                    best = (len, i - c, len);
                }
            }
        }
        best
    }

    pub fn parse(
        &mut self,
        d: &[u8],
        (start, end): (usize, usize),
        rep: &mut usize,
        seqs: &mut Vec<Seq>,
        lits: &mut Vec<u8>,
    ) -> usize {
        let (mut i, mut anchor, mut miss) = (start, start, 0);
        while i + 8 <= end {
            let mut cur = self.best(d, i, end, *rep);
            if cur.0 < 4 {
                i += 1 + (miss >> 5);
                miss += 1;
                continue;
            }
            miss = 0;
            if self.dual && cur.0 < 24 && i + 9 <= end {
                let next = self.best(d, i + 1, end, *rep);
                if next.0 > cur.0 + 1 {
                    i += 1;
                    cur = next;
                }
            }
            let (_, off, len) = cur;
            let ll = i - anchor;
            lits.extend_from_slice(&d[anchor..i]);
            seqs.push(Seq {
                ll: ll as u32,
                ml: len as u32,
                off: off as u32,
                rep: off == *rep && ll > 0,
            });
            *rep = off;
            self.put(d, i + 1);
            if len > 4 {
                self.put(d, i + len - 2);
                self.put(d, i + len - 1);
            }
            i += len;
            anchor = i;
        }
        anchor
    }
}
