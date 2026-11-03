//! 霍夫曼表（T.88 附录 B）：标准表 B.1–B.15、码表段（§7.4.13）、按码长赋码（§B.3），
//! 和高位在前的读位器。
//! 【待实现】接口已定（`Bits` 已实现）。

/// 高位在前的读位器；霍夫曼编码的符号字典、文本区域和码表段用。
pub(super) struct Bits<'a> {
    data: &'a [u8],
    pos: usize,
}

impl<'a> Bits<'a> {
    pub(super) fn new(data: &'a [u8]) -> Bits<'a> {
        Bits { data, pos: 0 }
    }

    /// 读 `n` 位（n ≤ 32）；读过末尾报错。
    pub(super) fn bits(&mut self, n: u32) -> Result<u32, String> {
        if self.pos + n as usize > self.data.len() * 8 {
            return Err("霍夫曼编码数据读过了末尾".into());
        }
        let mut v = 0u64;
        for _ in 0..n {
            let b = self.data[self.pos >> 3] >> (7 - (self.pos & 7)) & 1;
            v = (v << 1) | u64::from(b);
            self.pos += 1;
        }
        Ok(v as u32)
    }

    /// 跳到下一个字节边界。
    pub(super) fn align(&mut self) {
        self.pos = self.pos.next_multiple_of(8);
    }

    /// 当前位置所在字节之后的下标（先 [`Bits::align`] 再取，就是下一段数据的起点）。
    pub(super) fn byte_pos(&self) -> usize {
        self.pos.div_ceil(8)
    }
}

/// 一张霍夫曼表。
#[derive(Debug)]
pub(super) struct Table {}

impl Table {
    /// 解一个值；None 是 OOB。
    pub(super) fn decode(&self, _r: &mut Bits) -> Result<Option<i32>, String> {
        Err("霍夫曼表尚未实现".into())
    }
}

/// 标准表 B.n（n = 1–15）。
pub(super) fn standard(n: u8) -> Result<&'static Table, String> {
    Err(format!("标准霍夫曼表 B.{n} 尚未实现"))
}

/// 码表段（类型 53）的数据（§7.4.13、§B.2）。
pub(super) fn custom(_data: &[u8]) -> Result<Table, String> {
    Err("码表段尚未实现".into())
}

/// 文本区域的符号 ID 霍夫曼表（§7.4.3.1.7）：从 `r` 读 35 个游程码长和 `count` 个符号的码长，
/// 读完后对齐到字节边界。
pub(super) fn symbol_ids(_r: &mut Bits, _count: usize) -> Result<Table, String> {
    Err("符号 ID 霍夫曼表尚未实现".into())
}
