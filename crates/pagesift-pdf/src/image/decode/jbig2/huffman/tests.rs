//! 霍夫曼表的单元测试：码流都按规范手工构造。

use super::standard::ROWS;
use super::{Bits, Kind, assign, custom, standard, symbol_ids};

/// 高位在前的写位器，用来手工拼码流。
#[derive(Default)]
struct W {
    bytes: Vec<u8>,
    n: usize,
}

impl W {
    /// 写 `v` 的低 `len` 位。
    fn put(&mut self, v: u64, len: u32) -> &mut W {
        for k in (0..len).rev() {
            if self.n.is_multiple_of(8) {
                self.bytes.push(0);
            }
            let b = ((v >> k) & 1) as u8;
            *self.bytes.last_mut().unwrap() |= b << (7 - self.n % 8);
            self.n += 1;
        }
        self
    }

    /// 写一个位串，如 "1110"。
    fn code(&mut self, s: &str) -> &mut W {
        self.put(u64::from_str_radix(s, 2).unwrap(), s.len() as u32)
    }
}

/// [协议] 附录 B.5 各表“编码”一列，按 standard.rs 的行序（普通行、下界行、上界行、OOB 行）。
const SPEC_CODES: [&[&str]; 15] = [
    // B.1
    &["0", "10", "110", "111"],
    // B.2
    &["0", "10", "110", "1110", "11110", "111110", "111111"],
    // B.3
    &[
        "11111110", "0", "10", "110", "1110", "11110", "11111111", "1111110", "111110",
    ],
    // B.4
    &["0", "10", "110", "1110", "11110", "11111"],
    // B.5
    &[
        "1111110", "0", "10", "110", "1110", "11110", "1111111", "111110",
    ],
    // B.6
    &[
        "11100", "1000", "1001", "1010", "11101", "11110", "1011", "00", "010", "011", "1100",
        "1101", "111110", "111111",
    ],
    // B.7
    &[
        "1000", "000", "1001", "11010", "11011", "1010", "1011", "11100", "11101", "1100", "001",
        "010", "011", "11110", "11111",
    ],
    // B.8
    &[
        "11111100",
        "111111100",
        "11111101",
        "111111101",
        "1111100",
        "1010",
        "00",
        "11010",
        "111010",
        "100",
        "111011",
        "1011",
        "1100",
        "11011",
        "11100",
        "111100",
        "1111101",
        "111101",
        "111111110",
        "111111111",
        "01",
    ],
    // B.9
    &[
        "11111100",
        "111111100",
        "11111101",
        "111111101",
        "1111100",
        "1010",
        "010",
        "011",
        "11010",
        "111010",
        "100",
        "111011",
        "1011",
        "1100",
        "11011",
        "11100",
        "111100",
        "1111101",
        "111101",
        "111111110",
        "111111111",
        "00",
    ],
    // B.10
    &[
        "1111010", "11111100", "1111011", "11000", "00", "11001", "110110", "1111100", "11111101",
        "01", "11010", "110111", "111000", "111001", "111010", "111011", "111100", "1111101",
        "11111110", "11111111", "10",
    ],
    // B.11
    &[
        "0", "10", "1100", "1101", "11100", "11101", "111100", "1111010", "1111011", "1111100",
        "1111101", "1111110", "1111111",
    ],
    // B.12
    &[
        "0", "10", "110", "11100", "11101", "111100", "1111010", "1111011", "1111100", "1111101",
        "1111110", "11111110", "11111111",
    ],
    // B.13
    &[
        "0", "100", "1100", "11100", "1101", "101", "111010", "111011", "111100", "111101",
        "111110", "1111110", "1111111",
    ],
    // B.14
    &["100", "101", "0", "110", "111"],
    // B.15
    &[
        "1111100", "111100", "11100", "1100", "100", "0", "101", "1101", "11101", "111101",
        "1111101", "1111110", "1111111",
    ],
];

fn lens(n: usize) -> Vec<u8> {
    ROWS[n].iter().map(|l| l.preflen).collect()
}

#[test]
fn bits_reader() {
    let data = [0b1011_0011, 0xFF, 0x00, 0x12, 0x34];
    let mut r = Bits::new(&data);
    assert_eq!(r.bits(3).unwrap(), 0b101);
    assert_eq!(r.bit().unwrap(), 1);
    assert_eq!(r.bits(6).unwrap(), 0b00_1111);
    assert_eq!(r.byte_pos(), 2);
    r.align();
    assert_eq!(r.byte_pos(), 2);
    assert_eq!(r.bits(0).unwrap(), 0);
    let mut r = Bits::new(&data);
    r.bits(4).unwrap();
    assert_eq!(r.bits(32).unwrap(), 0x3FF0_0123);
    assert!(r.bits(5).is_err());
    assert!(r.bits(33).is_err());
}

/// 每张标准表都能建出来；赋出的码两两不互为前缀，而且码长之和恰好填满（完整前缀码）。
#[test]
fn standard_tables_build_prefix_free() {
    for n in 0..15 {
        assert!(standard(n as u8 + 1).is_ok(), "B.{}", n + 1);
        let lens = lens(n);
        let codes = assign(&lens).unwrap();
        for i in 0..codes.len() {
            for j in 0..codes.len() {
                if i == j {
                    continue;
                }
                let (a, la, b, lb) = (codes[i], lens[i], codes[j], lens[j]);
                let prefix = la <= lb && b >> (lb - la) == a;
                assert!(!prefix, "B.{}：第 {i} 行的码是第 {j} 行的前缀", n + 1);
            }
        }
        let kraft: u64 = lens.iter().map(|&l| 1u64 << (32 - l)).sum();
        assert_eq!(kraft, 1u64 << 32, "B.{} 不是完整前缀码", n + 1);
    }
    assert!(standard(0).is_err());
    assert!(standard(16).is_err());
}

/// §B.3 赋出的码与规范表的“编码”一列逐条一致。
#[test]
fn standard_codes_match_spec() {
    for (n, spec) in SPEC_CODES.iter().enumerate() {
        let lens = lens(n);
        let codes = assign(&lens).unwrap();
        assert_eq!(codes.len(), spec.len(), "B.{} 行数", n + 1);
        for (i, s) in spec.iter().enumerate() {
            let got = format!("{:0w$b}", codes[i], w = usize::from(lens[i]));
            assert_eq!(got, *s, "B.{} 第 {i} 行", n + 1);
        }
    }
}

/// 普通行的区间首尾相接；下界行在最小值之下、上界行在最大值之上紧接着。
#[test]
fn standard_ranges_contiguous() {
    for (n, rows) in ROWS.iter().enumerate() {
        let mut ranges: Vec<(i64, i64)> = rows
            .iter()
            .filter(|l| l.kind == Kind::Normal)
            .map(|l| (l.low, l.low + (1i64 << l.rangelen)))
            .collect();
        ranges.sort();
        for w in ranges.windows(2) {
            assert_eq!(w[0].1, w[1].0, "B.{} 区间不连续", n + 1);
        }
        let (min, max) = (ranges[0].0, ranges.last().unwrap().1);
        for l in rows.iter() {
            match l.kind {
                Kind::Lower => assert_eq!(l.low, min - 1, "B.{} 下界行", n + 1),
                Kind::Upper => assert_eq!(l.low, max, "B.{} 上界行", n + 1),
                _ => {}
            }
        }
    }
}

/// 每张标准表的每一行：写出它的码和偏移（取 0 和最大偏移），解出来应是区间两端。
#[test]
fn standard_every_line_round_trip() {
    for (n, rows) in ROWS.iter().enumerate() {
        let lens = lens(n);
        let codes = assign(&lens).unwrap();
        let t = standard(n as u8 + 1).unwrap();
        for (i, l) in rows.iter().enumerate() {
            let max = (1u64 << l.rangelen) - 1;
            for off in [0, max] {
                let mut w = W::default();
                w.put(u64::from(codes[i]), u32::from(lens[i]));
                w.put(off, u32::from(l.rangelen));
                let want = match l.kind {
                    Kind::Oob => None,
                    Kind::Lower => i32::try_from(l.low - off as i64).ok(),
                    _ => i32::try_from(l.low + off as i64).ok(),
                };
                let got = t.decode(&mut Bits::new(&w.bytes));
                match (l.kind, want) {
                    // 上、下界行的最大偏移超出 i32：应报错而不是回绕
                    (Kind::Lower | Kind::Upper, None) => assert!(got.is_err()),
                    _ => assert_eq!(got.unwrap(), want, "B.{} 第 {i} 行偏移 {off}", n + 1),
                }
            }
        }
    }
}

/// 几张标准表上手工按规范拼的码流，连续解出已知值：普通行、下界行、上界行、OOB。
#[test]
fn standard_known_values() {
    let mut w = W::default();
    // B.1：0–15 行 "0" + 4 位 5；16–271 行 "10" + 8 位 200；上界行 "111" + 32 位 1000
    w.code("0").put(5, 4);
    w.code("10").put(200, 8);
    w.code("111").put(1000, 32);
    // B.3：−256…−1 行 "11111110" + 8 位 16；下界行 "11111111" + 32 位 5；OOB "111110"；
    // 值 2 "110"；上界行 "1111110" + 32 位 0
    w.code("11111110").put(16, 8);
    w.code("11111111").put(5, 32);
    w.code("111110");
    w.code("110");
    w.code("1111110").put(0, 32);
    // B.8：OOB "01"；−1 "1010"；0–1 行 "00" + 1 位 1；下界行 "111111110" + 32 位 4
    w.code("01");
    w.code("1010");
    w.code("00").put(1, 1);
    w.code("111111110").put(4, 32);
    // B.15：下界行 "1111110" + 32 位 0；−24…−9 行 "1111100" + 4 位 15；上界行 "1111111" + 32 位 7
    w.code("1111110").put(0, 32);
    w.code("1111100").put(15, 4);
    w.code("1111111").put(7, 32);
    // B.14：−2 "100"；2 "111"
    w.code("100").code("111");

    let mut r = Bits::new(&w.bytes);
    let b = |n| standard(n).unwrap();
    let expect: [(u8, Option<i32>); 18] = [
        (1, Some(5)),
        (1, Some(216)),
        (1, Some(66808)),
        (3, Some(-240)),
        (3, Some(-262)),
        (3, None),
        (3, Some(2)),
        (3, Some(75)),
        (8, None),
        (8, Some(-1)),
        (8, Some(1)),
        (8, Some(-20)),
        (15, Some(-25)),
        (15, Some(-9)),
        (15, Some(32)),
        (14, Some(-2)),
        (14, Some(2)),
        (2, None),
    ];
    for (k, &(n, v)) in expect[..17].iter().enumerate() {
        assert_eq!(b(n).decode(&mut r).unwrap(), v, "第 {k} 个（B.{n}）");
    }
    // B.2 的 OOB "111111" 单独测；字节里剩下的 "11" 不是完整的码，再读应报错
    let mut w = W::default();
    w.code("111111").code("11");
    let mut r2 = Bits::new(&w.bytes);
    assert_eq!(b(2).decode(&mut r2).unwrap(), expect[17].1);
    assert!(b(2).decode(&mut r2).is_err());
}

/// 手工构造的码表段：HTOOB = 1，HTPS = 3，HTRS = 2，HTLOW = −3，HTHIGH = 7。
/// 普通行 (PREFLEN 1, RANGELEN 3) 覆盖 −3…4，(2, 1) 覆盖 5–6；下界行、上界行 PREFLEN 4，OOB 行 3。
/// 按 §B.3 赋码：−3…4 "0"，5–6 "10"，OOB "110"，下界 "1110"，上界 "1111"。
#[test]
fn custom_table_segment() {
    let mut seg = vec![0x15]; // 位 0 HTOOB=1；位 1–3 = HTPS−1 = 2；位 4–6 = HTRS−1 = 1
    seg.extend_from_slice(&(-3i32).to_be_bytes());
    seg.extend_from_slice(&7i32.to_be_bytes());
    let mut w = W::default();
    w.put(1, 3).put(3, 2); // 行 1
    w.put(2, 3).put(1, 2); // 行 2
    w.put(4, 3); // 下界行
    w.put(4, 3); // 上界行
    w.put(3, 3); // OOB 行
    seg.extend_from_slice(&w.bytes);
    let t = custom(&seg).unwrap();

    let mut w = W::default();
    w.code("0").put(5, 3); // −3 + 5 = 2
    w.code("0").put(0, 3); // −3
    w.code("10").put(1, 1); // 6
    w.code("1110").put(10, 32); // (−3 − 1) − 10 = −14
    w.code("1111").put(3, 32); // 7 + 3 = 10
    w.code("110"); // OOB
    let mut r = Bits::new(&w.bytes);
    for want in [Some(2), Some(-3), Some(6), Some(-14), Some(10), None] {
        assert_eq!(t.decode(&mut r).unwrap(), want);
    }

    // 没有 HTOOB：同样的行去掉 OOB，下界、上界变成 "1100"、"1101"，"1110" 不在表里
    let mut seg2 = seg.clone();
    seg2[0] = 0x14;
    let t2 = custom(&seg2).unwrap();
    let mut w = W::default();
    w.code("1101").put(1, 32); // 7 + 1 = 8
    assert_eq!(t2.decode(&mut Bits::new(&w.bytes)).unwrap(), Some(8));
    let mut w = W::default();
    w.code("1110").put(0, 40);
    assert!(t2.decode(&mut Bits::new(&w.bytes)).is_err());

    // 码长超额（三行都是 1 位）应报错；数据不全应报错
    let mut bad = vec![0x02]; // HTPS = 2，HTRS = 1，无 OOB
    bad.extend_from_slice(&0i32.to_be_bytes());
    bad.extend_from_slice(&1i32.to_be_bytes());
    let mut w = W::default();
    w.put(1, 2).put(0, 1).put(1, 2).put(1, 2);
    bad.extend_from_slice(&w.bytes);
    assert!(custom(&bad).is_err());
    assert!(custom(&seg[..5]).is_err());
}

/// 手工构造的符号 ID 码长：游程码 2→"00"、3→"01"、32→"10"、33→"110"、0→"1110"、34→"1111"，
/// 其余码长 0。码长序列：3，32（重复 3 次），33（5 个 0），2，34（12 个 0），3，3，0，共 25 个符号，
/// 码长部分结束在字节中间。赋码：ID 9 "00"；ID 0–3 "010"…"101"；ID 22 "110"；ID 23 "111"。
#[test]
fn symbol_id_table() {
    let mut run_lens = [0u64; 35];
    run_lens[2] = 2;
    run_lens[3] = 2;
    run_lens[32] = 2;
    run_lens[33] = 3;
    run_lens[0] = 4;
    run_lens[34] = 4;
    let mut w = W::default();
    for l in run_lens {
        w.put(l, 4);
    }
    w.code("01"); // 码长 3
    w.code("10").put(0, 2); // 32：重复前一个码长 3 + 0 次
    w.code("110").put(2, 3); // 33：3 + 2 = 5 个 0
    w.code("00"); // 码长 2
    w.code("1111").put(1, 7); // 34：11 + 1 = 12 个 0
    w.code("01").code("01"); // 码长 3、3
    w.code("1110"); // 码长 0
    assert_ne!(w.n % 8, 0, "码长部分应结束在字节中间");
    let head = w.n.div_ceil(8);
    // 对齐之后的下一字节起：ID 23、9、0、3、22
    w.n = head * 8;
    w.code("111").code("00").code("010").code("101").code("110");

    let mut r = Bits::new(&w.bytes);
    let t = symbol_ids(&mut r, 25).unwrap();
    assert_eq!(r.byte_pos(), head);
    for want in [23, 9, 0, 3, 22] {
        assert_eq!(t.decode(&mut r).unwrap(), Some(want));
    }

    // 符号数少于游程展开的数目时报错；游程码 32 打头（没有前一个码长）报错
    assert!(symbol_ids(&mut Bits::new(&w.bytes), 3).is_err());
    let mut w = W::default();
    for l in run_lens {
        w.put(l, 4);
    }
    w.code("10").put(0, 2);
    assert!(symbol_ids(&mut Bits::new(&w.bytes), 5).is_err());
}
