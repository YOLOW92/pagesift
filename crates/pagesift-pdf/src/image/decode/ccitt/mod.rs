//! CCITT 传真解码：ITU-T T.4 的一维（MH）、二维（MR）编码和 T.6（MMR），即 PDF 的
//! CCITTFaxDecode（ISO 32000-2 §7.4.6）。输出 8 位灰度，取值按滤镜的约定：/BlackIs1 为假
//! （缺省）时黑是 0、白是 255；/Decode 由调用方再套。
//!
//! 每行记成“变化元素”的位置（T.4 §4.2.1.3.1）：下标为偶数的是转黑处，奇数的是转白处。

mod bits;
mod codes;
#[cfg(test)]
mod tests;

use super::Raster;
use crate::image::tiff::Ccitt;
use bits::Bits;
use codes::{LOOKUP_BITS, Tables};

/// 解一段码流。`max_bytes` 是输出大小上限（防解压炸弹），PDF 里沿用 `Options::max_stream_bytes`。
/// 行数为 0（/Rows 缺省且不知道图高）时解到块结束码或数据末尾。中途出错时保留已解出的行，
/// 其余行留白（受损的传真常见，能读的部分仍有用）；第一行就解不开才报错。
pub(crate) fn decode(data: &[u8], p: &Ccitt, max_bytes: usize) -> Result<Raster, String> {
    decode_counted(data, p, max_bytes).map(|(r, _)| r)
}

/// 同 [`decode`]，另外返回用掉的字节数（含块结束码，按字节补齐）。JBIG2 的 MMR 编码区域
/// 一段数据里接连放几张图（半色调的灰度位平面），要知道下一张从哪里开始。
pub(crate) fn decode_counted(
    data: &[u8],
    p: &Ccitt,
    max_bytes: usize,
) -> Result<(Raster, usize), String> {
    let w = p.columns as usize;
    let rows = p.rows as usize;
    if w == 0 {
        return Err("行宽为 0".into());
    }
    if w.saturating_mul(rows) > max_bytes {
        return Err(format!("{w}×{rows} 超过上限 {max_bytes} 字节"));
    }
    let (black, white) = if p.black_is_1 { (255, 0) } else { (0, 255) }; // [协议] 8 位灰度两端
    let t = codes::tables();
    let mut bits = Bits::new(data);
    let mut out: Vec<u8> = Vec::with_capacity(w * rows);
    // [协议] 第一行的参考行是虚构的全白行（T.4 §4.2.1.3.4）
    let mut refl: Vec<usize> = vec![w, w];
    let mut cur: Vec<usize> = Vec::with_capacity(w + 2);
    let mut done = 0usize;
    while rows == 0 || done < rows {
        // 到时限了就按“中途出错”处理：保留已解出的行
        if crate::budget::expired() {
            break;
        }
        let two_d = match row_start(&mut bits, p) {
            Some(d) => d,
            None => break,
        };
        let r = if two_d {
            row_2d(&mut bits, t, w, &refl, &mut cur)
        } else {
            row_1d(&mut bits, t, w, &mut cur)
        };
        if let Err(e) = r {
            if done == 0 {
                return Err(e);
            }
            break;
        }
        if out.len() + w > max_bytes {
            return Err(format!("解出的像素超过上限 {max_bytes} 字节"));
        }
        let at = out.len();
        out.resize(at + w, white);
        for pair in cur.chunks(2) {
            let end = pair.get(1).copied().unwrap_or(w);
            out[at + pair[0]..at + end].fill(black);
        }
        std::mem::swap(&mut refl, &mut cur);
        refl.extend([w, w]);
        done += 1;
    }
    // 行数已知、解够了就停的 T.6 码流后面可能还跟着 EOFB：一并读掉，用掉的字节数才指向下一段
    // （JBIG2 半色调的灰度位平面就是这样首尾相接的，T.88 §6.2.6）
    // [协议] EOFB 是两个 EOL，共 24 位（T.6 §2.4.3）
    if p.k < 0 && done == rows && bits.peek(24) == 0x1001 {
        bits.skip(24); // [协议] EOFB 共 24 位
    }
    if done == 0 {
        return Err("没有解出任何行".into());
    }
    let height = rows.max(done);
    out.resize(w * height, white);
    let r = Raster {
        width: p.columns,
        height: height as u32,
        channels: 1,
        data: out,
    };
    Ok((r, bits.bytes_read()))
}

/// 行首：读掉行尾码和对齐填充，判断这一行是一维还是二维。块结束（T.6 的 EOFB、T.4 的 RTC）
/// 或数据读完时返回 None。
fn row_start(bits: &mut Bits, p: &Ccitt) -> Option<bool> {
    if p.k < 0 {
        // [协议] T.6：没有行尾码；/EncodedByteAlign 时每行从字节边界开始（ISO 32000-2 表 11）
        if p.byte_align {
            bits.align();
        }
        // [协议] EOFB 是两个 EOL（T.6 §2.4.3），看到一个就够了；读掉它，用掉的字节数才准
        if bits.at_end() {
            return None;
        }
        // [协议] 12 位的 EOL（000000000001，T.4 §4.1.2）
        if bits.peek(12) == 1 {
            // [协议] 两个 EOL 连写即 EOFB：24 位 0x001001（T.6）
            if bits.peek(24) == 0x1001 {
                bits.skip(24); // [协议] EOFB 共 24 位
            }
            return None;
        }
        return Some(true);
    }
    let mut eols = 0;
    let mut tag = None;
    while bits.eol() {
        eols += 1;
        if p.k > 0 {
            // [协议] T.4 二维编码：EOL 后跟 1 位，1 是一维行、0 是二维行（§4.2.1.3）
            tag = Some(bits.bit());
        }
    }
    // [协议] RTC 是连续 6 个 EOL（T.4 §4.1.4）；正常行之间只有一个
    if eols >= 2 {
        return None;
    }
    if eols == 0 && p.byte_align {
        bits.align();
    }
    if p.k > 0 && tag.is_none() {
        tag = Some(bits.bit());
    }
    if bits.at_end() {
        return None;
    }
    Some(tag == Some(0))
}

/// 一个游程：若干组合码加一个终止码（T.4 §4.1.1）。
fn run(bits: &mut Bits, t: &Tables, white: bool) -> Result<usize, String> {
    let mut total = 0usize;
    loop {
        let (len, r) = t.get(white, bits.peek(LOOKUP_BITS)).ok_or("无效的游程码")?;
        bits.skip(len);
        total += r;
        // [协议] 小于 64 的是终止码
        if r < 64 {
            return Ok(total);
        }
    }
}

/// 一维行（MH）：白黑交替的游程，从白开始。
fn row_1d(bits: &mut Bits, t: &Tables, w: usize, cur: &mut Vec<usize>) -> Result<(), String> {
    cur.clear();
    let (mut pos, mut white) = (0usize, true);
    while pos < w {
        // 超出行宽的游程截到行宽（写入方的错误，阅读器通常容忍）
        pos = (pos + run(bits, t, white)?).min(w);
        cur.push(pos);
        white = !white;
        // [推导] 同二维行：一行至多 w + 1 个变化元素；连续的 0 游程不推进位置，再多说明码流在原地打转
        if cur.len() > w + 2 {
            return Err("变化元素过多".into());
        }
    }
    Ok(())
}

enum Mode {
    Pass,
    Horizontal,
    /// a1 相对 b1 的偏移，−3 到 3。
    Vertical(isize),
}

/// [协议] T.4 表 4 的二维模式码
fn mode(bits: &mut Bits) -> Result<Mode, String> {
    // [协议] 二维模式码最长 7 位（T.4 表 4）
    let v = bits.peek(7);
    let (len, m) = match v {
        // [协议] 按 T.4 表 4 各码字的前缀分段；元组第一项是码长，Vertical 里是 a1 相对 b1 的偏移
        0b100_0000..=0b111_1111 => (1, Mode::Vertical(0)),
        0b011_0000..=0b011_1111 => (3, Mode::Vertical(1)),
        0b010_0000..=0b010_1111 => (3, Mode::Vertical(-1)),
        0b001_0000..=0b001_1111 => (3, Mode::Horizontal),
        0b000_1000..=0b000_1111 => (4, Mode::Pass),
        0b000_0110 | 0b000_0111 => (6, Mode::Vertical(2)),
        0b000_0100 | 0b000_0101 => (6, Mode::Vertical(-2)),
        0b000_0011 => (7, Mode::Vertical(3)),
        0b000_0010 => (7, Mode::Vertical(-3)),
        _ => return Err("扩展码、行尾码或无效的二维模式码".into()),
    };
    bits.skip(len);
    Ok(m)
}

/// 二维行（MR、MMR），T.4 §4.2.1.3 的编码过程反过来。`refl` 是参考行的变化元素，末尾补了两个行宽。
fn row_2d(
    bits: &mut Bits,
    t: &Tables,
    w: usize,
    refl: &[usize],
    cur: &mut Vec<usize>,
) -> Result<(), String> {
    cur.clear();
    // a0 从行首前面的虚拟位置开始，用 None 表示
    let mut a0: Option<usize> = None;
    let mut white = true;
    let mut i = 0usize;
    while a0.is_none_or(|a| a < w) {
        // b1：参考行上 a0 右边第一个、颜色与 a0 相反的变化元素；a0 白时找转黑处（偶数下标）。
        // i 只记“a0 右边第一个”：颜色不对跳过的那个，向左垂直模式翻色之后可能就是下一个 b1
        while i < refl.len() && a0.is_some_and(|a| refl[i] <= a) {
            i += 1;
        }
        let j = if i % 2 == usize::from(!white) {
            i
        } else {
            i + 1
        };
        let b1 = refl.get(j).copied().unwrap_or(w);
        let b2 = refl.get(j + 1).copied().unwrap_or(w);
        match mode(bits)? {
            Mode::Pass => a0 = Some(b2),
            Mode::Horizontal => {
                let start = a0.unwrap_or(0);
                let a1 = (start + run(bits, t, white)?).min(w);
                let a2 = (a1 + run(bits, t, !white)?).min(w);
                cur.extend([a1, a2]);
                a0 = Some(a2);
            }
            Mode::Vertical(d) => {
                let a1 = b1.checked_add_signed(d).filter(|&a| a <= w);
                let a1 = a1.filter(|&a| a0.is_none_or(|a0| a >= a0));
                let a1 = a1.ok_or("垂直模式的位置越界")?;
                cur.push(a1);
                a0 = Some(a1);
                white = !white;
            }
        }
        // [推导] 一行至多 w + 1 个变化元素；再多说明码流在原地打转
        if cur.len() > w + 2 {
            return Err("变化元素过多".into());
        }
    }
    Ok(())
}
