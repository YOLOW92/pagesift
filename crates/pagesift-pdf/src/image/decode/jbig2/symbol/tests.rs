//! 符号字典解码过程的单元测试：用回放预设字段值的来源，检查高度类循环（§6.5.5）、
//! 细化/聚合（§6.5.8.2）和导出（§6.5.10）。

use std::collections::VecDeque;
use std::rc::Rc;

use super::super::bitmap::Bitmap;
use super::super::text::coder::Source;
use super::super::text::tests::{Script, art};
use super::source::{DictField, DictSource};
use super::{Header, header, run};

/// 回放预设值的字典来源：通用区域返回全黑位图并记下尺寸。
#[derive(Default)]
struct Dict {
    ints: VecDeque<Option<i32>>,
    text: Script,
    generic: Vec<(usize, usize)>,
}

impl DictSource for Dict {
    fn int(&mut self, _: DictField) -> Result<Option<i32>, String> {
        self.ints
            .pop_front()
            .ok_or_else(|| "脚本里的整数用完了".into())
    }
    fn generic(&mut self, w: usize, h: usize) -> Result<Bitmap, String> {
        self.generic.push((w, h));
        Bitmap::new(w, h, 1)
    }
    fn collective(&mut self, _: &[usize], _: usize, _: &mut Vec<Rc<Bitmap>>) -> Result<(), String> {
        Err("不该调用".into())
    }
    fn text(&mut self) -> &mut dyn Source {
        &mut self.text
    }
}

fn hdr(refagg: bool, num_new: u32) -> Header {
    Header {
        huff: false,
        refagg,
        sel_dh: 0,
        sel_dw: 0,
        sel_bmsize: 0,
        sel_agginst: 0,
        ctx_used: false,
        ctx_retained: false,
        template: 0,
        rtemplate: 0,
        at: [(0, 0); 4],
        rat: [(0, 0); 2],
        num_new,
    }
}

#[test]
fn height_classes_and_export_runs() {
    let _ = art(&[]); // 设好分配上限
    let mut d = Dict {
        // 高度类 2：宽 1、2；高度类 3：宽 2（每个高度类的宽度从 0 重新累加）；
        // 导出游程 0（不导出）、3（导出）
        ints: [
            Some(2),
            Some(1),
            Some(1),
            None,
            Some(1),
            Some(2),
            None,
            Some(0),
            Some(3),
        ]
        .into(),
        ..Dict::default()
    };
    let out = run(&mut d, &hdr(false, 3), &[]).unwrap();
    assert_eq!(d.generic, vec![(1, 2), (2, 2), (2, 3)]);
    assert_eq!(out.len(), 3);
    assert!(d.ints.is_empty());
}

#[test]
fn export_picks_input_and_new_by_runs() {
    let input = vec![Rc::new(art(&["#"])), Rc::new(art(&["##"]))];
    let mut d = Dict {
        // 一个高度类 1，宽 3、4；导出游程：1 不导出、2 导出、1 不导出
        ints: [Some(1), Some(3), Some(1), None, Some(1), Some(2), Some(1)].into(),
        ..Dict::default()
    };
    let out = run(&mut d, &hdr(false, 2), &input).unwrap();
    let widths: Vec<usize> = out.iter().map(|b| b.w).collect();
    assert_eq!(widths, vec![2, 3]);
}

#[test]
fn export_run_past_total_fails() {
    let _ = art(&[]);
    let mut d = Dict {
        ints: [Some(1), Some(1), None, Some(0), Some(2)].into(),
        ..Dict::default()
    };
    assert!(run(&mut d, &hdr(false, 1), &[]).is_err());
}

#[test]
fn too_many_symbols_in_class_fails() {
    let _ = art(&[]);
    let mut d = Dict {
        ints: [Some(1), Some(1), Some(0)].into(),
        ..Dict::default()
    };
    assert!(run(&mut d, &hdr(false, 1), &[]).is_err());
}

#[test]
fn refinement_one_and_aggregate() {
    let input = vec![Rc::new(art(&["#"]))];
    let mut d = Dict {
        // 高度类 4：宽 3（聚合数 1），宽 2（聚合数 2）；导出游程 1 不导出、2 导出
        ints: [
            Some(4),
            Some(3),
            Some(1),
            Some(-1),
            Some(2),
            None,
            Some(1),
            Some(2),
        ]
        .into(),
        text: Script {
            // 聚合数 1：RDX 1、RDY −1；聚合数 2：DT、DT、DFS、IDS 1、IDS OOB
            ints: [Some(1), Some(-1), Some(0), Some(0), Some(0), Some(1), None].into(),
            // 聚合数 1 的参照符号 0；聚合里的输入符号 0、新符号 0（ID 1）
            ids: [0, 0, 1].into(),
            ris: [false, false].into(),
            ..Script::default()
        },
        ..Dict::default()
    };
    let out = run(&mut d, &hdr(true, 2), &input).unwrap();
    // 聚合数 1 时直接细化：尺寸是符号的，偏移就是 RDX、RDY（表 18）
    assert_eq!(d.text.refined, vec![(3, 4, 1, -1, 1)]);
    assert_eq!(out.len(), 2);
    assert_eq!(*out[0], Bitmap::new(3, 4, 1).unwrap());
    // 聚合数 2：输入符号在 (0,0)，刚解出的新符号 0（全黑 3×4）在 (1,0)，裁到 2×4
    assert_eq!(*out[1], art(&["##", ".#", ".#", ".#"]));
    assert!(d.ints.is_empty() && d.text.ints.is_empty());
}

#[test]
fn header_reads_at_pixels_and_counts() {
    // 标志：SDTEMPLATE 1（位 10）、REFAGG、SDRTEMPLATE 0；1 对 AT；4 字节细化 AT；导出 5、新 7
    let mut data = vec![0x04, 0x02, 0x03, 0xFF, 1, 2, 0xFE, 0xFF];
    data.extend_from_slice(&5u32.to_be_bytes());
    data.extend_from_slice(&7u32.to_be_bytes());
    data.push(0xAA);
    let (h, rest) = header(&data).unwrap();
    assert!(h.refagg && !h.huff);
    assert_eq!(h.template, 1);
    assert_eq!(h.at[0], (3, -1));
    assert_eq!(h.rat, [(1, 2), (-2, -1)]);
    assert_eq!(h.num_new, 7);
    assert_eq!(rest, &[0xAA]);
}
