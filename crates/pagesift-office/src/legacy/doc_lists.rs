//! Word 97 的列表定义：PlfLfo（段落里的 ilfo 指向的列表实例）和 PlfLst（列表模板，每级的编号格式）。
//! 只读一件事：某个 ilfo 的某一级是不是有序列表、从几开始。输入不可信，所有长度都按剩余字节数限制。

use std::collections::HashMap;

fn u16_at(d: &[u8], i: usize) -> Option<u16> {
    Some(u16::from_le_bytes(d.get(i..i + 2)?.try_into().ok()?))
}

fn u32_at(d: &[u8], i: usize) -> Option<u32> {
    Some(u32::from_le_bytes(d.get(i..i + 4)?.try_into().ok()?))
}

const LSTF_LEN: usize = 28;
const LVLF_LEN: usize = 28;
const LFO_LEN: usize = 16;
/// 编号格式 23 是项目符号，255 是无编号。
const NFC_BULLET: u8 = 23;
const NFC_NONE: u8 = 255;

/// 一级的编号：有序与否、起始号。
#[derive(Debug, Clone, Copy)]
pub(super) struct Level {
    pub ordered: bool,
    pub start: u32,
}

#[derive(Debug, Default)]
pub(super) struct Lists {
    /// 下标是 ilfo - 1，值是 lsid。
    lfo: Vec<u32>,
    lst: HashMap<u32, Vec<Level>>,
}

impl Lists {
    pub(super) fn read(plf_lst: &[u8], plf_lfo: &[u8]) -> Lists {
        let mut lists = Lists::default();
        let n = u32_at(plf_lfo, 0).unwrap_or(0) as usize;
        let n = n.min(plf_lfo.len().saturating_sub(4) / LFO_LEN);
        lists.lfo = (0..n)
            .filter_map(|i| u32_at(plf_lfo, 4 + i * LFO_LEN))
            .collect();
        lists.read_templates(plf_lst);
        lists
    }

    fn read_templates(&mut self, d: &[u8]) {
        let Some(count) = u16_at(d, 0) else { return };
        let count = usize::from(count).min(d.len().saturating_sub(2) / LSTF_LEN);
        let mut at = 2 + count * LSTF_LEN;
        for i in 0..count {
            let head = 2 + i * LSTF_LEN;
            let (Some(lsid), Some(flags)) = (u32_at(d, head), d.get(head + 26)) else {
                return;
            };
            let levels = if flags & 1 != 0 { 1 } else { 9 };
            let mut out = Vec::with_capacity(levels);
            for _ in 0..levels {
                let Some((level, next)) = level_at(d, at) else {
                    return;
                };
                out.push(level);
                at = next;
            }
            self.lst.insert(lsid, out);
        }
    }

    /// 列表实例的 lsid（同一个 lsid 的编号是连续的）和这一级的编号。查不到就是 None。
    pub(super) fn level(&self, ilfo: u16, ilvl: u8) -> Option<(u32, Level)> {
        let lsid = *self.lfo.get(usize::from(ilfo).checked_sub(1)?)?;
        let levels = self.lst.get(&lsid)?;
        let level = levels.get(usize::from(ilvl)).or(levels.first())?;
        Some((lsid, *level))
    }
}

/// 读一个 LVL，返回它和下一个 LVL 的位置。
fn level_at(d: &[u8], at: usize) -> Option<(Level, usize)> {
    let head = d.get(at..at.checked_add(LVLF_LEN)?)?;
    let start = i32::from_le_bytes(head[0..4].try_into().ok()?);
    let nfc = head[4];
    let (chpx, papx) = (usize::from(head[24]), usize::from(head[25]));
    let xst = at + LVLF_LEN + papx + chpx;
    let cch = usize::from(u16_at(d, xst)?);
    let next = xst.checked_add(2 + cch * 2)?;
    if next > d.len() {
        return None;
    }
    let level = Level {
        ordered: nfc != NFC_BULLET && nfc != NFC_NONE,
        start: start.max(0) as u32,
    };
    Some((level, next))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn lstf(lsid: u32) -> Vec<u8> {
        let mut v = vec![0u8; LSTF_LEN];
        v[..4].copy_from_slice(&lsid.to_le_bytes());
        v
    }

    fn lvl(nfc: u8, start: i32) -> Vec<u8> {
        let mut v = vec![0u8; LVLF_LEN + 2];
        v[..4].copy_from_slice(&start.to_le_bytes());
        v[4] = nfc;
        v
    }

    #[test]
    fn reads_ordered_and_bullet_levels() {
        let mut lst = vec![2, 0];
        lst.extend(lstf(77));
        lst.extend(lstf(88));
        for _ in 0..9 {
            lst.extend(lvl(0, 3));
        }
        for _ in 0..9 {
            lst.extend(lvl(NFC_BULLET, 1));
        }
        let mut lfo = 2u32.to_le_bytes().to_vec();
        for id in [88u32, 77] {
            let mut e = vec![0u8; LFO_LEN];
            e[..4].copy_from_slice(&id.to_le_bytes());
            lfo.extend(e);
        }
        let l = Lists::read(&lst, &lfo);
        let (id, a) = l.level(1, 0).unwrap();
        assert!(!a.ordered && id == 88);
        let (id, b) = l.level(2, 4).unwrap();
        assert!(b.ordered && b.start == 3 && id == 77);
        assert!(l.level(3, 0).is_none() && l.level(0, 0).is_none());
    }

    #[test]
    fn garbage_gives_nothing() {
        assert!(Lists::read(&[9, 9], &[1, 0, 0]).level(1, 0).is_none());
        assert!(Lists::read(&[0xFF; 40], &[0xFF; 40]).level(1, 0).is_none());
    }
}
