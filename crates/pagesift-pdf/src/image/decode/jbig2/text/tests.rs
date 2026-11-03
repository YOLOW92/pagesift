//! 文本区域解码过程的单元测试：用回放预设字段值的来源，检查放置规则（§6.4.5）、细化参数
//! （§6.4.11.1）和读位器。

use std::collections::VecDeque;
use std::rc::Rc;

use super::super::bitmap::{self, Bitmap, Op};
use super::super::refine;
use super::coder::{Field, Reader, Source, code_len};
use super::{Corner, Params, Symbols, decode, ds_offset};

/// 按顺序回放预设值的来源；细化时返回全黑位图并记下参数。
#[derive(Default)]
pub(in crate::image::decode::jbig2) struct Script {
    pub(in crate::image::decode::jbig2) ints: VecDeque<Option<i32>>,
    pub(in crate::image::decode::jbig2) ids: VecDeque<usize>,
    pub(in crate::image::decode::jbig2) curts: VecDeque<i32>,
    pub(in crate::image::decode::jbig2) ris: VecDeque<bool>,
    /// 细化调用的 (宽, 高, dx, dy, 参照图宽)。
    pub(in crate::image::decode::jbig2) refined: Vec<(usize, usize, i32, i32, usize)>,
}

impl Source for Script {
    fn int(&mut self, _: Field) -> Result<Option<i32>, String> {
        self.ints
            .pop_front()
            .ok_or_else(|| "脚本里的整数用完了".into())
    }
    fn curt(&mut self, _: u32) -> Result<i32, String> {
        self.curts
            .pop_front()
            .ok_or_else(|| "脚本里的 CURT 用完了".into())
    }
    fn id(&mut self) -> Result<usize, String> {
        self.ids
            .pop_front()
            .ok_or_else(|| "脚本里的 ID 用完了".into())
    }
    fn ri(&mut self) -> Result<bool, String> {
        self.ris
            .pop_front()
            .ok_or_else(|| "脚本里的 RI 用完了".into())
    }
    fn refine(&mut self, p: &refine::Params) -> Result<Bitmap, String> {
        self.refined.push((p.w, p.h, p.dx, p.dy, p.reference.w));
        Bitmap::new(p.w, p.h, 1)
    }
}

/// 由字符画建位图：`#` 是黑。
pub(in crate::image::decode::jbig2) fn art(rows: &[&str]) -> Bitmap {
    bitmap::set_budget(1 << 24);
    let (w, h) = (rows.first().map_or(0, |r| r.len()), rows.len());
    let mut b = Bitmap::new(w, h, 0).unwrap();
    for (y, r) in rows.iter().enumerate() {
        for (x, c) in r.bytes().enumerate() {
            b.set(x, y, u8::from(c == b'#'));
        }
    }
    b
}

fn params(w: usize, h: usize, instances: u32, corner: Corner, transposed: bool) -> Params {
    Params {
        w,
        h,
        instances,
        log_strips: 0,
        corner,
        transposed,
        op: Op::Or,
        default_pixel: 0,
        ds_offset: 0,
        refine: false,
        rtemplate: 0,
        rat: [(0, 0); 2],
    }
}

fn two_symbols() -> Vec<Rc<Bitmap>> {
    vec![
        Rc::new(art(&["###", "#.."])), // 3×2
        Rc::new(art(&["##", ".#"])),   // 2×2
    ]
}

#[test]
fn places_top_left_left_to_right() {
    let syms = two_symbols();
    let mut s = Script {
        // 初始 DT 0；DT 1 → STRIPT 1；DFS 2；IDS 1；IDS OOB
        ints: [Some(0), Some(1), Some(2), Some(1), None].into(),
        ids: [0, 1].into(),
        ..Script::default()
    };
    let p = params(8, 4, 2, Corner::TopLeft, false);
    let sy = Symbols {
        head: &syms,
        tail: &[],
    };
    let out = decode(&mut s, &p, &sy).unwrap();
    // A 在 (2,1)；CURS = 2 + 3 − 1 = 4，加 IDS 1 → 5；B 在 (5,1)
    let want = art(&["........", "..#####.", "..#...#.", "........"]);
    assert_eq!(out, want);
    assert!(s.ints.is_empty());
}

#[test]
fn places_bottom_right_transposed_with_strips() {
    let syms = two_symbols();
    let mut s = Script {
        // 初始 DT 0；DT 2（× SBSTRIPS 2 → STRIPT 4）；DFS 1；IDS 3；IDS OOB
        ints: [Some(0), Some(2), Some(1), Some(3), None].into(),
        ids: [0, 1].into(),
        curts: [1, 0].into(),
        ..Script::default()
    };
    let mut p = params(8, 8, 2, Corner::BottomRight, true);
    p.log_strips = 1;
    p.ds_offset = -1;
    let sy = Symbols {
        head: &syms,
        tail: &[],
    };
    let out = decode(&mut s, &p, &sy).unwrap();
    // 第一个：T = 5，CURS = 1 + (2 − 1) = 2，右下角在 (5, 2) → 左上 (3, 1)
    // 第二个：CURS = 2 + 3 − 1 = 4，再加 2 − 1 → 5；T = 4，右下角在 (4, 5) → 左上 (3, 4)
    let want = art(&[
        "........", "...###..", "...#....", "........", "...##...", "....#...", "........",
        "........",
    ]);
    assert_eq!(out, want);
}

#[test]
fn bottom_left_combines_with_xor_on_black_background() {
    let syms = two_symbols();
    let mut s = Script {
        ints: [Some(0), Some(2), Some(0), None].into(),
        ids: [1].into(),
        ..Script::default()
    };
    let mut p = params(3, 3, 1, Corner::BottomLeft, false);
    p.op = Op::Xor;
    p.default_pixel = 1;
    let sy = Symbols {
        head: &syms,
        tail: &[],
    };
    // 左下角在 (0, 2) → 左上 (0, 1)
    let out = decode(&mut s, &p, &sy).unwrap();
    assert_eq!(out, art(&["###", "..#", "#.#"]));
}

#[test]
fn stops_when_instances_are_used_up() {
    let syms = two_symbols();
    // 实例数 1，但第一个之后的 IDS 不是 OOB：就此停下，不再读
    let mut s = Script {
        ints: [Some(0), Some(0), Some(0), Some(5)].into(),
        ids: [0].into(),
        ..Script::default()
    };
    let p = params(4, 2, 1, Corner::TopLeft, false);
    let sy = Symbols {
        head: &syms,
        tail: &[],
    };
    assert_eq!(decode(&mut s, &p, &sy).unwrap(), art(&["###.", "#..."]));
}

#[test]
fn refinement_offsets_use_floor_of_half_delta() {
    let syms = vec![Rc::new(art(&["####", "####", "####", "####"]))];
    let mut s = Script {
        // DT、DT、DFS，RDW −3、RDH 2、RDX 1、RDY −1，IDS OOB
        ints: [
            Some(0),
            Some(0),
            Some(0),
            Some(-3),
            Some(2),
            Some(1),
            Some(-1),
            None,
        ]
        .into(),
        ids: [0].into(),
        ris: [true].into(),
        ..Script::default()
    };
    let mut p = params(4, 8, 1, Corner::TopLeft, false);
    p.refine = true;
    let sy = Symbols {
        head: &syms,
        tail: &[],
    };
    decode(&mut s, &p, &sy).unwrap();
    // 宽 4 − 3 = 1，高 6；dx = ⌊−3/2⌋ + 1 = −1，dy = ⌊2/2⌋ − 1 = 0
    assert_eq!(s.refined, vec![(1, 6, -1, 0, 4)]);
}

#[test]
fn symbol_ids_span_both_halves() {
    let head = two_symbols();
    let tail = vec![Rc::new(art(&["#"]))];
    let sy = Symbols {
        head: &head,
        tail: &tail,
    };
    assert_eq!(sy.get(2).unwrap().w, 1);
    assert!(sy.get(3).is_err());
}

#[test]
fn ds_offset_is_five_bit_signed() {
    assert_eq!(ds_offset(0x0F << 10), 15);
    assert_eq!(ds_offset(0x10 << 10), -16);
    assert_eq!(ds_offset(0x1F << 10), -1);
}

#[test]
fn code_len_is_ceil_log2() {
    let got: Vec<u32> = [0, 1, 2, 3, 4, 5, 256, 257].map(code_len).to_vec();
    assert_eq!(got, vec![0, 0, 1, 2, 2, 3, 8, 9]);
}

#[test]
fn reader_takes_aligned_bytes_and_continues() {
    let data = [0xA5, 0xFF, 0x01, 0x02];
    let mut r = Reader::new(&data);
    assert_eq!(r.bits(3).unwrap(), 0b101);
    assert_eq!(r.take(2), &[0xFF, 0x01]);
    assert_eq!(r.bits(8).unwrap(), 0x02);
    // 读过末尾：取到的为空
    assert_eq!(r.take(5), &[] as &[u8]);
}
