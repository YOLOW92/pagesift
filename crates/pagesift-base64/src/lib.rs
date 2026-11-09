//! Base64 encoding and decoding for PageSift, with runtime-dispatched AVX2 and
//! NEON kernels and scalar fallbacks.
//!
//! Whitespace mode ignores exactly ASCII space, tab, carriage return and line
//! feed. Padding is required when `padding` is true and forbidden otherwise.
//! Non-zero unused bits are rejected in both modes.

use std::fmt;
use std::mem::MaybeUninit;

mod compat;
pub use compat::decode_standard as decode_standard_compat;
#[cfg(test)]
mod threshold_tests;

#[cfg(target_arch = "x86_64")]
mod avx2;
#[cfg(target_arch = "aarch64")]
mod neon;

#[cfg(target_arch = "x86_64")]
fn avx2_available() -> bool {
    static DETECTED: std::sync::OnceLock<bool> = std::sync::OnceLock::new();
    *DETECTED.get_or_init(|| std::is_x86_feature_detected!("avx2"))
}

#[cfg(target_arch = "aarch64")]
fn neon_available() -> bool {
    static DETECTED: std::sync::OnceLock<bool> = std::sync::OnceLock::new();
    *DETECTED.get_or_init(|| std::arch::is_aarch64_feature_detected!("neon"))
}

/// Byte-to-sextet table for the strict scalar path; 0xff marks a non-alphabet
/// byte. Two 256-byte tables stay in L1 and remove the range-match chain for
/// short inputs.
const STANDARD_ALPHABET: &[u8; 64] =
    b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789+/";
const URL_ALPHABET: &[u8; 64] = b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789-_";

const fn decode_table(alphabet: &[u8; 64]) -> [u8; 256] {
    let mut table = [0xff; 256];
    let mut i = 0;
    while i < 64 {
        table[alphabet[i] as usize] = i as u8;
        i += 1;
    }
    table
}

static STANDARD_DECODE_TABLE: [u8; 256] = decode_table(STANDARD_ALPHABET);
static URL_DECODE_TABLE: [u8; 256] = decode_table(URL_ALPHABET);

/// Alphabet used for encoding and decoding.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum Alphabet {
    Standard,
    UrlSafe,
}

/// Codec options.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct Codec {
    pub alphabet: Alphabet,
    pub padding: bool,
    pub ignore_whitespace: bool,
}

/// Standard alphabet, with mandatory padding and no whitespace.
pub const STANDARD: Codec = Codec {
    alphabet: Alphabet::Standard,
    padding: true,
    ignore_whitespace: false,
};

/// A malformed input or an insufficient output buffer.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum Error {
    InvalidByte { byte: u8, offset: usize },
    InvalidLength,
    InvalidPadding,
    OutputTooSmall,
}

impl fmt::Display for Error {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::InvalidByte { byte, offset } => {
                write!(f, "Invalid byte {byte}, offset {offset}.")
            }
            Self::InvalidLength => f.write_str("Invalid input length."),
            Self::InvalidPadding => f.write_str("Invalid padding."),
            Self::OutputTooSmall => f.write_str("Output buffer too small."),
        }
    }
}

impl std::error::Error for Error {}

impl Codec {
    /// Exact output length, or `None` if it cannot fit in a `usize`.
    pub fn encoded_len(self, len: usize) -> Option<usize> {
        // 规律 2：3 个输入字节对应 4 个输出字符。
        let full = (len / 3).checked_mul(4)?;
        let tail = match (len % 3, self.padding) {
            (0, _) => 0,
            (_, true) => 4,
            (n, false) => n + 1,
        };
        full.checked_add(tail)
    }

    fn value(self, b: u8, offset: usize) -> Result<u8, Error> {
        match b {
            b'A'..=b'Z' => Ok(b - b'A'),
            b'a'..=b'z' => Ok(b - b'a' + 26),
            b'0'..=b'9' => Ok(b - b'0' + 52),
            b'+' if self.alphabet == Alphabet::Standard => Ok(62),
            b'/' if self.alphabet == Alphabet::Standard => Ok(63),
            b'-' if self.alphabet == Alphabet::UrlSafe => Ok(62),
            b'_' if self.alphabet == Alphabet::UrlSafe => Ok(63),
            _ => Err(Error::InvalidByte { byte: b, offset }),
        }
    }

    /// Encode into a caller-provided buffer.
    pub fn encode(self, input: &[u8], output: &mut [u8]) -> Result<usize, Error> {
        let len = self.encoded_len(input.len()).ok_or(Error::InvalidLength)?;
        if output.len() < len {
            return Err(Error::OutputTooSmall);
        }
        let mut out = 0;
        let mut start = 0;
        #[cfg(target_arch = "x86_64")]
        if input.len() >= 24 && avx2_available() {
            // SAFETY: runtime detection guarantees AVX2 and `output` has the
            // exact required encoded length checked above.
            start = unsafe { avx2::encode(input, output, self.alphabet) };
            out = start / 3 * 4;
        }
        #[cfg(target_arch = "aarch64")]
        if input.len() >= 24 && neon_available() {
            // SAFETY: runtime detection guarantees NEON and the exact output
            // length has been checked above.
            start = unsafe { neon::encode(input, output, self.alphabet) };
            out = start / 3 * 4;
        }
        // Rule 1: four 6-bit symbols are mapped in parallel inside a u32.
        // Adding a biased constant sets each byte's top bit iff its value
        // exceeds the relevant interval boundary; none of the byte sums
        // reaches 256, so there is no carry between lanes.
        let remaining = input.len() - start;
        let groups = remaining / 3;
        let table = match self.alphabet {
            Alphabet::Standard => STANDARD_ALPHABET,
            Alphabet::UrlSafe => URL_ALPHABET,
        };
        if remaining < 24 {
            // Rule 1: for a short tail the 64-byte alphabet table beats the
            // SWAR arithmetic; four L1 lookups map one group.
            for i in 0..groups {
                let p = start + i * 3;
                let (a, b, c) = (input[p], input[p + 1], input[p + 2]);
                let chars = [
                    table[(a >> 2) as usize],
                    table[((a & 3) << 4 | b >> 4) as usize],
                    table[((b & 15) << 2 | c >> 6) as usize],
                    table[(c & 63) as usize],
                ];
                output[out..out + 4].copy_from_slice(&chars);
                out += 4;
            }
        } else {
            for i in 0..groups {
                let p = start + i * 3;
                let (a, b, c) = (input[p], input[p + 1], input[p + 2]);
                let x = u32::from_le_bytes([
                    a >> 2,
                    (a & 3) << 4 | b >> 4,
                    (b & 15) << 2 | c >> 6,
                    c & 63,
                ]);
                let above = |bias: u32| ((x.wrapping_add(bias)) & 0x8080_8080) >> 7;
                let gt25 = above(0x6666_6666);
                let gt51 = above(0x4c4c_4c4c);
                let gt61 = above(0x4242_4242);
                let gt62 = above(0x4141_4141);
                let (sub, add) = match self.alphabet {
                    Alphabet::Standard => (15, 3),
                    Alphabet::UrlSafe => (13, 49),
                };
                let chars = x
                    .wrapping_add(0x4141_4141)
                    .wrapping_add(gt25 * 6)
                    .wrapping_sub(gt51 * 75)
                    .wrapping_sub(gt61 * sub)
                    .wrapping_add(gt62 * add);
                output[out..out + 4].copy_from_slice(&chars.to_le_bytes());
                out += 4;
            }
        }
        // At most two bytes remain after the whole groups; encode them with
        // the same table and append the configured padding.
        let tail = &input[start + groups * 3..];
        if !tail.is_empty() {
            let a = tail[0];
            output[out] = table[(a >> 2) as usize];
            if tail.len() == 2 {
                let b = tail[1];
                output[out + 1] = table[((a & 3) << 4 | b >> 4) as usize];
                output[out + 2] = table[((b & 15) << 2) as usize];
                out += 3;
                if self.padding {
                    output[out] = b'=';
                    out += 1;
                }
            } else {
                output[out + 1] = table[((a & 3) << 4) as usize];
                out += 2;
                if self.padding {
                    output[out] = b'=';
                    output[out + 1] = b'=';
                    out += 2;
                }
            }
        }
        Ok(out)
    }

    /// Append encoded input to an existing byte buffer.
    pub fn append_encode(self, input: &[u8], output: &mut Vec<u8>) -> Result<usize, Error> {
        let len = self.encoded_len(input.len()).ok_or(Error::InvalidLength)?;
        let start = output.len();
        let end = start.checked_add(len).ok_or(Error::InvalidLength)?;
        output.resize(end, 0);
        self.encode(input, &mut output[start..])
    }

    /// Encode input as an ASCII string.
    pub fn encode_to_string(self, input: &[u8]) -> Result<String, Error> {
        let mut out = Vec::new();
        self.append_encode(input, &mut out)?;
        // Encoding produces only ASCII; keep this conversion safe.
        String::from_utf8(out).map_err(|_| Error::InvalidLength)
    }

    fn whitespace(self, b: u8) -> bool {
        self.ignore_whitespace && matches!(b, b' ' | b'\t' | b'\r' | b'\n')
    }

    fn allocation_len(self, input: &[u8]) -> Result<usize, Error> {
        if self.ignore_whitespace {
            return self.validate(input);
        }
        // Rule 2: for an unbroken base64 string, length and final padding
        // determine the exact result size. Validation still happens during
        // decoding; this calculation never treats guessed content as valid.
        let len = input.len();
        let tail = match len % 4 {
            0 => 0,
            1 => return Err(Error::InvalidLength),
            2 => 1,
            _ => 2,
        };
        let padding = input
            .iter()
            .rev()
            .take(2)
            .take_while(|&&b| b == b'=')
            .count();
        (len / 4 * 3 + tail)
            .checked_sub(padding)
            .ok_or(Error::InvalidPadding)
    }

    /// Validate input and return its exact decoded length.
    pub fn validate(self, input: &[u8]) -> Result<usize, Error> {
        let mut count = 0usize;
        let mut pads = 0usize;
        let mut last = 0u8;
        for (offset, &b) in input.iter().enumerate() {
            if self.whitespace(b) {
                continue;
            }
            if b == b'=' {
                if !self.padding {
                    return Err(Error::InvalidPadding);
                }
                pads += 1;
                if pads > 2 {
                    return Err(Error::InvalidPadding);
                }
            } else {
                if pads != 0 {
                    return Err(Error::InvalidPadding);
                }
                last = self.value(b, offset)?;
                count += 1;
            }
        }
        let rem = count % 4;
        if rem == 1 {
            return Err(Error::InvalidLength);
        }
        if self.padding {
            let expected = match rem {
                0 => 0,
                2 => 2,
                3 => 1,
                _ => unreachable!(),
            };
            if pads != expected {
                return Err(Error::InvalidPadding);
            }
        }
        if (rem == 2 && last & 15 != 0) || (rem == 3 && last & 3 != 0) {
            return Err(Error::InvalidPadding);
        }
        // 规律 2：完整 4 字符输出 3 字节；尾部长度精确可知。
        Ok(count / 4 * 3
            + match rem {
                2 => 1,
                3 => 2,
                _ => 0,
            })
    }

    /// Decode into a caller-provided buffer. On error it may contain a partial result.
    pub fn decode(self, input: &[u8], output: &mut [u8]) -> Result<usize, Error> {
        // Small whitespace-bearing inputs would otherwise run a strict decode
        // over the raw prefix, fail at the first whitespace byte and then redo
        // the work after filtering. Short inputs go straight to the staging
        // path; longer ones probe the leading bytes first so whitespace-free
        // payloads still use the strict fast path.
        if self.ignore_whitespace && input.len() <= 512 {
            return self.decode_whitespace_short(input, output);
        }
        if self.ignore_whitespace
            && input.len() <= 4096
            && input[..input.len().min(256)]
                .iter()
                .any(|&byte| self.whitespace(byte))
        {
            return self.decode_whitespace(input, output);
        }
        #[cfg(target_arch = "x86_64")]
        if input.len() >= 32 && avx2_available() {
            // Rule 2: a block can include the final quartet only when it has
            // no padding. Otherwise leave that quartet to the scalar tail.
            let padded = input.len() >= 2 && input[input.len() - 2..].contains(&b'=');
            let tail = if padded { 4 } else { 0 };
            let bulk_len = input.len().saturating_sub(tail) / 32 * 32;
            if bulk_len < 32 {
                return if self.ignore_whitespace {
                    self.decode_whitespace(input, output)
                } else {
                    self.decode_short(input, output)
                };
            }
            // Decode the at most 35-byte tail once into a small scratch area.
            // This doubles as validation and avoids a separate length pass;
            // errors keep their original byte offsets.
            let mut scratch = [0u8; 32];
            let tail = if self.ignore_whitespace {
                self.decode_whitespace(&input[bulk_len..], &mut scratch)
            } else {
                self.decode_short(&input[bulk_len..], &mut scratch)
            };
            let tail_len = match tail {
                Ok(n) => n,
                // The raw chunk boundary can split a quartet once whitespace
                // shifts the character grid; redo the whole input in that case.
                Err(_) if self.ignore_whitespace => {
                    return self.decode_whitespace(input, output);
                }
                Err(Error::InvalidByte { byte, offset }) => {
                    return Err(Error::InvalidByte {
                        byte,
                        offset: offset + bulk_len,
                    });
                }
                Err(err) => return Err(err),
            };
            let prefix_len = bulk_len / 4 * 3;
            let len = prefix_len + tail_len;
            if output.len() < len {
                if self.ignore_whitespace {
                    return self.decode_whitespace(input, output);
                }
                return Err(Error::OutputTooSmall);
            }
            // SAFETY: runtime detection guarantees AVX2, the input prefix is a
            // whole number of 32-byte blocks and output has exact capacity.
            if let Err(offset) = unsafe { avx2::decode(&input[..bulk_len], output, self.alphabet) }
            {
                if self.ignore_whitespace {
                    return self.decode_whitespace(input, output);
                }
                return if input[offset] == b'=' {
                    Err(Error::InvalidPadding)
                } else {
                    Err(Error::InvalidByte {
                        byte: input[offset],
                        offset,
                    })
                };
            }
            // The capacity check above covers `len`; the scratch area holds
            // exactly the tail's decoded bytes.
            output[prefix_len..len].copy_from_slice(&scratch[..tail_len]);
            return Ok(len);
        }
        #[cfg(target_arch = "aarch64")]
        if input.len() >= 64 && neon_available() {
            let bulk_len = input.len().saturating_sub(4) / 16 * 16;
            let tail = self.validate(&input[bulk_len..]).map_err(|err| match err {
                Error::InvalidByte { byte, offset } => Error::InvalidByte {
                    byte,
                    offset: offset + bulk_len,
                },
                other => other,
            });
            let tail_len = match tail {
                Ok(n) => n,
                Err(_) if self.ignore_whitespace => return self.decode_whitespace(input, output),
                Err(err) => return Err(err),
            };
            let prefix_len = bulk_len / 4 * 3;
            let len = prefix_len + tail_len;
            if output.len() < len {
                if self.ignore_whitespace {
                    return self.decode_whitespace(input, output);
                }
                return Err(Error::OutputTooSmall);
            }
            // SAFETY: runtime detection guarantees NEON, the input prefix has
            // whole 16-byte blocks and output has sufficient capacity.
            if let Err(offset) = unsafe { neon::decode(&input[..bulk_len], output, self.alphabet) }
            {
                if self.ignore_whitespace {
                    return self.decode_whitespace(input, output);
                }
                return if input[offset] == b'=' {
                    Err(Error::InvalidPadding)
                } else {
                    Err(Error::InvalidByte {
                        byte: input[offset],
                        offset,
                    })
                };
            }
            self.decode(&input[bulk_len..], &mut output[prefix_len..])?;
            return Ok(len);
        }
        if !self.ignore_whitespace {
            self.decode_short(input, output)
        } else {
            self.decode_scalar(input, output)
        }
    }

    fn decode_short(self, input: &[u8], output: &mut [u8]) -> Result<usize, Error> {
        // Rule 1: one table lookup per byte replaces the range-match chain for
        // short, whitespace-free input. Anything unusual (including malformed
        // padding) falls back so error variants and offsets stay identical.
        let table = match self.alphabet {
            Alphabet::Standard => &STANDARD_DECODE_TABLE,
            Alphabet::UrlSafe => &URL_DECODE_TABLE,
        };
        let mut end = input.len();
        let mut pads = 0usize;
        while end > 0 && input[end - 1] == b'=' {
            pads += 1;
            end -= 1;
            if pads > 2 {
                return self.decode_short_slow(input, output);
            }
        }
        if (pads != 0 && !self.padding) || end % 4 == 1 {
            return self.decode_short_slow(input, output);
        }
        if self.padding {
            let expected = match end % 4 {
                0 => 0,
                2 => 2,
                3 => 1,
                _ => return self.decode_short_slow(input, output),
            };
            if pads != expected {
                return self.decode_short_slow(input, output);
            }
        }
        let rem = end % 4;
        let len = end / 4 * 3
            + match rem {
                2 => 1,
                3 => 2,
                _ => 0,
            };
        if output.len() < len {
            return Err(Error::OutputTooSmall);
        }
        // Rule 1: four lookups produce 24 bits; the three significant bytes
        // are written big-endian. Validation is fused into this loop. A byte
        // outside the alphabet produces 0xff, so one OR detects it; malformed
        // input then takes the slow path to keep exact variants and offsets.
        let mut at = 0;
        let mut out = 0;
        while at + 4 <= end {
            let a = table[input[at] as usize];
            let b = table[input[at + 1] as usize];
            let c = table[input[at + 2] as usize];
            let d = table[input[at + 3] as usize];
            if (a | b | c | d) == 0xff {
                return self.decode_short_slow(input, output);
            }
            let (a, b, c, d) = (u32::from(a), u32::from(b), u32::from(c), u32::from(d));
            let bits = a << 18 | b << 12 | c << 6 | d;
            output[out] = (bits >> 16) as u8;
            output[out + 1] = (bits >> 8) as u8;
            output[out + 2] = bits as u8;
            at += 4;
            out += 3;
        }
        if rem == 2 {
            let a = table[input[at] as usize];
            let b = table[input[at + 1] as usize];
            if (a | b) == 0xff || b & 15 != 0 {
                return self.decode_short_slow(input, output);
            }
            output[out] = (u32::from(a) << 2 | u32::from(b) >> 4) as u8;
        } else if rem == 3 {
            let a = table[input[at] as usize];
            let b = table[input[at + 1] as usize];
            let c = table[input[at + 2] as usize];
            if (a | b | c) == 0xff || c & 3 != 0 {
                return self.decode_short_slow(input, output);
            }
            output[out] = (u32::from(a) << 2 | u32::from(b) >> 4) as u8;
            output[out + 1] = (u32::from(b) << 4 | u32::from(c) >> 2) as u8;
        }
        Ok(len)
    }

    /// Careful path for malformed input: preserves the exact error variant
    /// and original byte offset.
    fn decode_short_slow(self, input: &[u8], output: &mut [u8]) -> Result<usize, Error> {
        let prefix = input.len().saturating_sub(1) / 4 * 4;
        let tail_len = self.validate(&input[prefix..]).map_err(|e| match e {
            Error::InvalidByte { byte, offset } => Error::InvalidByte {
                byte,
                offset: prefix + offset,
            },
            other => other,
        })?;
        let len = prefix / 4 * 3 + tail_len;
        if output.len() < len {
            return Err(Error::OutputTooSmall);
        }
        for (i, chunk) in input[..prefix].as_chunks::<4>().0.iter().enumerate() {
            let p = i * 4;
            let a = self.value(chunk[0], p)?;
            let b = self.value(chunk[1], p + 1)?;
            let c = self.value(chunk[2], p + 2)?;
            let d = self.value(chunk[3], p + 3)?;
            let o = i * 3;
            output[o] = a << 2 | b >> 4;
            output[o + 1] = b << 4 | c >> 2;
            output[o + 2] = c << 6 | d;
        }
        self.decode_scalar(&input[prefix..], &mut output[prefix / 4 * 3..])?;
        Ok(len)
    }

    fn decode_whitespace_short(self, input: &[u8], output: &mut [u8]) -> Result<usize, Error> {
        // A common short MIME value has only trailing CRLF. Avoid copying its
        // payload; any interior whitespace or error falls back to filtering.
        if input.len() < 64 && input.last().is_some_and(|&b| self.whitespace(b)) {
            let mut end = input.len();
            while end > 0 && self.whitespace(input[end - 1]) {
                end -= 1;
            }
            let strict = Codec {
                ignore_whitespace: false,
                ..self
            };
            if let Ok(n) = strict.decode(&input[..end], output) {
                return Ok(n);
            }
        }
        let mut buf = [MaybeUninit::<u8>::uninit(); 512];
        #[cfg(target_arch = "x86_64")]
        let filtered = if input.len() >= 64 && avx2_available() {
            // SAFETY: runtime detection guarantees AVX2, and the staging
            // buffer is large enough for the entire input.
            Some(unsafe { avx2::filter_whitespace(input, &mut buf) }.1)
        } else {
            None
        };
        #[cfg(not(target_arch = "x86_64"))]
        let filtered: Option<usize> = None;
        let used = filtered.unwrap_or_else(|| {
            let mut used = 0;
            for &byte in input {
                if !self.whitespace(byte) {
                    buf[used].write(byte);
                    used += 1;
                }
            }
            used
        });
        // SAFETY: every element in the prefix was initialized by the loop;
        // the caller restricts input length to the size of the buffer.
        let filled = unsafe { std::slice::from_raw_parts(buf.as_ptr().cast::<u8>(), used) };
        let strict = Codec {
            ignore_whitespace: false,
            ..self
        };
        match strict.decode(filled, output) {
            Ok(n) => Ok(n),
            Err(_) => self.decode_scalar(input, output),
        }
    }

    /// Strip whitespace in bounded blocks; invalid input is rechecked against
    /// the original bytes to preserve the original error offsets.
    fn decode_whitespace(self, input: &[u8], output: &mut [u8]) -> Result<usize, Error> {
        // Rule 4: a 4 KiB staging buffer stays comfortably within L1 cache.
        // Its performance is benchmarked against the existing codec plus a
        // reused filtering buffer; the constant is still provisional.
        // The buffer is uninitialized on purpose: only the written prefix is
        // ever read, and zeroing 4 KiB per call showed up as a large fixed
        // cost for short inputs in the disassembly. The 64-byte alignment
        // keeps the SIMD decoder reading whole cache lines.
        #[repr(align(64))]
        struct Staging {
            bytes: [MaybeUninit<u8>; 4096],
        }
        let mut staging = Staging {
            bytes: [MaybeUninit::<u8>::uninit(); 4096],
        };
        let buf = &mut staging.bytes;
        let (mut used, mut out) = (0usize, 0usize);
        let strict = Codec {
            ignore_whitespace: false,
            ..self
        };
        let mut remaining = input;
        while !remaining.is_empty() {
            #[cfg(target_arch = "x86_64")]
            let filtered = if remaining.len() >= 256 && avx2_available() {
                // SAFETY: runtime detection guarantees AVX2; the filter writes
                // at most the supplied space and initializes its output prefix.
                Some(unsafe { avx2::filter_whitespace(remaining, &mut buf[used..]) })
            } else {
                None
            };
            #[cfg(not(target_arch = "x86_64"))]
            let filtered: Option<(usize, usize)> = None;
            let (consumed, added) = filtered.unwrap_or_else(|| {
                let count = remaining.len().min(buf.len() - used);
                let mut added = 0;
                for &byte in &remaining[..count] {
                    if !self.whitespace(byte) {
                        buf[used + added].write(byte);
                        added += 1;
                    }
                }
                (count, added)
            });
            remaining = &remaining[consumed..];
            used += added;
            if used == buf.len() {
                // Keep the last quartet: padding is only valid at the end.
                let prefix = buf.len() - 4;
                let need = prefix / 4 * 3;
                if output.len() - out < need {
                    return self.decode_scalar(input, output);
                }
                // SAFETY: the loop wrote buf[0..used] exactly, and used is
                // the full length here, so the whole prefix is initialized.
                let filled =
                    unsafe { std::slice::from_raw_parts(buf.as_ptr().cast::<u8>(), prefix) };
                match strict.decode(filled, &mut output[out..]) {
                    Ok(n) => out += n,
                    Err(_) => return self.decode_scalar(input, output),
                }
                buf.copy_within(prefix.., 0);
                used = 4;
            }
        }
        // SAFETY: the loop wrote buf[0..used] exactly.
        let filled = unsafe { std::slice::from_raw_parts(buf.as_ptr().cast::<u8>(), used) };
        match strict.decode(filled, &mut output[out..]) {
            Ok(n) => Ok(out + n),
            Err(_) => self.decode_scalar(input, output),
        }
    }

    fn decode_scalar(self, input: &[u8], output: &mut [u8]) -> Result<usize, Error> {
        let len = self.validate(input)?;
        if output.len() < len {
            return Err(Error::OutputTooSmall);
        }
        let (mut acc, mut bits, mut out) = (0u32, 0u32, 0usize);
        for (offset, &b) in input.iter().enumerate() {
            if self.whitespace(b) {
                continue;
            }
            if b == b'=' {
                break;
            }
            acc = (acc << 6) | u32::from(self.value(b, offset)?);
            bits += 6;
            if bits >= 8 {
                bits -= 8;
                output[out] = (acc >> bits) as u8;
                out += 1;
                acc &= (1 << bits) - 1;
            }
        }
        Ok(out)
    }

    /// Decode to a new buffer.
    pub fn decode_to_vec(self, input: &[u8]) -> Result<Vec<u8>, Error> {
        let len = self.allocation_len(input)?;
        let mut out = vec![0; len];
        let written = self.decode(input, &mut out)?;
        out.truncate(written);
        Ok(out)
    }

    /// Append decoded input to an existing buffer.
    pub fn append_decode(self, input: &[u8], output: &mut Vec<u8>) -> Result<usize, Error> {
        let len = self.allocation_len(input)?;
        let start = output.len();
        let end = start.checked_add(len).ok_or(Error::InvalidLength)?;
        output.resize(end, 0);
        match self.decode(input, &mut output[start..]) {
            Ok(written) => {
                output.truncate(start + written);
                Ok(written)
            }
            Err(err) => {
                output.truncate(start);
                Err(err)
            }
        }
    }
}

/// Encode a data URL using standard padded Base64. The MIME type is not validated.
pub fn encode_data_url(mime: &str, input: &[u8]) -> Result<String, Error> {
    let len = 5usize
        .checked_add(mime.len())
        .and_then(|n| n.checked_add(8))
        .and_then(|n| n.checked_add(STANDARD.encoded_len(input.len())?))
        .ok_or(Error::InvalidLength)?;
    let mut out = Vec::with_capacity(len);
    out.extend_from_slice(b"data:");
    out.extend_from_slice(mime.as_bytes());
    out.extend_from_slice(b";base64,");
    STANDARD.append_encode(input, &mut out)?;
    String::from_utf8(out).map_err(|_| Error::InvalidLength)
}

/// Incremental decoder. Output produced before an error is not rolled back;
/// callers must discard it if the complete message must be valid.
///
/// Offsets in `InvalidByte` refer to the original input across all chunks,
/// including ignored whitespace. After any error the decoder cannot be reused.
pub struct Decoder {
    codec: Codec,
    group: [u8; 4],
    used: usize,
    offset: usize,
    ended: bool,
    failed: bool,
}

impl Decoder {
    /// Create a decoder with the same alphabet, padding and whitespace rules
    /// as the non-streaming codec.
    pub fn new(codec: Codec) -> Self {
        Self {
            codec,
            group: [0; 4],
            used: 0,
            offset: 0,
            ended: false,
            failed: false,
        }
    }

    /// Consume a chunk, appending decoded complete groups to `output`.
    pub fn feed(&mut self, input: &[u8], output: &mut Vec<u8>) -> Result<(), Error> {
        if self.failed {
            return Err(Error::InvalidLength);
        }
        let Some(end) = self.offset.checked_add(input.len()) else {
            self.failed = true;
            return Err(Error::InvalidLength);
        };
        for (i, &byte) in input.iter().enumerate() {
            if self.codec.whitespace(byte) {
                continue;
            }
            let offset = self.offset + i;
            if self.ended {
                self.failed = true;
                return Err(Error::InvalidPadding);
            }
            if byte != b'='
                && let Err(err) = self.codec.value(byte, offset)
            {
                self.failed = true;
                return Err(err);
            }
            self.group[self.used] = byte;
            self.used += 1;
            if self.used == 4 {
                let mut decoded = [0; 3];
                match self.codec.decode(&self.group, &mut decoded) {
                    Ok(n) => output.extend_from_slice(&decoded[..n]),
                    Err(err) => {
                        self.failed = true;
                        return Err(err);
                    }
                }
                self.ended = self.group.contains(&b'=');
                self.used = 0;
            }
        }
        self.offset = end;
        Ok(())
    }

    /// Finish the message, validating its final group and padding.
    pub fn finish(mut self, output: &mut Vec<u8>) -> Result<(), Error> {
        if self.failed {
            return Err(Error::InvalidLength);
        }
        if self.used != 0 {
            let mut decoded = [0; 3];
            let n = self.codec.decode(&self.group[..self.used], &mut decoded)?;
            output.extend_from_slice(&decoded[..n]);
            self.used = 0;
        }
        Ok(())
    }
}
