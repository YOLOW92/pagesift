//! Keccak：ML-KEM 要的 SHA3-256、SHA3-512、SHAKE128、SHAKE256（FIPS 202）。

/// [协议] FIPS 202 §3.2.5 的轮常数（24 轮）。
const ROUND_CONSTANTS: [u64; 24] = [
    0x0000000000000001,
    0x0000000000008082,
    0x800000000000808a,
    0x8000000080008000,
    0x000000000000808b,
    0x0000000080000001,
    0x8000000080008081,
    0x8000000000008009,
    0x000000000000008a,
    0x0000000000000088,
    0x0000000080008009,
    0x000000008000000a,
    0x000000008000808b,
    0x800000000000008b,
    0x8000000000008089,
    0x8000000000008003,
    0x8000000000008002,
    0x8000000000000080,
    0x000000000000800a,
    0x800000008000000a,
    0x8000000080008081,
    0x8000000000008080,
    0x0000000080000001,
    0x8000000080008008,
];

/// [协议] FIPS 202 §3.2.2 的旋转量和 §3.2.3 的换位，按 ρ、π 合并后的走位。
const ROTATIONS: [u32; 24] = [
    1, 3, 6, 10, 15, 21, 28, 36, 45, 55, 2, 14, 27, 41, 56, 8, 25, 43, 62, 18, 39, 61, 20, 44,
];
/// [协议] 同上。
const PILANES: [usize; 24] = [
    10, 7, 11, 17, 18, 3, 5, 16, 8, 21, 24, 4, 15, 23, 19, 13, 12, 2, 20, 14, 22, 9, 6, 1,
];

// [协议] FIPS 202
fn permute(a: &mut [u64; 25]) {
    for rc in ROUND_CONSTANTS {
        // θ
        let mut c = [0u64; 5]; // [协议] FIPS 202
        for x in 0..5 {
            c[x] = a[x] ^ a[x + 5] ^ a[x + 10] ^ a[x + 15] ^ a[x + 20]; // [协议] FIPS 202
        }
        for x in 0..5 {
            let d = c[(x + 4) % 5] ^ c[(x + 1) % 5].rotate_left(1); // [协议] FIPS 202
            for y in 0..5 {
                a[x + 5 * y] ^= d; // [协议] FIPS 202
            }
        }
        // ρ、π
        let mut last = a[1];
        for (rotation, plane) in ROTATIONS.iter().zip(PILANES) {
            let next = a[plane];
            a[plane] = last.rotate_left(*rotation);
            last = next;
        }
        // χ
        for y in 0..5 {
            let row = [
                a[5 * y],     // [协议] FIPS 202
                a[5 * y + 1], // [协议] FIPS 202
                a[5 * y + 2], // [协议] FIPS 202
                a[5 * y + 3], // [协议] FIPS 202
                a[5 * y + 4], // [协议] FIPS 202
            ];
            for x in 0..5 {
                a[5 * y + x] = row[x] ^ (!row[(x + 1) % 5] & row[(x + 2) % 5]); // [协议] FIPS 202
            }
        }
        // ι
        a[0] ^= rc;
    }
}

/// 海绵结构：先吸收，再挤出任意长度。
pub(super) struct Sponge {
    state: [u64; 25], // [协议] FIPS 202
    rate: usize,
    /// 吸收时是已写入的字节数，挤出时是这一块已读出的字节数。
    pos: usize,
    suffix: u8,
    squeezing: bool,
}

/// [协议] FIPS 202 §6.1、§6.2：SHA3-256 和 SHAKE256 的 rate 是 136，SHA3-512 是 72，SHAKE128 是 168；
/// 域分隔后缀 SHA3 是 0x06，SHAKE 是 0x1f。
const RATE_256: usize = 136;
const RATE_512: usize = 72; // [协议] FIPS 202
const RATE_SHAKE128: usize = 168; // [协议] FIPS 202
const SUFFIX_SHA3: u8 = 0x06; // [协议] FIPS 202
const SUFFIX_SHAKE: u8 = 0x1f; // [协议] FIPS 202
const LANE_BYTES: usize = 8; // [协议] FIPS 202

impl Sponge {
    fn new(rate: usize, suffix: u8) -> Self {
        Self {
            state: [0; 25], // [协议] FIPS 202
            rate,
            pos: 0,
            suffix,
            squeezing: false,
        }
    }

    pub(super) fn shake128() -> Self {
        Self::new(RATE_SHAKE128, SUFFIX_SHAKE)
    }

    pub(super) fn shake256() -> Self {
        Self::new(RATE_256, SUFFIX_SHAKE)
    }

    fn xor_byte(&mut self, index: usize, byte: u8) {
        self.state[index / LANE_BYTES] ^= u64::from(byte) << (LANE_BYTES * (index % LANE_BYTES));
    }

    pub(super) fn absorb(&mut self, mut data: &[u8]) {
        debug_assert!(!self.squeezing);
        while !data.is_empty() {
            let take = (self.rate - self.pos).min(data.len());
            for (i, byte) in data[..take].iter().enumerate() {
                self.xor_byte(self.pos + i, *byte);
            }
            self.pos += take;
            data = &data[take..];
            if self.pos == self.rate {
                permute(&mut self.state);
                self.pos = 0;
            }
        }
    }

    fn finish_absorbing(&mut self) {
        self.xor_byte(self.pos, self.suffix);
        // [协议] 填充 pad10*1 的最后一位（FIPS 202 §5.1）
        self.xor_byte(self.rate - 1, 0x80);
        permute(&mut self.state);
        self.pos = 0;
        self.squeezing = true;
    }

    pub(super) fn squeeze(&mut self, out: &mut [u8]) {
        if !self.squeezing {
            self.finish_absorbing();
        }
        for byte in out {
            if self.pos == self.rate {
                permute(&mut self.state);
                self.pos = 0;
            }
            let lane = self.state[self.pos / LANE_BYTES];
            *byte = (lane >> (LANE_BYTES * (self.pos % LANE_BYTES))) as u8;
            self.pos += 1;
        }
    }
}

// [协议] FIPS 202
pub(super) fn sha3_256(parts: &[&[u8]]) -> [u8; 32] {
    let mut sponge = Sponge::new(RATE_256, SUFFIX_SHA3);
    for part in parts {
        sponge.absorb(part);
    }
    let mut out = [0; 32]; // [协议] FIPS 202
    sponge.squeeze(&mut out);
    out
}

// [协议] FIPS 202
pub(super) fn sha3_512(parts: &[&[u8]]) -> [u8; 64] {
    let mut sponge = Sponge::new(RATE_512, SUFFIX_SHA3);
    for part in parts {
        sponge.absorb(part);
    }
    let mut out = [0; 64]; // [协议] FIPS 202
    sponge.squeeze(&mut out);
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    fn hex(bytes: &[u8]) -> String {
        bytes.iter().map(|b| format!("{b:02x}")).collect()
    }

    /// 已知答案：FIPS 202 的 "abc" 和空串；SHAKE 用 NIST 示例的前 32 字节。
    #[test]
    fn known_answers() {
        assert_eq!(
            hex(&sha3_256(&[b"abc"])),
            "3a985da74fe225b2045c172d6bd390bd855f086e3e9d525b46bfe24511431532"
        );
        assert_eq!(
            hex(&sha3_512(&[b"abc"])),
            "b751850b1a57168a5693cd924b6b096e08f621827444f70d884f5d0240d2712e\
             10e116e9192af3c91a7ec57647e3934057340b4cf408d5a56592f8274eec53f0"
        );
        let mut out = [0; 32];
        Sponge::shake128().squeeze(&mut out);
        assert_eq!(
            hex(&out),
            "7f9c2ba4e88f827d616045507605853ed73b8093f6efbc88eb1a6eacfa66ef26"
        );
        Sponge::shake256().squeeze(&mut out);
        assert_eq!(
            hex(&out),
            "46b9dd2b0ba88d13233b3feb743eeb243fcd52ea62b81b82b50c27646ed5762f"
        );
    }

    /// 跨块吸收和挤出：一次写完和分段写结果相同。
    #[test]
    fn split_absorb_matches() {
        let data = vec![0xa5u8; 1000];
        let mut whole = Sponge::shake128();
        whole.absorb(&data);
        let mut parts = Sponge::shake128();
        for chunk in data.chunks(7) {
            parts.absorb(chunk);
        }
        let (mut a, mut b) = ([0; 400], [0; 400]);
        whole.squeeze(&mut a);
        for chunk in b.chunks_mut(13) {
            parts.squeeze(chunk);
        }
        assert_eq!(a, b);
    }
}
