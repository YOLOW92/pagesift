//! 霍夫曼表（T.88 附录 B）：标准表 B.1–B.15、码表段（§7.4.13）、按码长赋码（§B.3），
//! 和高位在前的读位器。
//!
//! 三种来源的表（标准表、码表段、文本区域的符号 ID 表）都先写成“表行”（前缀码长、偏移位数、
//! 区间下端、行的种类），再由同一个赋码过程（§B.3）生成前缀码。赋出的码是规范霍夫曼码：
//! 同一码长的码连续递增，所以解码时逐位累加、按码长比对首码和个数即可，不用建树。

mod standard;
#[cfg(test)]
mod tests;

/// 高位在前的读位器；霍夫曼编码的符号字典、文本区域和码表段用。
pub(super) struct Bits<'a> {
    data: &'a [u8],
    pos: usize,
}

/// [推导] 一字节 8 位
const BYTE_BITS: u32 = 8;

/// [推导] `bits` 一次最多读 32 位（返回 u32）
const MAX_READ: u32 = 32;

impl<'a> Bits<'a> {
    pub(super) fn new(data: &'a [u8]) -> Bits<'a> {
        Bits { data, pos: 0 }
    }

    /// 读 `n` 位（n ≤ 32）；读过末尾报错。
    pub(super) fn bits(&mut self, n: u32) -> Result<u32, String> {
        if n > MAX_READ {
            return Err(format!("一次读 {n} 位超过了 32 位"));
        }
        let end = self.pos + n as usize;
        if end > self.data.len() * BYTE_BITS as usize {
            return Err("霍夫曼编码数据读过了末尾".into());
        }
        // 按字节成块取：每次取当前字节里剩下的位和还要的位中较少的一段
        let mut v = 0u64;
        let mut left = n;
        while left > 0 {
            let used = (self.pos % BYTE_BITS as usize) as u32;
            let avail = BYTE_BITS - used;
            let take = avail.min(left);
            let byte = u32::from(self.data[self.pos / BYTE_BITS as usize]);
            let chunk = (byte >> (avail - take)) & ((1u32 << take) - 1);
            v = (v << take) | u64::from(chunk);
            self.pos += take as usize;
            left -= take;
        }
        Ok(v as u32)
    }

    /// 读 1 位；读过末尾报错。解码前缀码时逐位用。
    pub(super) fn bit(&mut self) -> Result<u32, String> {
        let byte = self
            .data
            .get(self.pos / BYTE_BITS as usize)
            .ok_or("霍夫曼编码数据读过了末尾")?;
        // [推导] 高位在前：字节内第 k 位（从 0 数）在 7 − k 的位置上
        let b = (byte >> (BYTE_BITS as usize - 1 - self.pos % BYTE_BITS as usize)) & 1;
        self.pos += 1;
        Ok(u32::from(b))
    }

    /// 跳到下一个字节边界。
    pub(super) fn align(&mut self) {
        self.pos = self.pos.next_multiple_of(BYTE_BITS as usize);
    }

    /// 当前位置所在字节之后的下标（先 [`Bits::align`] 再取，就是下一段数据的起点）。
    pub(super) fn byte_pos(&self) -> usize {
        self.pos.div_ceil(BYTE_BITS as usize)
    }
}

/// 表行的种类（§B.1）。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Kind {
    /// 普通行：值 = RANGELOW + 读出的 RANGELEN 位。
    Normal,
    /// 下界行（lower range）：值 = RANGELOW − 读出的 RANGELEN 位。
    Lower,
    /// 上界行（upper range）：值 = RANGELOW + 读出的 RANGELEN 位（32 位）。
    Upper,
    /// OOB 行：没有值。
    Oob,
}

/// 一个表行：PREFLEN、RANGELEN、RANGELOW 和种类（§B.1）。PREFLEN 为 0 的行不分配码。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct Line {
    preflen: u8,
    rangelen: u8,
    low: i64,
    kind: Kind,
}

/// [协议] 上界行、下界行的偏移固定 32 位（§B.2 步骤 5、6，附录 B.5 各表）
const RANGE_BITS: u8 = 32;

/// 普通行。
const fn normal(preflen: u8, rangelen: u8, low: i64) -> Line {
    Line {
        preflen,
        rangelen,
        low,
        kind: Kind::Normal,
    }
}

/// 下界行：值 = `low` − 32 位偏移。
const fn lower(preflen: u8, low: i64) -> Line {
    Line {
        preflen,
        rangelen: RANGE_BITS,
        low,
        kind: Kind::Lower,
    }
}

/// 上界行：值 = `low` + 32 位偏移。
const fn upper(preflen: u8, low: i64) -> Line {
    Line {
        preflen,
        rangelen: RANGE_BITS,
        low,
        kind: Kind::Upper,
    }
}

/// OOB 行。
const fn oob(preflen: u8) -> Line {
    Line {
        preflen,
        rangelen: 0,
        low: 0,
        kind: Kind::Oob,
    }
}

impl Line {
    /// 前缀码之后：读偏移位，算出值（§B.4）。
    fn value(&self, r: &mut Bits) -> Result<Option<i32>, String> {
        let off = i64::from(r.bits(u32::from(self.rangelen))?);
        let v = match self.kind {
            Kind::Oob => return Ok(None),
            Kind::Lower => self.low - off,
            Kind::Normal | Kind::Upper => self.low + off,
        };
        i32::try_from(v)
            .map(Some)
            .map_err(|_| format!("霍夫曼解出的值 {v} 超出 32 位整数"))
    }
}

/// [策略] 前缀码最长 32 位：标准表最长 9 位，符号 ID 码长至多 31（游程码 0–31，§7.4.3.1.7），
/// 码表段的 HTPS 最多 8 位理论上能写到 255，超过 32 的当作损坏的表
const MAX_PREFLEN: usize = 32;

/// 按码长给各行赋前缀码（§B.3），返回每行的码（PREFLEN 为 0 的行是 0，不用）。
/// 某个码长上的码超出了这个长度能表示的范围（码长不满足 Kraft 不等式）时报错。
fn assign(lens: &[u8]) -> Result<Vec<u32>, String> {
    // 步骤 1：LENCOUNT[i] = 码长为 i 的行数；LENCOUNT[0] = 0
    let mut count = [0u64; MAX_PREFLEN + 1];
    for &l in lens {
        let l = usize::from(l);
        if l > MAX_PREFLEN {
            return Err(format!("霍夫曼前缀码长 {l} 超过 32 位"));
        }
        count[l] += 1;
    }
    count[0] = 0;
    let max = lens.iter().copied().max().map_or(0, usize::from);
    // 步骤 2、3：FIRSTCODE[0] = 0；FIRSTCODE[CURLEN] = (FIRSTCODE[CURLEN−1] + LENCOUNT[CURLEN−1]) × 2
    let mut next = [0u64; MAX_PREFLEN + 1];
    let mut first = 0u64;
    for len in 1..=max {
        first = (first + count[len - 1]) << 1;
        if first + count[len] > 1u64 << len {
            return Err(format!("霍夫曼码长不成前缀码：{len} 位的码超额"));
        }
        next[len] = first;
    }
    // 同一码长的行按表里的顺序依次取 CURCODE、CURCODE + 1……
    let mut codes = vec![0u32; lens.len()];
    for (c, &l) in codes.iter_mut().zip(lens) {
        let l = usize::from(l);
        if l > 0 {
            *c = next[l] as u32;
            next[l] += 1;
        }
    }
    Ok(codes)
}

/// 一张霍夫曼表。
#[derive(Debug)]
pub(super) struct Table {
    /// 有码的行，按码长排序（同一码长保持表里的顺序，也就是码从小到大）。
    lines: Vec<Line>,
    /// 每个码长的首码。
    first: [u32; MAX_PREFLEN + 1],
    /// 每个码长的行数。
    count: [u32; MAX_PREFLEN + 1],
    /// 每个码长的行在 `lines` 里的起点。
    start: [u32; MAX_PREFLEN + 1],
    /// 最长的码长。
    max_len: usize,
}

impl Table {
    /// 由表行建表：按 §B.3 赋码，再按码长排好供解码。
    fn build(rows: &[Line]) -> Result<Table, String> {
        let lens: Vec<u8> = rows.iter().map(|l| l.preflen).collect();
        let codes = assign(&lens)?;
        let mut order: Vec<usize> = (0..rows.len()).filter(|&i| lens[i] > 0).collect();
        order.sort_by_key(|&i| lens[i]); // 稳定排序：同一码长保持表里的顺序
        let mut t = Table {
            lines: Vec::with_capacity(order.len()),
            first: [0; MAX_PREFLEN + 1],
            count: [0; MAX_PREFLEN + 1],
            start: [0; MAX_PREFLEN + 1],
            max_len: 0,
        };
        for &i in &order {
            let l = usize::from(lens[i]);
            if t.count[l] == 0 {
                t.first[l] = codes[i];
                t.start[l] = t.lines.len() as u32;
            }
            t.count[l] += 1;
            t.max_len = t.max_len.max(l);
            t.lines.push(rows[i]);
        }
        Ok(t)
    }

    /// 解一个值；None 是 OOB。
    pub(super) fn decode(&self, r: &mut Bits) -> Result<Option<i32>, String> {
        let mut code = 0u32;
        for len in 1..=self.max_len {
            code = (code << 1) | r.bit()?;
            let first = self.first[len];
            if code >= first && code - first < self.count[len] {
                let i = self.start[len] + (code - first);
                return self.lines[i as usize].value(r);
            }
        }
        Err("霍夫曼码不在表里".into())
    }
}

/// 标准表 B.n（n = 1–15）。
pub(super) fn standard(n: u8) -> Result<&'static Table, String> {
    standard::get(n)
}

/// [协议] 码表段数据头：1 字节标志、4 字节 HTLOW、4 字节 HTHIGH（§7.4.13.1–3）
const CUSTOM_HEAD: usize = 9;

/// 码表段（类型 53）的数据（§7.4.13、§B.2）。
pub(super) fn custom(data: &[u8]) -> Result<Table, String> {
    let head = data.get(..CUSTOM_HEAD).ok_or("码表段太短")?;
    let flags = head[0];
    // [协议] 码表段标志（§7.4.13.1）：位 0 HTOOB，位 1–3 是 HTPS − 1，位 4–6 是 HTRS − 1
    let has_oob = flags & 1 != 0;
    let ps = u32::from((flags >> 1) & 7) + 1; // [协议] HTPS：位 1–3 加 1（§7.4.13.1）
    let rs = u32::from((flags >> 4) & 7) + 1; // [协议] HTRS：位 4–6 加 1（§7.4.13.1）
    // [协议] HTLOW、HTHIGH 是 4 字节有符号大端整数（§7.4.13.2、§7.4.13.3）
    let low = i64::from(i32::from_be_bytes([head[1], head[2], head[3], head[4]]));
    let high = i64::from(i32::from_be_bytes([head[5], head[6], head[7], head[8]]));
    let mut r = Bits::new(&data[CUSTOM_HEAD..]);
    let mut rows = Vec::new();
    // §B.2 步骤 3、4：从 HTLOW 起逐行读 PREFLEN（HTPS 位）、RANGELEN（HTRS 位），
    // 每行覆盖 2^RANGELEN 个值，直到覆盖到 HTHIGH。每行至少 2 位，行数受数据长度限制。
    let mut cur = low;
    while cur < high {
        let preflen = r.bits(ps)? as u8;
        let rangelen = r.bits(rs)?;
        if rangelen > u32::from(RANGE_BITS) {
            return Err(format!("码表段的偏移位数 {rangelen} 超过 32 位"));
        }
        rows.push(normal(preflen, rangelen as u8, cur));
        cur += 1i64 << rangelen;
    }
    // §B.2 步骤 5：下界行，RANGELOW = HTLOW − 1
    rows.push(lower(r.bits(ps)? as u8, low - 1));
    // §B.2 步骤 6：上界行，RANGELOW = HTHIGH
    rows.push(upper(r.bits(ps)? as u8, high));
    // §B.2 步骤 7：HTOOB 为 1 时追加 OOB 行
    if has_oob {
        rows.push(oob(r.bits(ps)? as u8));
    }
    Table::build(&rows)
}

/// [协议] 符号 ID 游程码有 35 个（值 0–34），各用 4 位写码长（§7.4.3.1.7）
const RUN_CODES: usize = 35;
/// [协议] 游程码码长的位数（§7.4.3.1.7）
const RUN_LEN_BITS: u32 = 4;
/// [协议] 游程码 0–31 直接就是码长（§7.4.3.1.7）
const RUN_LITERAL_MAX: i32 = 31;
/// [协议] 游程码 32：重复前一个码长 3–6 次，后跟 2 位（§7.4.3.1.7）
const RUN_REPEAT: i32 = 32;
/// [协议] 游程码 33：码长 0 重复 3–10 次，后跟 3 位（§7.4.3.1.7）
const RUN_ZEROS_SHORT: i32 = 33;
/// [协议] 游程码 34：码长 0 重复 11–138 次，后跟 7 位（§7.4.3.1.7）
const RUN_ZEROS_LONG: i32 = 34;

/// 文本区域的符号 ID 霍夫曼表（§7.4.3.1.7）：从 `r` 读 35 个游程码长和 `count` 个符号的码长，
/// 读完后对齐到字节边界。
pub(super) fn symbol_ids(r: &mut Bits, count: usize) -> Result<Table, String> {
    // 1. 35 个游程码的码长，各 4 位；按 §B.3 赋码，值就是游程码本身
    let mut runs = Vec::with_capacity(RUN_CODES);
    for i in 0..RUN_CODES {
        runs.push(normal(r.bits(RUN_LEN_BITS)? as u8, 0, i as i64));
    }
    let runs = Table::build(&runs)?;
    // 2. 用游程码解出 count 个符号的码长。count 来自码流，不按它预先分配
    let mut lens: Vec<u8> = Vec::new();
    while lens.len() < count {
        let code = runs.decode(r)?.ok_or("符号 ID 游程码表不该有 OOB")?;
        let (len, times) = match code {
            0..=RUN_LITERAL_MAX => (code as u8, 1),
            RUN_REPEAT => {
                let prev = *lens.last().ok_or("游程码 32 之前没有码长可重复")?;
                // [协议] 3 + 2 位（§7.4.3.1.7）
                (prev, 3 + r.bits(2)? as usize)
            }
            // [协议] 3 + 3 位（§7.4.3.1.7）
            RUN_ZEROS_SHORT => (0, 3 + r.bits(3)? as usize),
            // [协议] 11 + 7 位（§7.4.3.1.7）
            RUN_ZEROS_LONG => (0, 11 + r.bits(7)? as usize),
            _ => return Err(format!("符号 ID 游程码 {code} 无效")),
        };
        if lens.len() + times > count {
            return Err("符号 ID 码长的游程超出了符号总数".into());
        }
        lens.resize(lens.len() + times, len);
    }
    // 3. 读完码长后跳过当前字节剩下的位（§7.4.3.1.7）
    r.align();
    // 4. 按 §B.3 给符号 ID 赋码，值就是符号 ID
    let rows: Vec<Line> = lens
        .iter()
        .enumerate()
        .map(|(i, &l)| normal(l, 0, i as i64))
        .collect();
    Table::build(&rows)
}
