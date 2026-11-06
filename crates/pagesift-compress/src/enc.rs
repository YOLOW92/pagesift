//! 基础编码器：哈希链 LZ 匹配（level ≥ 3 带懒匹配），序列用按块统计的自定义 FSE 表（太短的块用预定义表），字面量做 Huffman（不划算时原样）。
use crate::dec::{LL_BASE, LL_DEFAULT, ML_BASE, ML_DEFAULT, OF_DEFAULT};
use crate::fse::{self, FseTable};

const BLOCK: usize = 128 * 1024;

pub(crate) struct BitWriter {
    out: Vec<u8>,
    acc: u64,
    n: u32,
}

impl BitWriter {
    pub fn new() -> Self {
        Self::with_capacity(0)
    }
    pub fn with_capacity(capacity: usize) -> Self {
        Self {
            out: Vec::with_capacity(capacity),
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
    symbols: u64,
    /// 每个符号在 `flat` 里的起点，以及分桶移位。
    start: [u32; 64],
    shift: [u8; 64],
    /// （格子下标, nb, base）。
    flat: Vec<(u16, u8, u16)>,
}

struct EncTables {
    ll: SymTable,
    of: SymTable,
    ml: SymTable,
}

fn by_symbol(t: FseTable) -> SymTable {
    let size = t.cells.len();
    let mut shift = [u8::MAX; 64];
    let mut symbols = 0u64;
    for c in &t.cells {
        shift[c.sym as usize] = shift[c.sym as usize].min(c.nb);
        symbols |= 1u64 << c.sym;
    }
    let mut start = [0u32; 64];
    let mut total = 0;
    for s in 0..64 {
        start[s] = total as u32;
        if shift[s] != u8::MAX {
            total += size >> shift[s];
        } else {
            shift[s] = 0;
        }
    }
    let mut flat = vec![(0, 0, 0); total];
    for (idx, c) in t.cells.iter().enumerate() {
        let s = c.sym as usize;
        let lo = start[s] as usize + (c.base as usize >> shift[s]);
        flat[lo..lo + (1usize << (c.nb - shift[s]))].fill((idx as u16, c.nb, c.base));
    }
    SymTable {
        al: t.al,
        symbols,
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
struct SeqTables<'a> {
    modes: u8,
    desc: Vec<u8>,
    ll: std::borrow::Cow<'a, SymTable>,
    of: std::borrow::Cow<'a, SymTable>,
    ml: std::borrow::Cow<'a, SymTable>,
}

/// 按统计做自定义 FSE 表:（分布表头, 表）。
fn custom_table(cnt: &[u32], al: u32) -> Option<(Vec<u8>, SymTable)> {
    let p = crate::henc::normalize(cnt, al)?;
    let head = crate::henc::write_dist(&p, al)?;
    let dist: Vec<i16> = p.iter().map(|&x| x as i16).collect();
    Some((head, by_symbol(fse::build(&dist, al).ok()?)))
}

/// 统计各码出现次数,够多就做自定义表,否则用预定义表。
fn choose_tables<'a>(seqs: &[Seq], pre: &'a EncTables) -> SeqTables<'a> {
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
    let mut custom = |cnt: &[u32], al: u32, shift: u32, pre: &'a SymTable| {
        let required = cnt
            .iter()
            .enumerate()
            .any(|(s, &c)| c > 0 && pre.symbols & (1u64 << s) == 0);
        match if required || seqs.len() >= 24 {
            custom_table(cnt, al)
        } else {
            None
        } {
            Some((head, t)) => {
                modes |= 2 << shift;
                desc.extend_from_slice(&head);
                std::borrow::Cow::Owned(t)
            }
            None => std::borrow::Cow::Borrowed(pre),
        }
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

/// 压缩成一个 zstd 帧。`level <= 0` 只存放；正等级按输入和硬件选择匹配工作集。
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
    let budget = (level > 0).then(|| crate::resources::Budget::new(data.len(), level));
    let fast_budget = budget.as_ref().filter(|_| level < 9);
    let mut fast = fast_budget.map(|b| {
        crate::lz::FastMode::new(
            level >= 3,
            b.hash_bits,
            b.tagged,
            data.len() > u32::MAX as usize,
        )
    });
    let chain_budget = budget.as_ref().filter(|_| level >= 9);
    let mut chain = chain_budget
        .filter(|_| data.len() <= u32::MAX as usize)
        .map(crate::chain::Chain::<u32>::new);
    let mut wide_chain = chain_budget
        .filter(|_| data.len() > u32::MAX as usize)
        .map(crate::chain::Chain::<usize>::new);
    static TABLES: std::sync::OnceLock<EncTables> = std::sync::OnceLock::new();
    let tabs = (level > 0).then(|| TABLES.get_or_init(EncTables::new));
    let mut rep = 1usize;
    let mut start = 0;
    let mut seqs: Vec<Seq> = Vec::new();
    let mut lits: Vec<u8> = Vec::new();
    let mut body = Vec::new();
    while start < data.len() {
        let end = (start + BLOCK).min(data.len());
        let last = end == data.len();
        let blk = &data[start..end];
        if level <= 0 {
            raw_block(&mut out, blk, last);
            start = end;
            continue;
        }
        seqs.clear();
        lits.clear();
        body.clear();
        let saved_rep = rep;
        let anchor = if let Some(f) = fast.as_mut() {
            f.parse(data, (start, end), &mut rep, &mut seqs, &mut lits)
        } else if let Some(c) = chain.as_mut() {
            c.parse(data, (start, end), &mut rep, &mut seqs, &mut lits)
        } else if let Some(c) = wide_chain.as_mut() {
            c.parse(data, (start, end), &mut rep, &mut seqs, &mut lits)
        } else {
            start
        };
        lits.extend_from_slice(&data[anchor..end]);
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
            let st = choose_tables(&seqs, tabs.expect("正等级已初始化表"));
            body.push(st.modes);
            body.extend_from_slice(&st.desc);
            body.extend_from_slice(&encode_seqs(&seqs, &st));
        }
        if body.len() >= blk.len() {
            // Raw 块没有序列，不改变解码器的重复偏移历史。
            rep = saved_rep;
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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn sparse_large_offsets_require_custom_table() {
        let pre = EncTables::new();
        for off in [1 << 29, u32::MAX - 3] {
            let seqs = [Seq {
                ll: 0,
                ml: 4,
                off,
                rep: false,
            }];
            let tables = choose_tables(&seqs, &pre);
            assert_eq!((tables.modes >> 4) & 3, 2);
            let sym = 31 - (off + 3).leading_zeros();
            assert_ne!(tables.of.symbols & (1u64 << sym), 0);
            let (decoded, used) = fse::read_table(&tables.desc, 31, 8).unwrap();
            assert_eq!(used, tables.desc.len());
            assert!(decoded.cells.iter().any(|c| c.sym as u32 == sym));
            assert!(!encode_seqs(&seqs, &tables).is_empty());
        }
    }
}
