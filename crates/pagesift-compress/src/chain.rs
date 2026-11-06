//! 高等级哈希链；历史环随输入及资源预算伸缩，搜索深度随等级变化。
use crate::enc::Seq;
use crate::lz::Position;
use crate::resources::Budget;

const MIN: usize = 4;

pub(crate) struct Chain<P> {
    head: Vec<P>,
    long: Vec<P>,
    prev: Vec<P>,
    bits: u32,
    depth: usize,
    local_span: usize,
    next: usize,
}

impl<P: Position> Chain<P> {
    pub fn new(budget: &Budget) -> Self {
        Self {
            head: vec![P::EMPTY; 1 << budget.hash_bits],
            long: vec![P::EMPTY; 1 << budget.hash_bits],
            prev: vec![P::EMPTY; budget.window],
            bits: budget.hash_bits,
            depth: budget.depth,
            local_span: budget.local_span,
            next: 0,
        }
    }

    fn hash(&self, d: &[u8], p: usize) -> usize {
        let v = u32::from_le_bytes(d[p..p + 4].try_into().expect("4 字节")) as u64;
        ((v.wrapping_mul(0x9E37_79B1_85EB_CA87) >> 1) >> (63 - self.bits)) as usize
    }

    fn long_hash(&self, d: &[u8], p: usize) -> Option<usize> {
        let bytes = d.get(p..p + 8)?;
        let v = u64::from_le_bytes(bytes.try_into().expect("8 字节"));
        Some(((v.wrapping_mul(0x9E37_79B1_85EB_CA87) >> 1) >> (63 - self.bits)) as usize)
    }

    fn insert_until(&mut self, d: &[u8], end: usize) {
        while self.next < end && self.next + MIN <= d.len() {
            let p = self.next;
            let h = self.hash(d, p);
            let slot = p & (self.prev.len() - 1);
            self.prev[slot] = self.head[h];
            self.head[h] = P::from_usize(p);
            if let Some(h) = self.long_hash(d, p) {
                self.long[h] = P::from_usize(p);
            }
            self.next += 1;
        }
    }

    fn find(&self, d: &[u8], i: usize, end: usize, rep: usize) -> (usize, usize, usize) {
        let mut best = (0, 0, 0);
        if rep <= i {
            let len = crate::lz::mlen(d, i - rep, i, end);
            if len >= MIN {
                best = (len + 1, rep, len);
            }
        }
        if let Some(h) = self.long_hash(d, i) {
            let c = self.long[h].to_usize();
            if c < i && i - c < self.prev.len() {
                let len = crate::lz::mlen(d, c, i, end);
                if len >= MIN && len > best.0 {
                    best = (len, i - c, len);
                }
            }
        }
        let mut c = self.head[self.hash(d, i)].to_usize();
        let mut spent = 0usize;
        for _ in 0..self.depth {
            if c >= i
                || i - c >= self.prev.len()
                || best.2 >= (end - i).min(self.depth.ilog2() as usize + MIN)
            {
                break;
            }
            // 远历史的短前缀候选随机访存更贵；长哈希仍可直接命中整个窗口。
            spent = spent.saturating_add(1 + (i - c) / self.local_span);
            if spent > self.depth {
                break;
            }
            if d[c..c + MIN] == d[i..i + MIN]
                && (best.0 == 0 || i + best.0 >= end || d[c + best.0] == d[i + best.0])
            {
                let len = crate::lz::mlen(d, c, i, end);
                if len >= MIN && len > best.0 {
                    best = (len, i - c, len);
                }
            }
            let next = self.prev[c & (self.prev.len() - 1)].to_usize();
            if next >= c {
                break;
            }
            c = next;
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
        let (mut i, mut anchor) = (start, start);
        let mut miss = 0;
        while i + MIN <= end {
            self.insert_until(d, i);
            let mut cur = self.find(d, i, end, *rep);
            if cur.0 < MIN {
                i += 1 + (miss >> 6);
                miss += 1;
                continue;
            }
            // 前瞻的收益取决于已找到的长度和搜索预算，而非固定的长度阈值。
            while cur.0 < self.depth && i + 1 + MIN <= end {
                self.insert_until(d, i + 1);
                let next = self.find(d, i + 1, end, *rep);
                if next.0 <= cur.0 + 1 {
                    break;
                }
                i += 1;
                cur = next;
            }
            miss = 0;
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
            // 长匹配内按搜索预算抽样，避免为已消费的每个字节随机写入整张历史表。
            let stop = i + len;
            self.insert_until(d, (i + 2).min(stop));
            let stride = (len / (self.depth.ilog2() as usize + 1)).max(1);
            while self.next + stride < stop {
                self.next += stride;
                self.insert_until(d, self.next + 1);
            }
            self.next = self.next.max(stop.saturating_sub(2));
            i += len;
            anchor = i;
        }
        anchor
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn check<P: Position>() -> Vec<(u32, u32, u32, bool)> {
        let mut state = 0x123456789abcdefu64;
        let first: Vec<u8> = (0..100_000)
            .map(|_| {
                state ^= state << 13;
                state ^= state >> 7;
                state ^= state << 17;
                (state >> 32) as u8
            })
            .collect();
        let data = first.repeat(4);
        let budget = Budget {
            tagged: false,
            hash_bits: 14,
            window: 1 << 19,
            depth: 27,
            local_span: 16 << 10,
        };
        let mut chain = Chain::<P>::new(&budget);
        let (mut seqs, mut lits, mut rep) = (Vec::new(), Vec::new(), 1);
        for start in (0..data.len()).step_by(128 << 10) {
            let end = (start + (128 << 10)).min(data.len());
            let anchor = chain.parse(&data, (start, end), &mut rep, &mut seqs, &mut lits);
            lits.extend_from_slice(&data[anchor..end]);
        }
        assert!(seqs.iter().any(|s| s.off > 65536));
        assert!(lits.len() < first.len() + 5000);
        assert_eq!(
            lits.len() + seqs.iter().map(|s| s.ml as usize).sum::<usize>(),
            data.len()
        );
        seqs.iter().map(|s| (s.ll, s.ml, s.off, s.rep)).collect()
    }

    #[test]
    fn long_history_compact_and_wide_positions() {
        assert_eq!(check::<u32>(), check::<usize>());
    }
}
