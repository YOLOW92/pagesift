//! 基础编码器：贪心 LZ 匹配，序列用预定义 FSE 表，字面量原样存放。
use crate::dec::{LL_DEFAULT, ML_DEFAULT, OF_DEFAULT};
use crate::fse::{self, FseTable};

const BLOCK: usize = 128 * 1024;
const MIN_MATCH: usize = 4;

pub(crate) struct BitWriter {
    out: Vec<u8>,
    acc: u64,
    n: u32,
}

impl BitWriter {
    pub fn new() -> Self {
        Self {
            out: Vec::new(),
            acc: 0,
            n: 0,
        }
    }
    /// 补齐到整字节(不写哨兵),用于 FSE 表头。
    pub fn flush_bytes(mut self) -> Vec<u8> {
        if self.n > 0 {
            self.out.push(self.acc as u8);
        }
        self.out
    }
    pub fn put(&mut self, v: u32, bits: u32) {
        if bits == 0 {
            return;
        }
        self.acc |= (v as u64 & ((1u64 << bits) - 1)) << self.n;
        self.n += bits;
        while self.n >= 8 {
            self.out.push(self.acc as u8);
            self.acc >>= 8;
            self.n -= 8;
        }
    }
    /// 写结束哨兵位并补齐字节。
    pub fn finish(mut self) -> Vec<u8> {
        self.put(1, 1);
        if self.n > 0 {
            self.out.push(self.acc as u8);
        }
        self.out
    }
}

fn ll_code(v: u32) -> (u32, u32, u32) {
    // 返回（码，额外位数，额外值）
    if v < 16 {
        return (v, 0, 0);
    }
    let (bases, bits): (&[u32], &[u32]) = (
        &[
            16, 18, 20, 22, 24, 28, 32, 40, 48, 64, 128, 256, 512, 1024, 2048, 4096, 8192, 16384,
            32768, 65536,
        ],
        &[
            1, 1, 1, 1, 2, 2, 3, 3, 4, 6, 7, 8, 9, 10, 11, 12, 13, 14, 15, 16,
        ],
    );
    let i = bases.iter().rposition(|&b| b <= v).unwrap_or(0);
    (16 + i as u32, bits[i], v - bases[i])
}

fn ml_code(v: u32) -> (u32, u32, u32) {
    if v < 35 {
        return (v - 3, 0, 0);
    }
    let bases = [
        35u32, 37, 39, 41, 43, 47, 51, 59, 67, 83, 99, 131, 259, 515, 1027, 2051, 4099, 8195,
        16387, 32771, 65539,
    ];
    let bits = [
        1u32, 1, 1, 1, 2, 2, 3, 3, 4, 4, 5, 7, 8, 9, 10, 11, 12, 13, 14, 15, 16,
    ];
    let i = bases.iter().rposition(|&b| b <= v).unwrap_or(0);
    (32 + i as u32, bits[i], v - bases[i])
}

struct Seq {
    ll: u32,
    ml: u32,
    off: u32,
    /// 用重复偏移 1(上一次的偏移,且 ll > 0)。
    rep: bool,
}

/// 三张预定义表,及每个符号对应的格子下标(只建一次)。
type SymTable = (FseTable, Vec<Vec<u16>>);

#[derive(Clone)]
struct EncTables {
    ll: SymTable,
    of: SymTable,
    ml: SymTable,
}

fn by_symbol(t: FseTable) -> SymTable {
    let mut v = vec![Vec::new(); 64];
    for (i, c) in t.cells.iter().enumerate() {
        v[c.sym as usize].push(i as u16);
    }
    for l in v.iter_mut() {
        l.sort_by_key(|&i| t.cells[i as usize].base);
    }
    (t, v)
}

impl EncTables {
    fn new() -> Self {
        let b = |d: &[i16], al| by_symbol(fse::build(d, al).expect("预定义表合法"));
        Self {
            ll: b(&LL_DEFAULT, 6),
            of: b(&OF_DEFAULT, 5),
            ml: b(&ML_DEFAULT, 6),
        }
    }
}

/// 选下一个状态:符号 `sym` 的格子里,覆盖 `next` 的那个(`next` 为 None 时取第一个)。
/// 同一符号的格子按 base 排序后,各自的区间 [base, base + 2^nb) 恰好铺满整张表,所以二分即可。
fn pick(t: &SymTable, sym: u32, next: Option<usize>) -> (usize, u32, u32) {
    let list = &t.1[sym as usize];
    match next {
        None => (list[0] as usize, 0, 0),
        Some(x) => {
            let k = list.partition_point(|&i| t.0.cells[i as usize].base as usize <= x) - 1;
            let i = list[k] as usize;
            let c = t.0.cells[i];
            (i, c.nb as u32, (x - c.base as usize) as u32)
        }
    }
}

/// 三类序列码的表:预定义(None)或自定义(分布表头 + 表)。
struct SeqTables {
    modes: u8,
    desc: Vec<u8>,
    ll: SymTable,
    of: SymTable,
    ml: SymTable,
}

/// 按统计做自定义 FSE 表:（分布表头, 表）。序列太少不划算,返回 None。
fn custom_table(cnt: &[u32], al: u32, nseq: usize) -> Option<(Vec<u8>, SymTable)> {
    if nseq < 24 {
        return None;
    }
    let p = crate::henc::normalize(cnt, al)?;
    let head = crate::henc::write_dist(&p, al)?;
    let dist: Vec<i16> = p.iter().map(|&x| x as i16).collect();
    Some((head, by_symbol(fse::build(&dist, al).ok()?)))
}

/// 统计各码出现次数,够多就做自定义表,否则用预定义表。
fn choose_tables(seqs: &[Seq], pre: &EncTables) -> SeqTables {
    let mut cl = [0u32; 36];
    let mut co = [0u32; 32];
    let mut cm = [0u32; 53];
    for s in seqs {
        cl[ll_code(s.ll).0 as usize] += 1;
        let ofv = if s.rep { 1 } else { s.off + 3 };
        co[(31 - ofv.leading_zeros()) as usize] += 1;
        cm[ml_code(s.ml).0 as usize] += 1;
    }
    let mut modes = 0u8;
    let mut desc = Vec::new();
    let mut custom = |cnt: &[u32], al: u32, shift: u32, pre: &SymTable| match custom_table(
        cnt,
        al,
        seqs.len(),
    ) {
        Some((head, t)) => {
            modes |= 2 << shift;
            desc.extend_from_slice(&head);
            t
        }
        None => pre.clone(),
    };
    let ll = custom(&cl, 9, 6, &pre.ll);
    let of = custom(&co, 8, 4, &pre.of);
    let ml = custom(&cm, 9, 2, &pre.ml);
    SeqTables {
        modes,
        desc,
        ll,
        of,
        ml,
    }
}

fn encode_seqs(seqs: &[Seq], t: &SeqTables) -> Vec<u8> {
    let (tl, to, tm) = (&t.ll, &t.of, &t.ml);
    let mut w = BitWriter::new();
    let (mut cl, mut co, mut cm) = (0usize, 0usize, 0usize);
    for (k, s) in seqs.iter().enumerate().rev() {
        let (lc, lb, lv) = ll_code(s.ll);
        let (mc, mb, mv) = ml_code(s.ml);
        let ofv = if s.rep { 1 } else { s.off + 3 };
        let oc = 31 - ofv.leading_zeros();
        let (ob, ov) = (oc, ofv - (1 << oc));
        if k + 1 == seqs.len() {
            cl = pick(tl, lc, None).0;
            co = pick(to, oc, None).0;
            cm = pick(tm, mc, None).0;
        } else {
            let (i, n, v) = pick(to, oc, Some(co));
            w.put(v, n);
            co = i;
            let (i, n, v) = pick(tm, mc, Some(cm));
            w.put(v, n);
            cm = i;
            let (i, n, v) = pick(tl, lc, Some(cl));
            w.put(v, n);
            cl = i;
        }
        w.put(lv, lb);
        w.put(mv, mb);
        w.put(ov, ob);
    }
    w.put(cm as u32, tm.0.al);
    w.put(co as u32, to.0.al);
    w.put(cl as u32, tl.0.al);
    w.finish()
}

fn raw_block(out: &mut Vec<u8>, b: &[u8], last: bool) {
    let h = (b.len() << 3) as u32 | last as u32;
    out.extend_from_slice(&h.to_le_bytes()[..3]);
    out.extend_from_slice(b);
}

const WINDOW: usize = 1 << 16;
const HEAD_BITS: u32 = 15;

/// 哈希链匹配器:覆盖全部输入,距离不超过 WINDOW。
struct Matcher<'a> {
    d: &'a [u8],
    head: Vec<u32>,
    prev: Vec<u32>,
    depth: usize,
}

impl<'a> Matcher<'a> {
    fn h(&self, p: usize) -> usize {
        let v = u32::from_le_bytes([self.d[p], self.d[p + 1], self.d[p + 2], self.d[p + 3]]);
        (v.wrapping_mul(2654435761) >> (32 - HEAD_BITS)) as usize
    }
    fn insert(&mut self, p: usize) {
        if p + 4 <= self.d.len() {
            let h = self.h(p);
            self.prev[p & (WINDOW - 1)] = self.head[h];
            self.head[h] = p as u32;
        }
    }
    fn mlen(&self, a: usize, b: usize, end: usize) -> usize {
        let mut l = 0;
        while b + l + 8 <= end {
            let x = u64::from_le_bytes(self.d[a + l..a + l + 8].try_into().expect("8 字节"))
                ^ u64::from_le_bytes(self.d[b + l..b + l + 8].try_into().expect("8 字节"));
            if x != 0 {
                return l + (x.trailing_zeros() / 8) as usize;
            }
            l += 8;
        }
        while b + l < end && self.d[a + l] == self.d[b + l] {
            l += 1;
        }
        l
    }
    /// 在 i 处找最优匹配:(长度, 偏移)。`rep` 是上一次的偏移,命中时略占便宜。
    fn find(&self, i: usize, end: usize, rep: usize) -> (usize, usize, usize) {
        let mut best = (0usize, 0usize, 0usize);
        if i + MIN_MATCH > end {
            return best;
        }
        if rep <= i {
            let l = self.mlen(i - rep, i, end);
            if l >= MIN_MATCH {
                best = (l + 1, rep, l);
            }
        }
        let mut c = self.head[self.h(i)];
        for _ in 0..self.depth {
            if c == u32::MAX || (c as usize) >= i || i - c as usize >= WINDOW {
                break;
            }
            let cp = c as usize;
            let l = self.mlen(cp, i, end);
            if l > best.0 && l >= MIN_MATCH {
                best = (l, i - cp, l);
            }
            let n = self.prev[cp & (WINDOW - 1)];
            if n != u32::MAX && n >= c {
                break;
            }
            c = n;
        }
        best
    }
}

/// 压缩成一个 zstd 帧。`level <= 0` 只存放不匹配;更高的级别只影响搜索深度。
pub fn compress(data: &[u8], level: i32) -> Vec<u8> {
    let mut out = Vec::with_capacity(data.len() / 2 + 32);
    out.extend_from_slice(&0xFD2FB528u32.to_le_bytes());
    // 单段帧:窗口等于内容大小,内容大小用 4 或 8 字节
    if data.len() <= u32::MAX as usize {
        out.push(0x80 | 0x20 | 0x04);
        out.extend_from_slice(&(data.len() as u32).to_le_bytes());
    } else {
        out.push(0xC0 | 0x20 | 0x04);
        out.extend_from_slice(&(data.len() as u64).to_le_bytes());
    }
    if data.is_empty() {
        raw_block(&mut out, &[], true);
        out.extend_from_slice(&(crate::xxh::xxh64(data) as u32).to_le_bytes());
        return out;
    }
    let mut m = Matcher {
        d: data,
        head: vec![u32::MAX; 1 << HEAD_BITS],
        prev: vec![u32::MAX; WINDOW],
        depth: if level >= 9 {
            32
        } else if level >= 3 {
            4
        } else {
            2
        },
    };
    let tabs = EncTables::new();
    let mut rep = 1usize;
    let mut ni = 0usize;
    let mut start = 0;
    while start < data.len() {
        let end = (start + BLOCK).min(data.len());
        let last = end == data.len();
        let blk = &data[start..end];
        if level <= 0 {
            raw_block(&mut out, blk, last);
            start = end;
            continue;
        }
        let mut seqs: Vec<Seq> = Vec::new();
        let mut lits: Vec<u8> = Vec::new();
        let mut i = start;
        let mut anchor = start;
        let mut miss = 0usize;
        while i + MIN_MATCH <= end {
            while ni < i {
                m.insert(ni);
                ni += 1;
            }
            let mut cur = m.find(i, end, rep);
            if cur.0 >= MIN_MATCH {
                // 懒匹配:下一个位置明显更长就先吐一个字面量
                while cur.0 < 32 && i + 1 + MIN_MATCH <= end {
                    while ni <= i {
                        m.insert(ni);
                        ni += 1;
                    }
                    let nx = m.find(i + 1, end, rep);
                    if nx.0 > cur.0 + 1 {
                        i += 1;
                        cur = nx;
                    } else {
                        break;
                    }
                }
                miss = 0;
                let (_, off, l) = cur;
                let ll = i - anchor;
                lits.extend_from_slice(&data[anchor..i]);
                seqs.push(Seq {
                    ll: ll as u32,
                    ml: l as u32,
                    off: off as u32,
                    rep: off == rep && ll > 0,
                });
                rep = off;
                // 匹配体内只登记开头 2 个和结尾 2 个位置,中间跳过(省时间,略损压缩率)
                while ni < i + l.min(2) {
                    m.insert(ni);
                    ni += 1;
                }
                i += l;
                ni = ni.max(i.saturating_sub(2));
                anchor = i;
            } else {
                // 连续未命中越久步长越大,不可压缩的数据很快跳过
                i += 1 + (miss >> 6);
                miss += 1;
            }
        }
        lits.extend_from_slice(&data[anchor..end]);
        let mut body = Vec::new();
        let n = lits.len();
        match crate::henc::encode_literals(&lits) {
            Some(h) => body.extend_from_slice(&h),
            None => {
                body.push(3 << 2 | ((n & 15) << 4) as u8);
                body.push((n >> 4) as u8);
                body.push((n >> 12) as u8);
                body.extend_from_slice(&lits);
            }
        }
        if seqs.is_empty() {
            body.push(0);
        } else {
            let n = seqs.len();
            if n < 128 {
                body.push(n as u8);
            } else if n < 0x7F00 {
                body.push((n >> 8) as u8 + 128);
                body.push(n as u8);
            } else {
                body.push(255);
                body.extend_from_slice(&((n - 0x7F00) as u16).to_le_bytes());
            }
            let st = choose_tables(&seqs, &tabs);
            body.push(st.modes);
            body.extend_from_slice(&st.desc);
            body.extend_from_slice(&encode_seqs(&seqs, &st));
        }
        if body.len() >= blk.len() {
            raw_block(&mut out, blk, last);
        } else {
            let h = (body.len() << 3) as u32 | 4 | last as u32;
            out.extend_from_slice(&h.to_le_bytes()[..3]);
            out.extend_from_slice(&body);
        }
        start = end;
    }
    out.extend_from_slice(&(crate::xxh::xxh64(data) as u32).to_le_bytes());
    out
}
