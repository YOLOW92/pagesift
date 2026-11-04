//! JBIG2 解码的单元测试。

use super::mq::{IntDecoder, Mq, contexts};

/// T.88 附录 H.2 的 MQ 编码测试序列：编码后的 30 字节解出来应是给定的 256 位
/// （与 JPEG 2000 T.800 附录 J.3 相同）。
#[test]
fn mq_conformance() {
    // [协议] T.88 表 H.1 的编码结果（ITU-T 公布的测试数据）
    let coded: [u8; 30] = [
        0x84, 0xC7, 0x3B, 0xFC, 0xE1, 0xA1, 0x43, 0x04, 0x02, 0x20, 0x00, 0x00, 0x41, 0x0D, 0xBB,
        0x86, 0xF4, 0x31, 0x7F, 0xFF, 0x88, 0xFF, 0x37, 0x47, 0x1A, 0xDB, 0x6A, 0xDF, 0xFF, 0xAC,
    ];
    // [协议] 原始数据 32 字节
    let plain: [u8; 32] = [
        0x00, 0x02, 0x00, 0x51, 0x00, 0x00, 0x00, 0xC0, 0x03, 0x52, 0x87, 0x2A, 0xAA, 0xAA, 0xAA,
        0xAA, 0x82, 0xC0, 0x20, 0x00, 0xFC, 0xD7, 0x9E, 0xF6, 0xBF, 0x7F, 0xED, 0x90, 0x4F, 0x46,
        0xA3, 0xBF,
    ];
    let mut mq = Mq::new(&coded);
    let mut cx = contexts(1);
    for (i, &byte) in plain.iter().enumerate() {
        let mut got = 0u8;
        for _ in 0..8 {
            got = (got << 1) | mq.decode(&mut cx[0]);
        }
        assert_eq!(got, byte, "第 {i} 字节");
    }
    let _ = IntDecoder::new();
}

/// 滑动分组算出的上下文与规范图 3–6 的逐像素定义一致（随机位图、随机 AT）。
#[test]
fn generic_context_groups_match_templates() {
    use super::bitmap::{Bitmap, set_budget};
    use super::generic::{context_fast, context_slow, default_at};
    set_budget(1 << 20);
    let (w, h) = (23usize, 9usize);
    let mut b = Bitmap::new(w, h, 0).unwrap();
    let mut seed = 0x2545_F491u32;
    let mut rnd = || {
        // [策略] xorshift32，只为造确定的测试数据
        seed ^= seed << 13;
        seed ^= seed >> 17;
        seed ^= seed << 5;
        seed
    };
    for v in &mut b.data {
        *v = (rnd() & 1) as u8;
    }
    for t in 0..4 {
        let mut ats = vec![default_at(t as u8)];
        for _ in 0..8 {
            let mut at = [(0i16, 0i16); 4];
            for a in &mut at {
                // [策略] AT 偏移取 −8…7、行 −8…0，覆盖图外取 0
                *a = ((rnd() % 16) as i16 - 8, -((rnd() % 9) as i16));
            }
            ats.push(at);
        }
        for at in &ats {
            for y in 0..h as i64 {
                for x in 0..w as i64 {
                    assert_eq!(
                        context_fast(&b, t, at, x, y),
                        context_slow(&b, t, at, x, y),
                        "模板 {t} AT {at:?} 位置 ({x},{y})"
                    );
                }
            }
        }
    }
}

use std::rc::Rc;

use super::bitmap::{Bitmap, set_budget};
use super::segment::Segment;
use super::{State, Stored, kind};

/// 一段：段号 0、页关联 1 字节，没有引用。
fn raw_segment(kind: u8, data: &[u8]) -> Vec<u8> {
    let mut b = vec![0, 0, 0, 0, kind, 0, 1];
    b.extend_from_slice(&(data.len() as u32).to_be_bytes());
    b.extend_from_slice(data);
    b
}

/// 页信息段：宽、高、两个分辨率、标志、条带信息（§7.4.8）。
fn page_info(w: u32, h: u32) -> Vec<u8> {
    let mut d = Vec::new();
    for v in [w, h, 0, 0] {
        d.extend_from_slice(&v.to_be_bytes());
    }
    d.extend_from_slice(&[0, 0, 0]);
    raw_segment(kind::PAGE_INFO, &d)
}

/// 分配上限刚够一页时页也交得出去：收尾是原地换算，不再计费。
#[test]
fn a_page_that_fits_the_budget_is_delivered() {
    let r = super::decode(&page_info(10, 10), None, 100).unwrap();
    assert_eq!((r.width, r.height), (10, 10));
    assert_eq!(r.data, vec![255; 100]);
    assert!(super::decode(&page_info(10, 10), None, 99).is_err());
}

/// 符号字典被反复引用：指针也计入分配上限。
#[test]
fn repeated_dictionary_references_are_charged() {
    let symbol = |n: usize| {
        set_budget(1 << 20);
        let b = Rc::new(Bitmap::new(1, 1, 0).unwrap());
        vec![b; n]
    };
    let mut st = State::default();
    st.stored.insert(1, Stored::Symbols(symbol(1000), None));
    let seg = Segment {
        number: 2,
        kind: kind::TEXT[1],
        refs: vec![1; 2000],
        data: &[],
    };
    // 2000 × 1000 个指针 = 16 MB，上限 1 MB 时在分配前就拒绝
    set_budget(1 << 20);
    let e = st.referred_symbols(&seg).unwrap_err();
    assert!(e.contains("上限"), "{e}");
    // 上限够时拼得出来，并且扣掉了指针的字节
    set_budget(32 << 20);
    assert_eq!(st.referred_symbols(&seg).unwrap().len(), 2_000_000);
    set_budget(2_000_000 * size_of::<Rc<Bitmap>>());
    st.referred_symbols(&seg).unwrap();
    let e = super::bitmap::charge(1).unwrap_err();
    assert!(e.contains("上限"), "{e}");
}

/// 保留的上下文计入分配上限：空字典段只有几十字节，却要留下上百 KB。
#[test]
fn retained_contexts_are_charged() {
    // [协议] 标志 0x0200：上下文保留、算术编码、模板 0；AT 4 对；导出数、新符号数都是 0
    let mut data = vec![0x02, 0x00];
    data.extend_from_slice(&[0; 8 + 8]);
    let segs: Vec<Vec<u8>> = (0..64).map(|_| data.clone()).collect();
    let mut st = State::default();
    set_budget(1 << 20);
    let mut failed = None;
    for (i, d) in segs.iter().enumerate() {
        let seg = Segment {
            number: i as u32,
            kind: kind::SYMBOL_DICT,
            refs: Vec::new(),
            data: d,
        };
        if let Err(e) = st.process(&seg) {
            failed = Some((i, e));
            break;
        }
    }
    let (at, e) = failed.expect("上限应该挡住");
    assert!(at > 0 && e.contains("上限"), "{at}: {e}");
}

/// 图样字典里每个图样的 `Bitmap` 头也计费：1×1 的图样不能只算 1 字节。
#[test]
fn pattern_headers_are_charged() {
    // [协议] 标志 0、HDPW 1、HDPH 1、GRAYMAX 0xFFFF：65536 个 1×1 图样
    let data = [0, 1, 1, 0, 0, 0xFF, 0xFF];
    set_budget(1 << 18);
    assert!(super::halftone::pattern_dict(&data).is_err());
    set_budget(16 << 20);
    assert_eq!(super::halftone::pattern_dict(&data).unwrap().len(), 65536);
}

/// 过了截止时间，半色调的合成循环要停（单图样 + 4000×4000 格能要上万亿次运算）。
#[test]
fn halftone_stops_at_the_deadline() {
    set_budget(256 << 20);
    let pattern = Bitmap::new(255, 255, 1).unwrap();
    // [协议] 半色调区域段头：标志 0、HGW、HGH 各 4000、HGX、HGY 0、HRX、HRY 0（§7.4.5.1）
    let mut head = vec![0u8];
    for v in [4000u32, 4000, 0, 0] {
        head.extend_from_slice(&v.to_be_bytes());
    }
    head.extend_from_slice(&[0; 4]);
    let info = super::RegionInfo {
        w: 16,
        h: 16,
        x: 0,
        y: 0,
        op: super::bitmap::Op::Or,
    };
    // wall-clock: 截止时间已经过了，什么机器上都一样
    let _b = crate::budget::enter(Some(std::time::Instant::now()));
    let e = super::halftone::region(&head, &info, &[pattern]).unwrap_err();
    assert!(e.contains("时限"), "{e}");
}
