//! 对照 libtiff 编出的码流。testdata/ 由 benchmarks/history/1.0/g1/make_ccitt.py（种子 11、3 张）生成：
//! r000N-{g4,g3,g3mr,g3fill}.ccitt 是同一张图的 T.6、T.4 一维、T.4 二维、T.4 二维且 EOL 字节对齐；
//! r000N.bits 是原图按行打包的位（每行补齐到字节）。Pillow 把黑白图存成 BlackIsZero，所以码流里的
//! “黑”游程是位为 1 的像素。

use super::codes::{BLACK_MAKEUP, BLACK_TERM, EXT_MAKEUP, WHITE_MAKEUP, WHITE_TERM, build};
use super::decode;
use crate::image::tiff::Ccitt;

/// [策略] 测试图都很小，给 PDF 默认的流大小上限
const MAX: usize = 64 << 20;

/// (名字, 宽, 高)
const IMAGES: &[(&str, u32, u32)] = &[("r0000", 1211, 33), ("r0001", 2200, 25), ("r0002", 32, 33)];

/// (后缀, /K, /EncodedByteAlign)
const MODES: &[(&str, i64, bool)] = &[
    ("g4", -1, false),
    ("g3", 0, false),
    ("g3mr", 1, false),
    ("g3fill", 1, true),
];

fn file(name: &str) -> Vec<u8> {
    let dir = concat!(
        env!("CARGO_MANIFEST_DIR"),
        "/src/image/decode/ccitt/testdata/"
    );
    std::fs::read(format!("{dir}{name}")).unwrap()
}

/// 期望的像素：位为 1 是码流里的黑，按滤镜缺省（/BlackIs1 假）解成 0。
fn want(name: &str, w: u32, h: u32) -> Vec<u8> {
    let bits = file(&format!("{name}.bits"));
    let stride = (w as usize).div_ceil(8);
    let mut v = Vec::with_capacity((w * h) as usize);
    for y in 0..h as usize {
        for x in 0..w as usize {
            let on = bits[y * stride + x / 8] >> (7 - x % 8) & 1 == 1;
            v.push(if on { 0 } else { 255 });
        }
    }
    v
}

fn params(k: i64, w: u32, rows: u32, byte_align: bool) -> Ccitt {
    Ccitt {
        k,
        columns: w,
        rows,
        black_is_1: false,
        byte_align,
    }
}

#[test]
fn code_tables_are_prefix_free() {
    assert!(build(&[WHITE_TERM, WHITE_MAKEUP, EXT_MAKEUP]).is_ok());
    assert!(build(&[BLACK_TERM, BLACK_MAKEUP, EXT_MAKEUP]).is_ok());
}

#[test]
fn matches_libtiff() {
    for &(name, w, h) in IMAGES {
        let expect = want(name, w, h);
        for &(suffix, k, align) in MODES {
            let data = file(&format!("{name}-{suffix}.ccitt"));
            // 给行数和不给行数（解到数据末尾）都要对
            for rows in [h, 0] {
                let r = decode(&data, &params(k, w, rows, align), MAX).unwrap();
                assert_eq!((r.width, r.height), (w, h), "{name}-{suffix} rows={rows}");
                assert!(r.data == expect, "{name}-{suffix} rows={rows} 像素不同");
            }
        }
    }
}

#[test]
fn black_is_1_flips_output() {
    let (name, w, h) = IMAGES[2];
    let data = file(&format!("{name}-g4.ccitt"));
    let p = Ccitt {
        black_is_1: true,
        ..params(-1, w, h, false)
    };
    let r = decode(&data, &p, MAX).unwrap();
    let flipped: Vec<u8> = want(name, w, h).iter().map(|v| 255 - v).collect();
    assert!(r.data == flipped);
}

#[test]
fn truncated_keeps_decoded_rows() {
    let (name, w, h) = IMAGES[0];
    let data = file(&format!("{name}-g4.ccitt"));
    let r = decode(&data[..data.len() / 2], &params(-1, w, h, false), MAX).unwrap();
    assert_eq!(r.height, h, "缺的行留白补足");
    // 前几行和完整解码一致
    let full = want(name, w, h);
    assert_eq!(r.data[..w as usize * 2], full[..w as usize * 2]);
}

#[test]
fn rejects_garbage_and_bombs() {
    assert!(decode(&[0, 0, 0, 0], &params(-1, 100, 10, false), MAX).is_err());
    assert!(decode(&[0xFF; 16], &params(0, 0, 10, false), MAX).is_err());
    // 声明 10 万 × 10 万像素：分配之前就拒绝
    assert!(decode(&[0x80], &params(-1, 100_000, 100_000, false), MAX).is_err());
}

/// 一维行里连续的 0 游程不推进位置：变化元素有上限，不能跟着码流无限增长。
#[test]
fn zero_runs_in_a_1d_row_are_bounded() {
    // [协议] 白 0 游程 00110101、黑 0 游程 0000110111（T.4 表 2）
    let mut bits = String::new();
    for _ in 0..64 {
        bits.push_str("00110101");
        bits.push_str("0000110111");
    }
    let bytes: Vec<u8> = bits
        .as_bytes()
        .chunks(8)
        .map(|c| {
            c.iter()
                .enumerate()
                .fold(0u8, |a, (i, &b)| a | ((b - b'0') << (7 - i)))
        })
        .collect();
    let e = decode(&bytes, &params(0, 8, 1, false), MAX).unwrap_err();
    assert!(e.contains("变化元素过多"), "{e}");
}

/// 过了截止时间不再解新行。
#[test]
fn stops_at_the_deadline() {
    let (name, w, h) = IMAGES[0];
    let data = file(&format!("{name}-g4.ccitt"));
    // wall-clock: 截止时间已经过了，什么机器上都一样
    let _b = crate::budget::enter(Some(std::time::Instant::now()));
    assert!(decode(&data, &params(-1, w, h, false), MAX).is_err());
}
