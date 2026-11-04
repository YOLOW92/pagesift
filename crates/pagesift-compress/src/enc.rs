//! 基础编码器：哈希链 LZ 匹配（level ≥ 3 带懒匹配），序列用按块统计的自定义 FSE 表（太短的块用预定义表），字面量做 Huffman（不划算时原样）。
use crate::dec::{LL_BASE, LL_DEFAULT, ML_BASE, ML_DEFAULT, OF_DEFAULT};
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
        let mut n = self.n;
        while n > 0 {
            self.out.push(self.acc as u8);
            self.acc >>= 8;
            n = n.saturating_sub(8);
        }
        self.out
    }
    pub fn put(&mut self, v: u32, bits: u32) {
        if bits == 0 {
            return;
        }
        self.acc |= (v as u64 & ((1u64 << bits) - 1)) << self.n;
        self.n += bits;
        if self.n >= 32 {
            self.out.extend_from_slice(&(self.acc as u32).to_le_bytes());
            self.acc >>= 32;
            self.n -= 32;
        }
    }
    /// 写结束哨兵位并补齐字节。
    pub fn finish(mut self) -> Vec<u8> {
        self.put(1, 1);
        self.flush_bytes()
    }
}

/// 小值（< N）直接查表得到码；用 `bases` 里的基值反推。
const fn small_codes<const N: usize, const M: usize>(
    bases: &[(u32, u32); M],
    delta: u32,
) -> [u8; N] {
    let mut t = [0u8; N];
    let mut v = 0;
    let mut c = 0;
    while v < N {
        while c + 1 < M && (bases[c + 1].0 as usize) <= v + delta as usize {
            c += 1;
        }
        t[v] = c as u8;
        v += 1;
    }
    t
}

static LL_TAB: [u8; 64] = small_codes(&LL_BASE, 0);
static ML_TAB: [u8; 128] = small_codes(&ML_BASE, 3);

/// 返回（码，额外位数，额外值）。大值的码是 highbit 加常数，小值查表，没有循环。
fn ll_code(v: u32) -> (u32, u32, u32) {
    let c = if v < 64 {
        LL_TAB[v as usize] as u32
    } else {
        19 + (31 - v.leading_zeros())
    };
    let (base, bits) = LL_BASE[c as usize];
    (c, bits, v - base)
}

fn ml_code(v: u32) -> (u32, u32, u32) {
    let m = v - 3;
    let c = if m < 128 {
        ML_TAB[m as usize] as u32
    } else {
        36 + (31 - m.leading_zeros())
    };
    let (base, bits) = ML_BASE[c as usize];
    (c, bits, v - base)
}

pub(crate) struct Seq {
    pub ll: u32,
    pub ml: u32,
    pub off: u32,
    /// 用重复偏移 1(上一次的偏移,且 ll > 0)。
    pub rep: bool,
}

/// 三张预定义表,及每个符号对应的格子下标(只建一次)。
/// 编码用的表。同一符号的格子区间 [base, base + 2^nb) 恰好铺满整张表，
/// 所以按 `x >> 最小nb` 分桶后每个桶落在唯一一个区间里，转移查找是一次下标读取，没有搜索和分支。
#[derive(Clone)]
struct SymTable {
    al: u32,
    /// 每个符号在 `flat` 里的起点，以及分桶移位。
    start: Vec<u32>,
    shift: Vec<u8>,
    /// （格子下标, nb, base）。
    flat: Vec<(u16, u8, u16)>,
}

#[derive(Clone)]
struct EncTables {
    ll: SymTable,
    of: SymTable,
    ml: SymTable,
}

fn by_symbol(t: FseTable) -> SymTable {
    let size = t.cells.len();
    let mut lists: Vec<Vec<(u16, u16, u8)>> = vec![Vec::new(); 64];
    for (i, c) in t.cells.iter().enumerate() {
        lists[c.sym as usize].push((c.base, i as u16, c.nb));
    }
    let (mut start, mut shift, mut flat) = (vec![0u32; 64], vec![0u8; 64], Vec::new());
    for (s, l) in lists.iter().enumerate() {
        start[s] = flat.len() as u32;
        let Some(sh) = l.iter().map(|c| c.2).min() else {
            continue;
        };
        shift[s] = sh;
        flat.resize(flat.len() + (size >> sh), (0, 0, 0));
        for &(base, idx, nb) in l {
            let lo = (base as usize) >> sh;
            for slot in
                &mut flat[start[s] as usize + lo..start[s] as usize + lo + (1usize << (nb - sh))]
            {
                *slot = (idx, nb, base);
            }
        }
    }
    SymTable {
        al: t.al,
        start,
        shift,
        flat,
    }
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

/// 选下一个状态：符号 `sym` 的格子里，覆盖 `next` 的那个（`next` 为 None 时随便取一个）。
#[inline(always)]
fn pick(t: &SymTable, sym: u32, next: Option<usize>) -> (usize, u32, u32) {
    let s = sym as usize;
    let x = next.unwrap_or(0);
    let (idx, nb, base) = t.flat[t.start[s] as usize + (x >> t.shift[s])];
    if next.is_none() {
        return (idx as usize, 0, 0);
    }
    (idx as usize, nb as u32, (x - base as usize) as u32)
}

/// 三类序列码的表,及模式字节和自定义表的分布表头。
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
            // 写入顺序（反向）：of、ml、ll 的状态转移位，合并成一次 put
            let (i1, n1, v1) = pick(to, oc, Some(co));
            let (i2, n2, v2) = pick(tm, mc, Some(cm));
            let (i3, n3, v3) = pick(tl, lc, Some(cl));
            (co, cm, cl) = (i1, i2, i3);
            w.put(v1 | v2 << n1 | v3 << (n1 + n2), n1 + n2 + n3);
        }
        // 额外位：ll、ml 合并（各 ≤ 16 位），of 单独（≤ 31 位）
        w.put(lv | mv << lb, lb + mb);
        w.put(ov, ob);
    }
    w.put(cm as u32, tm.al);
    w.put(co as u32, to.al);
    w.put(cl as u32, tl.al);
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
            // 先比 4 字节和已有最佳长度处的一个字节,多数候选在这里就被淘汰
            if self.d[cp..cp + 4] != self.d[i..i + 4]
                || (best.0 > 0 && i + best.0 < end && self.d[cp + best.0] != self.d[i + best.0])
            {
                let n = self.prev[cp & (WINDOW - 1)];
                if n != u32::MAX && n >= c {
                    break;
                }
                c = n;
                continue;
            }
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
            1
        },
    };
    let mut fast = (level < 9).then(|| crate::lz::Fast::new(level >= 3));
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
        let anchor = if let Some(f) = fast.as_mut() {
            f.parse(data, (start, end), &mut rep, &mut seqs, &mut lits)
        } else {
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
                    while level >= 3 && cur.0 < 16 && i + 1 + MIN_MATCH <= end {
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
            anchor
        };
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
