//! XXH64(seed 0):zstd 帧尾校验和是它的低 32 位。
const P1: u64 = 11400714785074694791;
const P2: u64 = 14029467366897019727;
const P3: u64 = 1609587929392839161;
const P4: u64 = 9650029242287828579;
const P5: u64 = 2870177450012600261;

fn round(acc: u64, x: u64) -> u64 {
    acc.wrapping_add(x.wrapping_mul(P2))
        .rotate_left(31)
        .wrapping_mul(P1)
}

fn merge(h: u64, v: u64) -> u64 {
    (h ^ round(0, v)).wrapping_mul(P1).wrapping_add(P4)
}

fn r64(b: &[u8]) -> u64 {
    u64::from_le_bytes(b[..8].try_into().expect("8 字节"))
}

pub(crate) fn xxh64(data: &[u8]) -> u64 {
    let mut rest = data;
    let mut h;
    if data.len() >= 32 {
        let (mut v1, mut v2, mut v3, mut v4) =
            (P1.wrapping_add(P2), P2, 0u64, 0u64.wrapping_sub(P1));
        while rest.len() >= 32 {
            v1 = round(v1, r64(rest));
            v2 = round(v2, r64(&rest[8..]));
            v3 = round(v3, r64(&rest[16..]));
            v4 = round(v4, r64(&rest[24..]));
            rest = &rest[32..];
        }
        h = v1
            .rotate_left(1)
            .wrapping_add(v2.rotate_left(7))
            .wrapping_add(v3.rotate_left(12))
            .wrapping_add(v4.rotate_left(18));
        for v in [v1, v2, v3, v4] {
            h = merge(h, v);
        }
    } else {
        h = P5;
    }
    h = h.wrapping_add(data.len() as u64);
    while rest.len() >= 8 {
        h = (h ^ round(0, r64(rest)))
            .rotate_left(27)
            .wrapping_mul(P1)
            .wrapping_add(P4);
        rest = &rest[8..];
    }
    if rest.len() >= 4 {
        let k = u32::from_le_bytes(rest[..4].try_into().expect("4 字节")) as u64;
        h = (h ^ k.wrapping_mul(P1))
            .rotate_left(23)
            .wrapping_mul(P2)
            .wrapping_add(P3);
        rest = &rest[4..];
    }
    for &b in rest {
        h = (h ^ (b as u64).wrapping_mul(P5))
            .rotate_left(11)
            .wrapping_mul(P1);
    }
    h ^= h >> 33;
    h = h.wrapping_mul(P2);
    h ^= h >> 29;
    h = h.wrapping_mul(P3);
    h ^ (h >> 32)
}
