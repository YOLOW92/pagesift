//! Word 97 的段落属性：PAPX FKP 给出每个段落的样式号和直接属性，样式表（STSH）把样式号映射成内置样式。
//! 这里只要两件事：是不是标题、是不是列表项。其余排版属性不读。

fn u16_at(d: &[u8], i: usize) -> Option<u16> {
    Some(u16::from_le_bytes(d.get(i..i + 2)?.try_into().ok()?))
}

fn u32_at(d: &[u8], i: usize) -> Option<u32> {
    Some(u32::from_le_bytes(d.get(i..i + 4)?.try_into().ok()?))
}

use super::doc_lists::Lists;

const PAGE: usize = 512;
const MAX_RUNS: usize = 2_000_000;
/// 内置样式 Title。
const STI_TITLE: u16 = 62;
const SPRM_ILVL: u16 = 0x260A;
const SPRM_ILFO: u16 = 0x460B;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) enum Kind {
    Heading(u8),
    /// 列表项和层级。
    Item {
        level: u8,
        ordered: bool,
        start: u32,
        /// 同一个列表（lsid）的编号连续；查不到列表定义时是 ilfo。
        list: u32,
    },
}

#[derive(Debug)]
struct Run {
    start: u32,
    end: u32,
    istd: u16,
    list: Option<(u16, u8)>,
}

/// 按字节位置查段落类型。
#[derive(Debug, Default)]
pub(super) struct Styles {
    runs: Vec<Run>,
    headings: Vec<Option<u8>>,
    lists: Lists,
}

impl Styles {
    /// `plc`、`stsh` 是表流里的 PlcBtePapx 和样式表。读不出来就返回空表（都当普通段落）。
    pub(super) fn read(word: &[u8], plc: &[u8], stsh: &[u8], lists: Lists) -> Styles {
        Styles {
            runs: runs(word, plc),
            headings: headings(stsh),
            lists,
        }
    }

    pub(super) fn kind_at(&self, fc: u32) -> Option<Kind> {
        let i = self.runs.partition_point(|r| r.end <= fc);
        let run = self.runs.get(i).filter(|r| r.start <= fc)?;
        if let Some(level) = self.headings.get(usize::from(run.istd)).copied().flatten() {
            return Some(Kind::Heading(level));
        }
        let (ilfo, level) = run.list?;
        Some(match self.lists.level(ilfo, level) {
            Some((list, l)) => Kind::Item {
                level,
                ordered: l.ordered,
                start: l.start,
                list,
            },
            None => Kind::Item {
                level,
                ordered: false,
                start: 1,
                list: u32::from(ilfo),
            },
        })
    }
}

fn runs(word: &[u8], plc: &[u8]) -> Vec<Run> {
    let mut out = Vec::new();
    if plc.len() < 4 {
        return out;
    }
    let n = (plc.len() - 4) / 8;
    for i in 0..n {
        let Some(pn) = u32_at(plc, (n + 1) * 4 + i * 4) else {
            break;
        };
        let at = (pn & 0x3F_FFFF) as usize * PAGE;
        let Some(page) = word.get(at..at + PAGE) else {
            continue;
        };
        let count = usize::from(page[PAGE - 1]);
        for k in 0..count {
            let (Some(start), Some(end)) = (u32_at(page, k * 4), u32_at(page, (k + 1) * 4)) else {
                break;
            };
            // rgbx 紧跟在 count+1 个 FC 后面，每项 13 字节，第一个字节是 PapxInFkp 的字偏移
            let Some(&bx) = page.get((count + 1) * 4 + k * 13) else {
                break;
            };
            if bx == 0 {
                continue;
            }
            let o = usize::from(bx) * 2;
            let Some(&cb) = page.get(o) else { continue };
            let (body, len) = if cb == 0 {
                (
                    o + 2,
                    usize::from(page.get(o + 1).copied().unwrap_or(0)) * 2,
                )
            } else {
                (o + 1, usize::from(cb) * 2 - 1)
            };
            let Some(grpprl) = page.get(body..(body + len).min(PAGE)) else {
                continue;
            };
            let Some(istd) = u16_at(grpprl, 0) else {
                continue;
            };
            out.push(Run {
                start,
                end,
                istd,
                list: list_level(grpprl.get(2..).unwrap_or(&[])),
            });
            if out.len() >= MAX_RUNS {
                return sorted(out);
            }
        }
    }
    sorted(out)
}

fn sorted(mut runs: Vec<Run>) -> Vec<Run> {
    runs.sort_by_key(|r| r.start);
    runs
}

/// 走一遍 sprm：操作数长度由高 3 位决定（MS-DOC §2.6.1）。有 ilfo 且不为 0 就是列表项。
fn list_level(mut p: &[u8]) -> Option<(u16, u8)> {
    let (mut ilfo, mut level) = (0u16, 0u8);
    while p.len() >= 2 {
        let sprm = u16::from_le_bytes([p[0], p[1]]);
        p = &p[2..];
        let len = match sprm >> 13 {
            0 | 1 => 1,
            2 | 4 | 5 => 2,
            3 => 4,
            7 => 3,
            _ => {
                let n = usize::from(*p.first()?);
                // sprmPHugePapx 这类变长操作数之后不再读
                1 + n
            }
        };
        let arg = p.get(..len)?;
        match sprm {
            SPRM_ILFO => ilfo = u16::from_le_bytes([arg[0], arg[1]]),
            SPRM_ILVL => level = arg[0],
            _ => {}
        }
        p = &p[len..];
    }
    (ilfo != 0 && ilfo != 0xF801).then_some((ilfo, level.min(8)))
}

/// 每个样式号是不是标题：内置样式 Heading 1–9 和 Title，自定义样式跟着基样式走。
fn headings(stsh: &[u8]) -> Vec<Option<u8>> {
    let Some(cb) = u16_at(stsh, 0).map(usize::from) else {
        return Vec::new();
    };
    let Some(count) = u16_at(stsh, 2).map(usize::from) else {
        return Vec::new();
    };
    let mut info: Vec<(u16, u16)> = Vec::with_capacity(count);
    let mut p = 2 + cb;
    for _ in 0..count {
        let Some(len) = u16_at(stsh, p).map(usize::from) else {
            break;
        };
        if len >= 4 {
            let word0 = u16_at(stsh, p + 2).unwrap_or(0);
            let word1 = u16_at(stsh, p + 4).unwrap_or(0);
            info.push((word0 & 0xFFF, word1 >> 4));
        } else {
            info.push((0xFFF, 0xFFF));
        }
        p += 2 + len;
    }
    (0..info.len())
        .map(|i| {
            let mut cur = i;
            for _ in 0..8 {
                let (sti, base) = info[cur];
                match sti {
                    1..=9 => return Some(sti as u8),
                    STI_TITLE => return Some(1),
                    // 0 是 Normal；其他内置样式不是标题
                    0xFFE | 0xFFF => {}
                    _ => return None,
                }
                if usize::from(base) >= info.len() {
                    return None;
                }
                cur = usize::from(base);
            }
            None
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn stsh() -> Vec<u8> {
        let mut out = vec![];
        out.extend_from_slice(&4u16.to_le_bytes()); // cbStshi
        out.extend_from_slice(&4u16.to_le_bytes()); // cstd
        out.extend_from_slice(&10u16.to_le_bytes());
        // Normal、Heading 2、自定义样式（基于 Heading 2）、Title
        for (sti, base) in [(0u16, 0xFFFu16), (2, 0), (0xFFE, 1), (STI_TITLE, 0)] {
            out.extend_from_slice(&10u16.to_le_bytes());
            out.extend_from_slice(&sti.to_le_bytes());
            out.extend_from_slice(&(base << 4).to_le_bytes());
            out.extend_from_slice(&[0; 6]);
        }
        out
    }

    #[test]
    fn style_table_maps_headings() {
        assert_eq!(headings(&stsh()), vec![None, Some(2), Some(2), Some(1)]);
        assert_eq!(headings(&[1]), Vec::<Option<u8>>::new());
    }

    #[test]
    fn fkp_paragraphs_and_lists() {
        let mut word = vec![0u8; PAGE * 2];
        let page = &mut word[PAGE..PAGE * 2];
        // 两个段落：[100,110) 样式 1，[110,120) 样式 0 + 列表
        for (i, fc) in [100u32, 110, 120].iter().enumerate() {
            page[i * 4..i * 4 + 4].copy_from_slice(&fc.to_le_bytes());
        }
        page[PAGE - 1] = 2;
        page[12] = 100;
        page[12 + 13] = 110;
        // PapxInFkp：cb、istd
        page[200] = 2;
        page[201..203].copy_from_slice(&1u16.to_le_bytes());
        let sprms = [
            0x0Bu8, 0x46, 3, 0, // ilfo = 3
            0x0A, 0x26, 2, // ilvl = 2
        ];
        page[220] = 5;
        page[221..223].copy_from_slice(&0u16.to_le_bytes());
        page[223..223 + sprms.len()].copy_from_slice(&sprms);
        let mut plc = Vec::new();
        for v in [100u32, 120, 1] {
            plc.extend_from_slice(&v.to_le_bytes());
        }
        let s = Styles::read(&word, &plc, &stsh(), Lists::default());
        assert_eq!(s.kind_at(105), Some(Kind::Heading(2)));
        assert_eq!(
            s.kind_at(115),
            Some(Kind::Item {
                level: 2,
                ordered: false,
                start: 1,
                list: 3
            })
        );
        assert_eq!(s.kind_at(99), None);
        assert_eq!(s.kind_at(120), None);
    }

    #[test]
    fn broken_tables_give_nothing() {
        let s = Styles::read(
            &[],
            &[1, 2, 3, 4, 5, 6, 7, 8, 9, 10, 11, 12],
            &[],
            Lists::default(),
        );
        assert_eq!(s.kind_at(0), None);
        assert_eq!(list_level(&[0x0B, 0x46]), None);
        assert_eq!(list_level(&[0x0B, 0xC6, 200, 1]), None);
    }
}
