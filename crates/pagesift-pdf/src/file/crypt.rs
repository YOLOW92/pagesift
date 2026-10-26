//! 标准安全处理器（ISO 32000-2 §7.6.4）：只用空口令打开——用户口令为空，或所有者口令为空。
//!
//! 支持 RC4（40–128 位，V1/V2）、V4 加密过滤器（RC4 / AESV2）、V5 的 AESV3（R5、R6）。
//! 分组密码和摘要用 RustCrypto（aes、md-5、sha2），CBC 链接、填充和 RC4 在这里写。

use aes::cipher::{Array, BlockCipherDecrypt, BlockCipherEncrypt, KeyInit};
use md5::{Digest, Md5};
use sha2::{Sha256, Sha384, Sha512};

use super::{Dictionary, Object, ObjectId};

const PAD: [u8; 32] = [
    0x28, 0xBF, 0x4E, 0x5E, 0x4E, 0x75, 0x8A, 0x41, 0x64, 0x00, 0x4E, 0x56, 0xFF, 0xFA, 0x01, 0x08,
    0x2E, 0x2E, 0x00, 0xB6, 0xD0, 0x68, 0x3E, 0x80, 0x2F, 0x0C, 0xA9, 0xFE, 0x64, 0x53, 0x69, 0x7A,
];

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Method {
    Identity,
    Rc4,
    Aes128,
    Aes256,
}

#[derive(Debug)]
pub(crate) struct Crypt {
    key: Vec<u8>,
    streams: Method,
    strings: Method,
    /// /CF 里按名字定义的加密过滤器（流的 /Crypt 过滤器用名字指定）。
    named: Vec<(Vec<u8>, Method)>,
    /// /EncryptMetadata false：/Type /Metadata 的流不加密。
    pub(crate) encrypt_metadata: bool,
}

/// 打不开：需要口令，或者不是标准安全处理器。
#[derive(Debug)]
pub(crate) struct NeedsPassword;

impl Crypt {
    /// `encrypt` 是解好引用的 /Encrypt 字典，`id0` 是 trailer /ID 的第一个串。
    pub(crate) fn open(encrypt: &Dictionary, id0: &[u8]) -> Result<Crypt, NeedsPassword> {
        if encrypt.get(b"Filter").and_then(Object::as_name) != Some(b"Standard") {
            return Err(NeedsPassword);
        }
        let int = |k: &[u8]| encrypt.get(k).and_then(Object::as_i64);
        let bytes = |k: &[u8]| match encrypt.get(k) {
            Some(Object::String(s, _)) => s.clone(),
            _ => Vec::new(),
        };
        let v = int(b"V").unwrap_or(0);
        let r = int(b"R").unwrap_or(0);
        let encrypt_metadata = !matches!(
            encrypt.get(b"EncryptMetadata"),
            Some(Object::Boolean(false))
        );
        let (o, u) = (bytes(b"O"), bytes(b"U"));
        let (streams, strings) = match v {
            1 | 2 => (Method::Rc4, Method::Rc4),
            4 | 5 => (
                crypt_filter(encrypt, b"StmF"),
                crypt_filter(encrypt, b"StrF"),
            ),
            _ => return Err(NeedsPassword),
        };
        // 正文的流和串都不加密（口令只保护附件，/AuthEvent /EFOpen）：不需要密钥
        if streams == Method::Identity && strings == Method::Identity {
            return Ok(Crypt {
                key: Vec::new(),
                streams,
                strings,
                named: Vec::new(),
                encrypt_metadata,
            });
        }
        let key = if r >= 5 {
            v5_key(&o, &u, &bytes(b"OE"), &bytes(b"UE"), r)
        } else {
            let length = match r {
                2 => 5,
                // V4 的顶层 /Length 常缺省，此时按 128 位
                _ => (int(b"Length").unwrap_or(if v == 4 { 128 } else { 40 }) / 8).clamp(5, 16)
                    as usize,
            };
            let p = int(b"P").unwrap_or(0) as i32;
            let ctx = Legacy {
                o: &o,
                u: &u,
                p,
                id0,
                r,
                length,
                encrypt_metadata,
            };
            // R=4 而密钥不足 128 位时，Acrobat 先用 0 补到 16 字节再算每个对象的密钥（规范没写；
            // pdf.js issue 19484 / PR 19555 同样处理）
            ctx.user_key(&[]).or_else(|| ctx.owner_key(&[])).map(|mut k| {
                if r == 4 && k.len() < 16 {
                    k.resize(16, 0);
                }
                k
            })
        };
        let named = match encrypt.get(b"CF") {
            Some(Object::Dictionary(cf)) => {
                cf.0.iter()
                    .map(|(name, _)| (name.clone(), cf_method(cf, name)))
                    .collect()
            }
            _ => Vec::new(),
        };
        Ok(Crypt {
            key: key.ok_or(NeedsPassword)?,
            streams,
            strings,
            named,
            encrypt_metadata,
        })
    }

    pub(crate) fn decrypt_string(&self, id: ObjectId, data: &[u8]) -> Vec<u8> {
        self.decrypt(self.strings, id, data)
    }

    pub(crate) fn decrypt_stream(&self, id: ObjectId, data: &[u8]) -> Vec<u8> {
        self.decrypt(self.streams, id, data)
    }

    /// 流自己用 /Crypt 过滤器指定了加密过滤器的名字。
    pub(crate) fn decrypt_named(&self, name: &[u8], id: ObjectId, data: &[u8]) -> Vec<u8> {
        let method = self
            .named
            .iter()
            .find(|(n, _)| n == name)
            .map_or(Method::Identity, |(_, m)| *m);
        self.decrypt(method, id, data)
    }

    pub(crate) fn strings_encrypted(&self) -> bool {
        self.strings != Method::Identity
    }

    fn decrypt(&self, method: Method, (num, generation): ObjectId, data: &[u8]) -> Vec<u8> {
        let object_key = |aes: bool| {
            let mut h = Md5::new();
            h.update(&self.key);
            h.update(&num.to_le_bytes()[..3]);
            h.update(generation.to_le_bytes());
            if aes {
                h.update(b"sAlT");
            }
            let d = h.finalize();
            d[..(self.key.len() + 5).min(16)].to_vec()
        };
        match method {
            Method::Identity => data.to_vec(),
            Method::Rc4 => rc4(&object_key(false), data),
            Method::Aes128 => aes_cbc_decrypt(&object_key(true), data),
            Method::Aes256 => aes_cbc_decrypt(&self.key, data),
        }
    }
}

/// V4/V5：/StmF、/StrF 指向 /CF 里的过滤器；Identity 或缺省表示不加密。
fn crypt_filter(encrypt: &Dictionary, which: &[u8]) -> Method {
    let Some(name) = encrypt.get(which).and_then(Object::as_name) else {
        return Method::Identity;
    };
    match encrypt.get(b"CF") {
        Some(Object::Dictionary(cf)) => cf_method(cf, name),
        _ => Method::Identity,
    }
}

fn cf_method(cf: &Dictionary, name: &[u8]) -> Method {
    if name == b"Identity" {
        return Method::Identity;
    }
    let cfm = match cf.get(name) {
        Some(Object::Dictionary(f)) => f.get(b"CFM").and_then(Object::as_name),
        _ => None,
    };
    match cfm {
        Some(b"V2") => Method::Rc4,
        Some(b"AESV2") => Method::Aes128,
        Some(b"AESV3") => Method::Aes256,
        _ => Method::Identity,
    }
}

/// R2–R4 的口令算法（算法 2、6、7）。
struct Legacy<'a> {
    o: &'a [u8],
    u: &'a [u8],
    p: i32,
    id0: &'a [u8],
    r: i64,
    length: usize,
    encrypt_metadata: bool,
}

impl Legacy<'_> {
    fn padded(password: &[u8]) -> [u8; 32] {
        let mut out = PAD;
        let n = password.len().min(32);
        out[..n].copy_from_slice(&password[..n]);
        out[n..].copy_from_slice(&PAD[..32 - n]);
        out
    }

    /// 算法 2：由用户口令算文件密钥；算法 6 验证。
    fn user_key(&self, password: &[u8]) -> Option<Vec<u8>> {
        let mut h = Md5::new();
        h.update(Self::padded(password));
        h.update(&self.o[..self.o.len().min(32)]);
        h.update(self.p.to_le_bytes());
        h.update(self.id0);
        if self.r >= 4 && !self.encrypt_metadata {
            h.update([0xFF; 4]);
        }
        let mut key = h.finalize().to_vec();
        if self.r >= 3 {
            for _ in 0..50 {
                key = Md5::digest(&key[..self.length]).to_vec();
            }
        }
        key.truncate(self.length);
        let ok = if self.r == 2 {
            self.u.len() >= 32 && rc4(&key, &PAD) == self.u[..32]
        } else {
            let mut h = Md5::new();
            h.update(PAD);
            h.update(self.id0);
            let mut x = rc4(&key, &h.finalize());
            for i in 1..=19u8 {
                let k: Vec<u8> = key.iter().map(|b| b ^ i).collect();
                x = rc4(&k, &x);
            }
            self.u.len() >= 16 && x[..16] == self.u[..16]
        };
        ok.then_some(key)
    }

    /// 算法 7：由所有者口令解出用户口令，再走用户口令的流程。
    fn owner_key(&self, password: &[u8]) -> Option<Vec<u8>> {
        let mut k = Md5::digest(Self::padded(password)).to_vec();
        if self.r >= 3 {
            for _ in 0..50 {
                k = Md5::digest(&k).to_vec();
            }
        }
        k.truncate(self.length);
        let mut user = self.o[..self.o.len().min(32)].to_vec();
        if self.r == 2 {
            user = rc4(&k, &user);
        } else {
            for i in (0..=19u8).rev() {
                let ki: Vec<u8> = k.iter().map(|b| b ^ i).collect();
                user = rc4(&ki, &user);
            }
        }
        self.user_key(&user)
    }
}

/// R5/R6（AESV3）：先试空的用户口令，再试空的所有者口令；口令对了就解出 32 字节文件密钥。
fn v5_key(o: &[u8], u: &[u8], oe: &[u8], ue: &[u8], r: i64) -> Option<Vec<u8>> {
    if o.len() < 48 || u.len() < 48 {
        return None;
    }
    let pw: &[u8] = &[];
    if hash(r, pw, &u[32..40], &[]) == u[..32] {
        let k = hash(r, pw, &u[40..48], &[]);
        return aes256_cbc_nopad_zero_iv(&k, ue);
    }
    if hash(r, pw, &o[32..40], &u[..48]) == o[..32] {
        let k = hash(r, pw, &o[40..48], &u[..48]);
        return aes256_cbc_nopad_zero_iv(&k, oe);
    }
    None
}

/// R5：SHA-256；R6：算法 2.B（SHA-256/384/512 与 AES-128 交替迭代）。
fn hash(r: i64, password: &[u8], salt: &[u8], udata: &[u8]) -> Vec<u8> {
    let mut h = Sha256::new();
    h.update(password);
    h.update(salt);
    h.update(udata);
    let mut k = h.finalize().to_vec();
    if r < 6 {
        return k;
    }
    let mut round = 0u32;
    loop {
        let mut k1 = Vec::with_capacity(64 * (password.len() + k.len() + udata.len()));
        for _ in 0..64 {
            k1.extend_from_slice(password);
            k1.extend_from_slice(&k);
            k1.extend_from_slice(udata);
        }
        let e = aes128_cbc_encrypt_nopad(&k[..16], &k[16..32], &k1);
        let m = e[..16].iter().map(|&b| u32::from(b)).sum::<u32>() % 3;
        k = match m {
            0 => Sha256::digest(&e).to_vec(),
            1 => Sha384::digest(&e).to_vec(),
            _ => Sha512::digest(&e).to_vec(),
        };
        round += 1;
        if round >= 64 && u32::from(*e.last().unwrap_or(&0)) <= round - 32 {
            break;
        }
    }
    k.truncate(32);
    k
}

fn rc4(key: &[u8], data: &[u8]) -> Vec<u8> {
    let mut s: [u8; 256] = std::array::from_fn(|i| i as u8);
    if key.is_empty() {
        return data.to_vec();
    }
    let mut j = 0u8;
    for i in 0..256 {
        j = j.wrapping_add(s[i]).wrapping_add(key[i % key.len()]);
        s.swap(i, usize::from(j));
    }
    let (mut i, mut j) = (0u8, 0u8);
    data.iter()
        .map(|&b| {
            i = i.wrapping_add(1);
            j = j.wrapping_add(s[usize::from(i)]);
            s.swap(usize::from(i), usize::from(j));
            b ^ s[usize::from(s[usize::from(i)].wrapping_add(s[usize::from(j)]))]
        })
        .collect()
}

fn block(chunk: &[u8]) -> Array<u8, aes::cipher::consts::U16> {
    let mut a = [0u8; 16];
    a.copy_from_slice(chunk);
    Array::from(a)
}

/// AES-CBC 解密：前 16 字节是 IV，去掉 PKCS#7 填充（填充不合法时保留原样，与 pdf.js 一致）。
/// 不足一块的尾巴丢掉。
fn aes_cbc_decrypt(key: &[u8], data: &[u8]) -> Vec<u8> {
    if data.len() < 32 {
        // 只有 IV（或更短）：空串
        return Vec::new();
    }
    let (iv, body) = data.split_at(16);
    let body = &body[..body.len() / 16 * 16];
    let mut out = match key.len() {
        16 => cbc_decrypt(
            &aes::Aes128::new_from_slice(key).expect("16-byte key"),
            iv,
            body,
        ),
        32 => cbc_decrypt(
            &aes::Aes256::new_from_slice(key).expect("32-byte key"),
            iv,
            body,
        ),
        _ => return Vec::new(),
    };
    if let Some(&n) = out.last() {
        let n = usize::from(n);
        if (1..=16).contains(&n)
            && n <= out.len()
            && out[out.len() - n..].iter().all(|&b| usize::from(b) == n)
        {
            out.truncate(out.len() - n);
        }
    }
    out
}

fn cbc_decrypt<C: BlockCipherDecrypt<BlockSize = aes::cipher::consts::U16>>(
    cipher: &C,
    iv: &[u8],
    body: &[u8],
) -> Vec<u8> {
    let mut prev = block(iv);
    let mut out = Vec::with_capacity(body.len());
    for chunk in body.as_chunks::<16>().0 {
        let mut b = block(chunk);
        cipher.decrypt_block(&mut b);
        out.extend(b.iter().zip(prev.iter()).map(|(x, y)| x ^ y));
        prev = block(chunk);
    }
    out
}

fn aes256_cbc_nopad_zero_iv(key: &[u8], data: &[u8]) -> Option<Vec<u8>> {
    if data.len() < 32 {
        return None;
    }
    let cipher = aes::Aes256::new_from_slice(key).ok()?;
    Some(cbc_decrypt(&cipher, &[0; 16], &data[..32]))
}

fn aes128_cbc_encrypt_nopad(key: &[u8], iv: &[u8], data: &[u8]) -> Vec<u8> {
    let cipher = aes::Aes128::new_from_slice(key).expect("16-byte key");
    let mut prev = block(iv);
    let mut out = Vec::with_capacity(data.len());
    for chunk in data.as_chunks::<16>().0 {
        let mut b = block(chunk);
        for (x, y) in b.iter_mut().zip(prev.iter()) {
            *x ^= y;
        }
        cipher.encrypt_block(&mut b);
        out.extend_from_slice(&b);
        prev = b;
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn rc4_known_vector() {
        // RFC 6229 之外最常见的测试向量：Key="Key", Plaintext="Plaintext"
        assert_eq!(
            rc4(b"Key", b"Plaintext"),
            [0xBB, 0xF3, 0x16, 0xE8, 0xD9, 0x40, 0xAF, 0x0A, 0xD3]
        );
    }

    /// R=4、40 位 RC4（pdf.js issue19484_1 的加密字典）：空口令校验通过，文件密钥补到 16 字节。
    #[test]
    fn r4_short_key_is_padded_to_16_bytes() {
        let hex = |s: &str| (0..s.len()).step_by(2).map(|i| u8::from_str_radix(&s[i..i + 2], 16).unwrap()).collect::<Vec<u8>>();
        let mut cf = Dictionary::default();
        let mut std_cf = Dictionary::default();
        std_cf.push(b"CFM".to_vec(), Object::Name(b"V2".to_vec()));
        std_cf.push(b"Length".to_vec(), Object::Integer(5));
        cf.push(b"StdCF".to_vec(), Object::Dictionary(std_cf));
        let mut e = Dictionary::default();
        e.push(b"Filter".to_vec(), Object::Name(b"Standard".to_vec()));
        e.push(b"V".to_vec(), Object::Integer(4));
        e.push(b"R".to_vec(), Object::Integer(4));
        e.push(b"Length".to_vec(), Object::Integer(40));
        e.push(b"P".to_vec(), Object::Integer(-3392));
        e.push(b"EncryptMetadata".to_vec(), Object::Boolean(false));
        e.push(b"CF".to_vec(), Object::Dictionary(cf));
        e.push(b"StmF".to_vec(), Object::Name(b"StdCF".to_vec()));
        e.push(b"StrF".to_vec(), Object::Name(b"StdCF".to_vec()));
        let o = hex("8a1dae2eb45c5cc580269a1e4a4bbee55881274b427f3f92465ecbea7bff2334");
        let u = hex("7f995c2cf48b82990a16f1d52f34ea5400000000000000000000000000000000");
        e.push(b"O".to_vec(), Object::String(o, super::super::StringFormat::Literal));
        e.push(b"U".to_vec(), Object::String(u, super::super::StringFormat::Literal));
        let c = Crypt::open(&e, &hex("061046f3e5aef840af0a1519dae542b6")).expect("空口令应能打开");
        assert_eq!(c.key.len(), 16);
        assert!(c.key[5..].iter().all(|&b| b == 0));
    }

    #[test]
    fn aes_cbc_roundtrip_with_padding() {
        let key = [7u8; 16];
        let iv = [9u8; 16];
        let mut plain = b"hello, encrypted pdf".to_vec();
        let pad = 16 - plain.len() % 16;
        plain.extend(std::iter::repeat_n(pad as u8, pad));
        let mut data = iv.to_vec();
        data.extend(aes128_cbc_encrypt_nopad(&key, &iv, &plain));
        assert_eq!(aes_cbc_decrypt(&key, &data), b"hello, encrypted pdf");
    }
}
