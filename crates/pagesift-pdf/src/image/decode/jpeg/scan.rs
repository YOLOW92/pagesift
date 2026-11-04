//! 一个扫描的熵解码，把系数写进各分量的系数缓冲区。顺序方式（T.81 §F.2）和渐进方式（§G.1.2）
//! 共用一套缓冲区：顺序方式就是一次覆盖全部 64 个系数、不做逐次逼近的扫描。

use super::huffman::{Bits, Huffman};
use super::marker::{COEFS, Frame, Scan, ZIGZAG};

/// 一个分量的系数，按块存放（自然顺序）；块数按 MCU 补齐。
pub(super) struct Plane {
    pub(super) blocks_w: usize,
    pub(super) blocks_h: usize,
    pub(super) coefs: Vec<i16>,
}

impl Plane {
    pub(super) fn new(frame: &Frame, comp: usize) -> Plane {
        let (mx, my) = frame.mcus();
        let c = &frame.comps[comp];
        let (blocks_w, blocks_h) = (mx * c.h, my * c.v);
        Plane {
            blocks_w,
            blocks_h,
            coefs: vec![0; blocks_w * blocks_h * COEFS],
        }
    }

    fn block(&mut self, row: usize, col: usize) -> &mut [i16] {
        let i = (row * self.blocks_w + col) * COEFS;
        &mut self.coefs[i..i + COEFS]
    }
}

/// 解一个扫描。返回熵编码数据之后的位置（下一个标记的 FF）。
/// 扫描要用到的表：DC、AC 霍夫曼表各 4 张，和重启间隔（以 MCU 计，0 表示没有）。
pub(super) struct Tables<'a> {
    /// [协议] 霍夫曼表号 0–3（T.81 §B.2.4.2）
    pub(super) dc: &'a [Option<Huffman>; 4],
    pub(super) ac: &'a [Option<Huffman>; 4],
    pub(super) restart: usize,
}

pub(super) fn decode(
    data: &[u8],
    pos: usize,
    frame: &Frame,
    scan: &Scan,
    t: &Tables,
    planes: &mut [Plane],
) -> Result<usize, String> {
    let (dc, ac, restart) = (t.dc, t.ac, t.restart);
    let mut bits = Bits::new(data, pos);
    let mut pred = [0i32; 4]; // [协议] 一个扫描至多 4 个分量（T.81 §B.2.3），各一个 DC 预测值
    let mut eobrun = 0u32;
    let mode = Mode::of(frame, scan)?;
    // [协议] 霍夫曼表号 0–3（T.81 §B.2.4.2）
    fn table<'t>(
        set: &'t [Option<Huffman>; 4],
        i: usize,
        what: &str,
    ) -> Result<&'t Huffman, String> {
        set.get(i)
            .and_then(|t| t.as_ref())
            .ok_or_else(|| format!("扫描用到了未定义的 {what} 霍夫曼表 {i}"))
    }
    let tables = scan
        .comps
        .iter()
        .map(|c| {
            let need_dc = scan.ss == 0 && scan.ah == 0;
            let need_ac = scan.se > 0;
            Ok((
                if need_dc {
                    Some(table(dc, c.td, "DC")?)
                } else {
                    None
                },
                if need_ac {
                    Some(table(ac, c.ta, "AC")?)
                } else {
                    None
                },
            ))
        })
        .collect::<Result<Vec<_>, String>>()?;

    // 交织扫描按 MCU 走；只有一个分量的扫描按该分量自己的块走（T.81 §A.2.2、§A.2.3）
    let units: Vec<(usize, usize)> = if scan.comps.len() == 1 {
        let c = &frame.comps[scan.comps[0].index];
        let w = (frame.width * c.h)
            .div_ceil(frame.hmax())
            .div_ceil(super::marker::BLOCK);
        let h = (frame.height * c.v)
            .div_ceil(frame.vmax())
            .div_ceil(super::marker::BLOCK);
        (0..h)
            .flat_map(|r| (0..w).map(move |col| (r, col)))
            .collect()
    } else {
        let (mx, my) = frame.mcus();
        (0..my)
            .flat_map(|r| (0..mx).map(move |col| (r, col)))
            .collect()
    };
    for (n, &(row, col)) in units.iter().enumerate() {
        // 按行查时限（单个块的开销和查一次差不多）
        if col == 0 && crate::budget::expired() {
            return Err("解码超过了时限".into());
        }
        if restart > 0 && n > 0 && n % restart == 0 {
            bits.restart();
            pred = [0; 4]; // [协议] 4 个分量的 DC 预测在重启处清零（T.81 附录 F）
            eobrun = 0;
        }
        if scan.comps.len() == 1 {
            let sc = scan.comps[0];
            let block = planes[sc.index].block(row, col);
            unit(
                &mut bits,
                mode,
                scan,
                tables[0],
                &mut pred[0],
                &mut eobrun,
                block,
            )?;
        } else {
            for (k, sc) in scan.comps.iter().enumerate() {
                let fc = &frame.comps[sc.index];
                for v in 0..fc.v {
                    for h in 0..fc.h {
                        let block = planes[sc.index].block(row * fc.v + v, col * fc.h + h);
                        unit(
                            &mut bits,
                            mode,
                            scan,
                            tables[k],
                            &mut pred[k],
                            &mut eobrun,
                            block,
                        )?;
                    }
                }
            }
        }
    }
    // 读位器从不越过标记的 FF（见 `Bits::fill`），从它停下的位置往后找下一个段标记即可
    Ok(next_marker(data, bits.pos))
}

/// 从 `pos` 起找下一个不是 RST、不是填充的标记。
fn next_marker(data: &[u8], pos: usize) -> usize {
    let mut i = pos.min(data.len());
    // 读位器停在标记的 FF 上，或者已越过最后一字节
    while i + 1 < data.len() {
        // [协议] FF 00 是填充、FFD0–FFD7 是 RST，都不是段标记
        if data[i] == 0xFF
            && data[i + 1] != 0x00
            && data[i + 1] != 0xFF
            && !(0xD0..=0xD7).contains(&data[i + 1])
        {
            return i;
        }
        i += 1;
    }
    data.len()
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum Mode {
    /// 顺序方式：DC 和全部 AC 一次给出。
    Sequential,
    DcFirst,
    DcRefine,
    AcFirst,
    AcRefine,
}

impl Mode {
    fn of(frame: &Frame, scan: &Scan) -> Result<Mode, String> {
        if !frame.progressive {
            return Ok(Mode::Sequential);
        }
        // T.81 §G.1.1.1.1：DC 扫描只含 DC；AC 扫描只含一个分量
        Ok(match (scan.ss == 0, scan.ah == 0) {
            (true, _) if scan.se != 0 => return Err("渐进方式的 DC 扫描不能带 AC 系数".into()),
            (false, _) if scan.comps.len() != 1 => {
                return Err("渐进方式的 AC 扫描只能有一个分量".into());
            }
            (true, true) => Mode::DcFirst,
            (true, false) => Mode::DcRefine,
            (false, true) => Mode::AcFirst,
            (false, false) => Mode::AcRefine,
        })
    }
}

/// 解一个块（数据单元）。
fn unit(
    bits: &mut Bits,
    mode: Mode,
    scan: &Scan,
    (dc, ac): (Option<&Huffman>, Option<&Huffman>),
    pred: &mut i32,
    eobrun: &mut u32,
    block: &mut [i16],
) -> Result<(), String> {
    match mode {
        Mode::Sequential => {
            let t = bits.decode(dc.ok_or("缺 DC 表")?)?;
            *pred += bits.receive_extend(u32::from(t));
            block[0] = *pred as i16;
            let ac = ac.ok_or("缺 AC 表")?;
            let mut k = 1;
            while k < COEFS {
                let rs = bits.decode(ac)?;
                let (r, s) = (usize::from(rs >> 4), u32::from(rs & 0x0F)); // [协议] RRRR/SSSS 各 4 位（T.81 §F.1.2.2）
                if s == 0 {
                    // [协议] SSSS = 0 时 RRRR = 15 是 ZRL，其余是 EOB（T.81 §F.1.2.2）
                    if r != 15 {
                        break; // EOB
                    }
                    k += 16; // [协议] ZRL：16 个零（T.81 §F.1.2.2）
                    continue;
                }
                k += r;
                if k >= COEFS {
                    return Err("AC 系数越过了块尾".into());
                }
                block[ZIGZAG[k]] = bits.receive_extend(s) as i16;
                k += 1;
            }
        }
        Mode::DcFirst => {
            let t = bits.decode(dc.ok_or("缺 DC 表")?)?;
            *pred += bits.receive_extend(u32::from(t));
            block[0] = (*pred << scan.al) as i16;
        }
        Mode::DcRefine => {
            if bits.bit() {
                block[0] |= (1 << scan.al) as i16;
            }
        }
        Mode::AcFirst => ac_first(bits, ac.ok_or("缺 AC 表")?, scan, eobrun, block)?,
        Mode::AcRefine => ac_refine(bits, ac.ok_or("缺 AC 表")?, scan, eobrun, block)?,
    }
    Ok(())
}

/// 渐进方式 AC 首次扫描（T.81 §G.1.2.2）。
fn ac_first(
    bits: &mut Bits,
    ac: &Huffman,
    scan: &Scan,
    eobrun: &mut u32,
    block: &mut [i16],
) -> Result<(), String> {
    if *eobrun > 0 {
        *eobrun -= 1;
        return Ok(());
    }
    let mut k = scan.ss;
    while k <= scan.se {
        let rs = bits.decode(ac)?;
        let (r, s) = (u32::from(rs >> 4), u32::from(rs & 0x0F)); // [协议] RRRR/SSSS 各 4 位
        if s == 0 {
            // [协议] RRRR < 15 是 EOBn，RRRR = 15 是 ZRL（T.81 §G.1.2.2）
            if r < 15 {
                // EOBn：本块和之后 2^r − 1 + 附加位 个块在本频带内都没有系数（T.81 §G.1.2.2）
                *eobrun = (1 << r) - 1 + bits.bits(r);
                break;
            }
            k += 16; // [协议] ZRL：16 个零
            continue;
        }
        k += r as usize;
        if k > scan.se {
            return Err("AC 系数越过了频带".into());
        }
        block[ZIGZAG[k]] = (bits.receive_extend(s) << scan.al) as i16;
        k += 1;
    }
    Ok(())
}

/// 渐进方式 AC 逐次逼近扫描（T.81 §G.1.2.3）：已非零的系数各收一个修正位，
/// 新出现的系数幅度是 ±2^Al，零游程只数仍为零的位置。
fn ac_refine(
    bits: &mut Bits,
    ac: &Huffman,
    scan: &Scan,
    eobrun: &mut u32,
    block: &mut [i16],
) -> Result<(), String> {
    let p1 = 1i16 << scan.al;
    let m1 = -1i16 << scan.al;
    let mut k = scan.ss;
    let correct = |bits: &mut Bits, c: &mut i16| {
        if bits.bit() && (*c & p1) == 0 {
            *c += if *c >= 0 { p1 } else { m1 };
        }
    };
    if *eobrun == 0 {
        while k <= scan.se {
            let rs = bits.decode(ac)?;
            let (mut r, s) = (i32::from(rs >> 4), rs & 0x0F); // [协议] RRRR/SSSS 各 4 位
            let mut value = 0i16;
            if s != 0 {
                // 逐次逼近扫描里新系数的 SSSS 只能是 1（T.81 §G.1.2.3）
                value = if bits.bit() { p1 } else { m1 };
                // [协议] 下一分支：SSSS = 0 时 RRRR = 15 是 ZRL，其余是 EOBn（T.81 §G.1.2.3）
            } else if r != 15 {
                *eobrun = (1 << r) + bits.bits(r as u32);
                break;
            }
            // 跳过 r 个仍为零的位置（途中经过的非零系数各收修正位），停在要放新系数的位置
            while k <= scan.se {
                let z = ZIGZAG[k];
                if block[z] != 0 {
                    correct(bits, &mut block[z]);
                } else {
                    if r == 0 {
                        break;
                    }
                    r -= 1;
                }
                k += 1;
            }
            if value != 0 {
                if k > scan.se {
                    return Err("AC 修正越过了频带".into());
                }
                block[ZIGZAG[k]] = value;
            }
            k += 1;
        }
    }
    if *eobrun > 0 {
        // 频带内剩下的非零系数收修正位
        while k <= scan.se {
            let z = ZIGZAG[k];
            if block[z] != 0 {
                correct(bits, &mut block[z]);
            }
            k += 1;
        }
        *eobrun -= 1;
    }
    Ok(())
}
