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
            let mut at = [(0i8, 0i8); 4];
            for a in &mut at {
                // [策略] AT 偏移取 −8…7、行 −8…0，覆盖图外取 0
                *a = ((rnd() % 16) as i8 - 8, -((rnd() % 9) as i8));
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
