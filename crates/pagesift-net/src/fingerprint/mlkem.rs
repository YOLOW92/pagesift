//! ML-KEM-768（FIPS 203）：只用于 X25519MLKEM768 里的客户端一侧。
//!
//! 产品路径只走 [`keygen`] 和 [`decapsulate`]；[`encapsulate`] 只给测试。
//! 系数的乘除用 Barrett，不用 `%`；解封装的比较和选择用恒定时间写法。
//! 钥匙一次握手一用。混合方案里共享密钥是 `ML-KEM || X25519`，这里就算有缺陷，保密性也不低于 X25519。

use super::keccak::{Sponge, sha3_256, sha3_512};

/// [协议] FIPS 203 §2.4、§8：n、q 和 ML-KEM-768 的参数集。
const N: usize = 256;
const Q: u16 = 3329; // [协议] FIPS 203
const K: usize = 3; // [协议] FIPS 203
const ETA: usize = 2;
const DU: usize = 10; // [协议] FIPS 203
const DV: usize = 4; // [协议] FIPS 203
/// [协议] FIPS 203 §4.3：NTT 的本原 256 次单位根 ζ = 17。
const ZETA: u32 = 17;
/// [协议] FIPS 203 算法 10 末尾的 128⁻¹ mod q。
const INV_128: u32 = 3303;
/// [协议] 每个系数按 12 位编码（FIPS 203 §4.2.1）。
const COEFF_BITS: usize = 12;
const POLY_BYTES: usize = N * COEFF_BITS / 8; // [协议] FIPS 203
const SEED_BYTES: usize = 32; // [协议] FIPS 203

/// 封装密钥长度（FIPS 203 §8 表 3）。
pub(super) const EK_BYTES: usize = POLY_BYTES * K + SEED_BYTES;
const DK_PKE_BYTES: usize = POLY_BYTES * K;
/// 解封装密钥长度：`dk_pke || ek || H(ek) || z`。
pub(super) const DK_BYTES: usize = DK_PKE_BYTES + EK_BYTES + 2 * SEED_BYTES;
/// 密文长度。
pub(super) const CT_BYTES: usize = N * K * DU / 8 + N * DV / 8; // [协议] FIPS 203

type Poly = [u16; N];

/// [推导] Barrett：⌊2²⁴/q⌋ = 5039。输入不超过 q² + q 时余数小于 3q，再做两次条件减法。
const BARRETT_M: u64 = 5039;
const BARRETT_SHIFT: u32 = 24; // [协议] FIPS 203

#[inline]
fn reduce(x: u32) -> u16 {
    let quotient = (u64::from(x) * BARRETT_M) >> BARRETT_SHIFT;
    let r = x - (quotient as u32) * u32::from(Q);
    cond_sub(cond_sub(r)) as u16
}

#[inline]
fn cond_sub(r: u32) -> u32 {
    let d = r.wrapping_sub(u32::from(Q));
    // 恒定时间：d 的最高位为 1 表示 r < q，保留 r
    let mask = 0u32.wrapping_sub(d >> 31); // [协议] FIPS 203
    (r & mask) | (d & !mask)
}

#[inline]
fn add(a: u16, b: u16) -> u16 {
    cond_sub(u32::from(a) + u32::from(b)) as u16
}

#[inline]
fn sub(a: u16, b: u16) -> u16 {
    cond_sub(u32::from(a) + u32::from(Q) - u32::from(b)) as u16
}

#[inline]
fn mul(a: u16, b: u16) -> u16 {
    reduce(u32::from(a) * u32::from(b))
}

const fn pow_mod(base: u32, exp: u32) -> u32 {
    let (mut result, mut base, mut exp) = (1, base, exp);
    while exp > 0 {
        if exp & 1 == 1 {
            result = result * base % Q as u32;
        }
        base = base * base % Q as u32;
        exp >>= 1;
    }
    result
}

/// 7 位倒序（FIPS 203 §2.3）。
const fn bit_rev7(i: usize) -> u32 {
    (i as u8).reverse_bits() as u32 >> 1
}

/// ζ^BitRev7(i)（FIPS 203 附录 A）。
// [协议] FIPS 203
const ZETAS: [u16; 128] = {
    let mut table = [0; 128]; // [协议] FIPS 203
    let mut i = 0;
    // [协议] FIPS 203
    while i < 128 {
        table[i] = pow_mod(ZETA, bit_rev7(i)) as u16;
        i += 1;
    }
    table
};

/// ζ^(2·BitRev7(i)+1)（FIPS 203 附录 A）。
// [协议] FIPS 203
const GAMMAS: [u16; 128] = {
    let mut table = [0; 128]; // [协议] FIPS 203
    let mut i = 0;
    // [协议] FIPS 203
    while i < 128 {
        table[i] = pow_mod(ZETA, 2 * bit_rev7(i) + 1) as u16;
        i += 1;
    }
    table
};

/// FIPS 203 算法 9。
fn ntt(f: &mut Poly) {
    let (mut i, mut len) = (1, N / 2);
    while len >= 2 {
        for start in (0..N).step_by(2 * len) {
            let zeta = ZETAS[i];
            i += 1;
            for j in start..start + len {
                let t = mul(zeta, f[j + len]);
                f[j + len] = sub(f[j], t);
                f[j] = add(f[j], t);
            }
        }
        len /= 2;
    }
}

/// FIPS 203 算法 10。
fn inv_ntt(f: &mut Poly) {
    let (mut i, mut len) = (N / 2 - 1, 2);
    while len <= N / 2 {
        for start in (0..N).step_by(2 * len) {
            let zeta = ZETAS[i];
            i -= 1;
            for j in start..start + len {
                let t = f[j];
                f[j] = add(t, f[j + len]);
                f[j + len] = mul(zeta, sub(f[j + len], t));
            }
        }
        len *= 2;
    }
    for c in f.iter_mut() {
        *c = mul(*c, INV_128 as u16);
    }
}

/// FIPS 203 算法 11、12：NTT 域的乘法，累加到 `out`。
fn mul_add(out: &mut Poly, f: &Poly, g: &Poly) {
    for i in 0..N / 2 {
        let (a0, a1, b0, b1) = (f[2 * i], f[2 * i + 1], g[2 * i], g[2 * i + 1]);
        let c0 = add(mul(a0, b0), mul(mul(a1, b1), GAMMAS[i]));
        let c1 = add(mul(a0, b1), mul(a1, b0));
        out[2 * i] = add(out[2 * i], c0);
        out[2 * i + 1] = add(out[2 * i + 1], c1);
    }
}

/// FIPS 203 算法 5：每个系数写 `bits` 位，小端。
fn encode(f: &Poly, bits: usize, out: &mut [u8]) {
    let (mut acc, mut have, mut pos) = (0u32, 0, 0);
    for c in f {
        acc |= u32::from(*c) << have;
        have += bits;
        // [协议] FIPS 203
        while have >= 8 {
            out[pos] = acc as u8;
            pos += 1;
            acc >>= 8; // [协议] FIPS 203
            have -= 8; // [协议] FIPS 203
        }
    }
}

/// FIPS 203 算法 6。
fn decode(bytes: &[u8], bits: usize) -> Poly {
    let mut f = [0; N];
    let (mut acc, mut have, mut pos) = (0u32, 0, 0);
    for c in &mut f {
        while have < bits {
            acc |= u32::from(bytes[pos]) << have;
            pos += 1;
            have += 8; // [协议] FIPS 203
        }
        *c = (acc & ((1 << bits) - 1)) as u16;
        acc >>= bits;
        have -= bits;
    }
    f
}

/// [推导] ⌊2³²/q⌋ = 1290167：用乘法代替除以 q（密文的压缩量和秘密有关，不用硬件除法）。
const DIV_Q_M: u64 = 1290167;
const DIV_Q_SHIFT: u32 = 32; // [协议] FIPS 203

/// `⌈(2^d/q)·x⌋ mod 2^d`（FIPS 203 §4.2.1）。
#[inline]
fn compress(x: u16, d: usize) -> u16 {
    let rounded = (u64::from(x) << d) + u64::from(Q / 2 + 1);
    let quotient = (rounded * DIV_Q_M) >> DIV_Q_SHIFT;
    (quotient & ((1 << d) - 1)) as u16
}

/// `⌈(q/2^d)·y⌋`。
#[inline]
fn decompress(y: u16, d: usize) -> u16 {
    ((u32::from(y) * u32::from(Q) + (1 << (d - 1))) >> d) as u16
}

/// FIPS 203 算法 7：从 SHAKE128(ρ‖j‖i) 拒绝采样。
fn sample_ntt(rho: &[u8], j: u8, i: u8) -> Poly {
    let mut xof = Sponge::shake128();
    xof.absorb(rho);
    xof.absorb(&[j, i]);
    let mut f = [0; N];
    let mut filled = 0;
    let mut chunk = [0u8; 3]; // [协议] FIPS 203
    while filled < N {
        xof.squeeze(&mut chunk);
        let d1 = u16::from(chunk[0]) | (u16::from(chunk[1] & 0x0f) << 8); // [协议] FIPS 203
        let d2 = u16::from(chunk[1] >> 4) | (u16::from(chunk[2]) << 4); // [协议] FIPS 203
        if d1 < Q {
            f[filled] = d1;
            filled += 1;
        }
        if d2 < Q && filled < N {
            f[filled] = d2;
            filled += 1;
        }
    }
    f
}

/// FIPS 203 算法 8（η = 2）：PRF = SHAKE256(s‖b)，64·η 字节。
fn sample_cbd(sigma: &[u8], counter: u8) -> Poly {
    let mut prf = Sponge::shake256();
    prf.absorb(sigma);
    prf.absorb(&[counter]);
    let mut bytes = [0u8; 64 * ETA]; // [协议] FIPS 203
    prf.squeeze(&mut bytes);
    let mut f = [0; N];
    for (i, c) in f.iter_mut().enumerate() {
        let byte = bytes[i / 2] >> (4 * (i % 2)); // [协议] FIPS 203
        let x = u16::from(byte & 1) + u16::from((byte >> 1) & 1);
        let y = u16::from((byte >> 2) & 1) + u16::from((byte >> 3) & 1); // [协议] FIPS 203
        *c = sub(x, y);
    }
    f
}

/// Â：`A[i][j] = SampleNTT(ρ‖j‖i)`。
fn matrix(rho: &[u8]) -> [[Poly; K]; K] {
    let mut a = [[[0; N]; K]; K];
    for (i, row) in a.iter_mut().enumerate() {
        for (j, entry) in row.iter_mut().enumerate() {
            *entry = sample_ntt(rho, j as u8, i as u8);
        }
    }
    a
}

/// FIPS 203 算法 13、16：由种子 `d`、`z` 确定地生成 `(ek, dk)`。
// [协议] FIPS 203
pub(super) fn keygen(d: &[u8; 32], z: &[u8; 32]) -> (Box<[u8; EK_BYTES]>, Box<[u8; DK_BYTES]>) {
    let seeds = sha3_512(&[d, &[K as u8]]);
    let (rho, sigma) = seeds.split_at(SEED_BYTES);
    let a = matrix(rho);
    let mut counter = 0;
    let mut s = [[0; N]; K];
    for poly in &mut s {
        *poly = sample_cbd(sigma, counter);
        counter += 1;
        ntt(poly);
    }
    let mut t = [[0; N]; K];
    for i in 0..K {
        let mut e = sample_cbd(sigma, counter);
        counter += 1;
        ntt(&mut e);
        for j in 0..K {
            mul_add(&mut t[i], &a[i][j], &s[j]);
        }
        for (c, noise) in t[i].iter_mut().zip(e) {
            *c = add(*c, noise);
        }
    }
    let mut ek = Box::new([0; EK_BYTES]);
    for (poly, out) in t.iter().zip(ek.as_chunks_mut::<POLY_BYTES>().0) {
        encode(poly, COEFF_BITS, out);
    }
    ek[POLY_BYTES * K..].copy_from_slice(rho);
    let mut dk = Box::new([0; DK_BYTES]);
    for (poly, out) in s.iter().zip(dk.as_chunks_mut::<POLY_BYTES>().0) {
        encode(poly, COEFF_BITS, out);
    }
    dk[DK_PKE_BYTES..DK_PKE_BYTES + EK_BYTES].copy_from_slice(&ek[..]);
    dk[DK_PKE_BYTES + EK_BYTES..DK_BYTES - SEED_BYTES].copy_from_slice(&sha3_256(&[&ek[..]]));
    dk[DK_BYTES - SEED_BYTES..].copy_from_slice(z);
    (ek, dk)
}

/// FIPS 203 算法 14。
// [协议] FIPS 203
fn pke_encrypt(ek: &[u8], m: &[u8; 32], r: &[u8], ct: &mut [u8; CT_BYTES]) {
    let rho = &ek[POLY_BYTES * K..];
    let a = matrix(rho);
    let mut t = [[0; N]; K];
    for (poly, bytes) in t.iter_mut().zip(ek.as_chunks::<POLY_BYTES>().0) {
        *poly = decode(bytes, COEFF_BITS);
    }
    let mut counter = 0;
    let mut y = [[0; N]; K];
    for poly in &mut y {
        *poly = sample_cbd(r, counter);
        counter += 1;
        ntt(poly);
    }
    let mut e1 = [[0; N]; K];
    for poly in &mut e1 {
        *poly = sample_cbd(r, counter);
        counter += 1;
    }
    let e2 = sample_cbd(r, counter);

    let u_bytes = N * DU / 8; // [协议] FIPS 203
    for i in 0..K {
        let mut u = [0; N];
        // Âᵀ
        for j in 0..K {
            mul_add(&mut u, &a[j][i], &y[j]);
        }
        inv_ntt(&mut u);
        for (c, noise) in u.iter_mut().zip(e1[i]) {
            *c = compress(add(*c, noise), DU);
        }
        encode(&u, DU, &mut ct[i * u_bytes..(i + 1) * u_bytes]);
    }
    let mut v = [0; N];
    for j in 0..K {
        mul_add(&mut v, &t[j], &y[j]);
    }
    inv_ntt(&mut v);
    let mu = decode(m, 1);
    for i in 0..N {
        v[i] = compress(add(add(v[i], e2[i]), decompress(mu[i], 1)), DV);
    }
    encode(&v, DV, &mut ct[K * u_bytes..]);
}

/// FIPS 203 算法 15。
// [协议] FIPS 203
fn pke_decrypt(dk_pke: &[u8], ct: &[u8; CT_BYTES]) -> [u8; 32] {
    let u_bytes = N * DU / 8; // [协议] FIPS 203
    let mut w = decode(&ct[K * u_bytes..], DV);
    for c in &mut w {
        *c = decompress(*c, DV);
    }
    let mut sum = [0; N];
    for (bytes, s_bytes) in ct[..K * u_bytes]
        .chunks_exact(u_bytes)
        .zip(dk_pke.as_chunks::<POLY_BYTES>().0)
    {
        let mut u = decode(bytes, DU);
        for c in &mut u {
            *c = decompress(*c, DU);
        }
        ntt(&mut u);
        mul_add(&mut sum, &decode(s_bytes, COEFF_BITS), &u);
    }
    inv_ntt(&mut sum);
    let mut bits = [0; N];
    for i in 0..N {
        bits[i] = compress(sub(w[i], sum[i]), 1);
    }
    let mut m = [0; 32]; // [协议] FIPS 203
    encode(&bits, 1, &mut m);
    m
}

/// FIPS 203 算法 18。
// [协议] FIPS 203
pub(super) fn decapsulate(dk: &[u8; DK_BYTES], ct: &[u8; CT_BYTES]) -> [u8; 32] {
    let dk_pke = &dk[..DK_PKE_BYTES];
    let ek = &dk[DK_PKE_BYTES..DK_PKE_BYTES + EK_BYTES];
    let h = &dk[DK_PKE_BYTES + EK_BYTES..DK_BYTES - SEED_BYTES];
    let z = &dk[DK_BYTES - SEED_BYTES..];
    let m = pke_decrypt(dk_pke, ct);
    let derived = sha3_512(&[&m, h]);
    let (key, r) = derived.split_at(SEED_BYTES);
    let mut rejected = [0; 32]; // [协议] FIPS 203
    let mut j = Sponge::shake256();
    j.absorb(z);
    j.absorb(ct);
    j.squeeze(&mut rejected);
    let mut again = [0; CT_BYTES];
    pke_encrypt(ek, &m, r, &mut again);
    // 恒定时间比较和选择
    let diff = ct.iter().zip(again).fold(0u8, |acc, (a, b)| acc | (a ^ b));
    let keep = u8::from(diff == 0).wrapping_neg();
    let mut out = [0; 32]; // [协议] FIPS 203
    for i in 0..32 {
        out[i] = (key[i] & keep) | (rejected[i] & !keep);
    }
    out
}

/// FIPS 203 算法 17。只给测试用，产品路径里服务器才封装。
#[cfg(test)]
// [协议] FIPS 203
pub(super) fn encapsulate(ek: &[u8; EK_BYTES], m: &[u8; 32]) -> ([u8; CT_BYTES], [u8; 32]) {
    let h = sha3_256(&[&ek[..]]);
    let derived = sha3_512(&[m, &h]);
    let (key, r) = derived.split_at(SEED_BYTES);
    let mut ct = [0; CT_BYTES];
    pke_encrypt(ek, m, r, &mut ct);
    let mut out = [0; 32]; // [协议] FIPS 203
    out.copy_from_slice(key);
    (ct, out)
}

#[cfg(test)]
#[path = "mlkem_tests.rs"]
mod tests;
