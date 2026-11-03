use super::*;

#[path = "mlkem_vectors.rs"]
mod vectors;

fn unhex(text: &str) -> Vec<u8> {
    (0..text.len())
        .step_by(2)
        .map(|i| u8::from_str_radix(&text[i..i + 2], 16).unwrap())
        .collect()
}

fn hex(bytes: &[u8]) -> String {
    bytes.iter().map(|b| format!("{b:02x}")).collect()
}

/// Barrett 在整个可能的输入范围上等于真正的取模。
#[test]
fn reduce_matches_modulo() {
    let q = u32::from(Q);
    for x in (0..q * q + q).step_by(97).chain(q * q..q * q + q) {
        assert_eq!(u32::from(reduce(x)), x % q, "{x}");
    }
}

/// 压缩和解压等于 FIPS 203 §4.2.1 的定义（用精确的整数算术对照）。
#[test]
fn compress_matches_definition() {
    let q = u64::from(Q);
    for d in [1usize, DV, DU] {
        for x in 0..Q {
            let exact = ((u64::from(x) << (d + 1)) + q) / (2 * q) % (1 << d);
            assert_eq!(u64::from(compress(x, d)), exact, "compress d={d} x={x}");
        }
        for y in 0..(1u16 << d) {
            let exact = (u64::from(y) * 2 * q + (1 << d)) / (2 << d);
            assert_eq!(u64::from(decompress(y, d)), exact, "decompress d={d} y={y}");
        }
    }
}

#[test]
fn acvp_keygen() {
    for (d, z, ek_hash, dk_hash) in vectors::KEYGEN {
        let (d, z): ([u8; 32], [u8; 32]) =
            (unhex(d).try_into().unwrap(), unhex(z).try_into().unwrap());
        let (ek, dk) = keygen(&d, &z);
        assert_eq!(hex(&sha3_256(&[&ek[..]])), *ek_hash);
        assert_eq!(hex(&sha3_256(&[&dk[..]])), *dk_hash);
    }
    assert!(!vectors::KEYGEN.is_empty());
}

#[test]
fn acvp_encapsulate() {
    for (ek, m, c_hash, k) in vectors::ENCAP {
        let ek: [u8; EK_BYTES] = unhex(ek).try_into().unwrap();
        let m: [u8; 32] = unhex(m).try_into().unwrap();
        let (ct, key) = encapsulate(&ek, &m);
        assert_eq!(hex(&sha3_256(&[&ct[..]])), *c_hash);
        assert_eq!(hex(&key), *k);
    }
    assert!(!vectors::ENCAP.is_empty());
}

/// 向量里包含改过的密文，走隐式拒绝那条路。
#[test]
fn acvp_decapsulate() {
    for (dk, c, k) in vectors::DECAP {
        let dk: [u8; DK_BYTES] = unhex(dk).try_into().unwrap();
        let ct: [u8; CT_BYTES] = unhex(c).try_into().unwrap();
        assert_eq!(hex(&decapsulate(&dk, &ct)), *k);
    }
    assert!(!vectors::DECAP.is_empty());
}

#[test]
fn round_trip_and_implicit_rejection() {
    let (ek, dk) = keygen(&[7; 32], &[9; 32]);
    let (mut ct, key) = encapsulate(&ek, &[3; 32]);
    assert_eq!(decapsulate(&dk, &ct), key);
    ct[0] ^= 1;
    assert_ne!(decapsulate(&dk, &ct), key);
}
