//! AArch64 NEON bulk kernels; tails are left to the portable codec.
use crate::Alphabet;
use std::arch::aarch64::*;

/// # Safety
/// The CPU must support NEON, and `output` must fit every encoded group.
#[target_feature(enable = "neon")]
pub(crate) unsafe fn encode(input: &[u8], output: &mut [u8], alphabet: Alphabet) -> usize {
    let mut at = 0;
    while input.len().saturating_sub(at) >= 16 {
        // SAFETY: input has at least 16 bytes; output has 16 bytes per
        // 12-byte group. NEON support is a precondition of this function.
        unsafe {
            let bytes = vld1q_u8(input.as_ptr().add(at));
            let indices = [0, 0, 1, 2, 3, 3, 4, 5, 6, 6, 7, 8, 9, 9, 10, 11];
            // Rule 1: shuffle bytes into independent 24-bit groups, then
            // extract their four six-bit digits with shifts and masks.
            let x = vreinterpretq_u32_u8(vqtbl1q_u8(bytes, vld1q_u8(indices.as_ptr())));
            let a = vandq_u32(vshrq_n_u32::<2>(x), vdupq_n_u32(0x0000003f));
            let b = vorrq_u32(
                vandq_u32(vshlq_n_u32::<4>(x), vdupq_n_u32(0x00003000)),
                vandq_u32(vshrq_n_u32::<12>(x), vdupq_n_u32(0x00000f00)),
            );
            let c = vorrq_u32(
                vandq_u32(vshlq_n_u32::<2>(x), vdupq_n_u32(0x003c0000)),
                vandq_u32(vshrq_n_u32::<14>(x), vdupq_n_u32(0x00030000)),
            );
            let d = vandq_u32(x, vdupq_n_u32(0x3f000000));
            let values = vreinterpretq_u8_u32(vorrq_u32(vorrq_u32(a, b), vorrq_u32(c, d)));
            let gt25 = vcgtq_u8(values, vdupq_n_u8(25));
            let gt51 = vcgtq_u8(values, vdupq_n_u8(51));
            let gt61 = vcgtq_u8(values, vdupq_n_u8(61));
            let gt62 = vcgtq_u8(values, vdupq_n_u8(62));
            let mut result = vaddq_u8(values, vdupq_n_u8(65));
            result = vaddq_u8(result, vandq_u8(gt25, vdupq_n_u8(6)));
            result = vsubq_u8(result, vandq_u8(gt51, vdupq_n_u8(75)));
            let (adjust62, adjust63) = match alphabet {
                Alphabet::Standard => (15, 3),
                Alphabet::UrlSafe => (13, 49),
            };
            result = vsubq_u8(result, vandq_u8(gt61, vdupq_n_u8(adjust62)));
            result = vaddq_u8(result, vandq_u8(gt62, vdupq_n_u8(adjust63)));
            vst1q_u8(output.as_mut_ptr().add(at / 12 * 16), result);
        }
        at += 12;
    }
    at
}

/// # Safety
/// The CPU must support NEON, and output must fit 12 bytes per input block.
#[target_feature(enable = "neon")]
pub(crate) unsafe fn decode(
    input: &[u8],
    output: &mut [u8],
    alphabet: Alphabet,
) -> Result<usize, usize> {
    let blocks = input.len() / 16;
    for block in 0..blocks {
        // SAFETY: input and output bounds follow from the number of complete
        // blocks and the caller's output-length contract. NEON was detected.
        unsafe {
            let bytes = vld1q_u8(input.as_ptr().add(block * 16));
            let range = |lo, hi| {
                vandq_u8(
                    vcgeq_u8(bytes, vdupq_n_u8(lo)),
                    vcleq_u8(bytes, vdupq_n_u8(hi)),
                )
            };
            let upper = range(b'A', b'Z');
            let lower = range(b'a', b'z');
            let digit = range(b'0', b'9');
            let (c62, c63) = match alphabet {
                Alphabet::Standard => (b'+', b'/'),
                Alphabet::UrlSafe => (b'-', b'_'),
            };
            let is62 = vceqq_u8(bytes, vdupq_n_u8(c62));
            let is63 = vceqq_u8(bytes, vdupq_n_u8(c63));
            let valid = vorrq_u8(
                vorrq_u8(upper, lower),
                vorrq_u8(digit, vorrq_u8(is62, is63)),
            );
            // Rule 3: detect the common valid block in parallel; locate errors
            // with a scalar scan only when the vector contains an invalid byte.
            if vminvq_u8(valid) != 255 {
                let mut masks = [0u8; 16];
                vst1q_u8(masks.as_mut_ptr(), valid);
                return Err(block * 16 + masks.iter().position(|&b| b == 0).unwrap_or(0));
            }
            // Rule 1: parallel interval mapping; group values into bytes.
            let values = vorrq_u8(
                vorrq_u8(
                    vandq_u8(upper, vsubq_u8(bytes, vdupq_n_u8(b'A'))),
                    vandq_u8(lower, vsubq_u8(bytes, vdupq_n_u8(b'a' - 26))),
                ),
                vorrq_u8(
                    vandq_u8(digit, vaddq_u8(bytes, vdupq_n_u8(4))),
                    vorrq_u8(
                        vandq_u8(is62, vdupq_n_u8(62)),
                        vandq_u8(is63, vdupq_n_u8(63)),
                    ),
                ),
            );
            let mut digits = [0u8; 16];
            vst1q_u8(digits.as_mut_ptr(), values);
            for group in 0..4 {
                let i = group * 4;
                let o = block * 12 + group * 3;
                output[o] = digits[i] << 2 | digits[i + 1] >> 4;
                output[o + 1] = digits[i + 1] << 4 | digits[i + 2] >> 2;
                output[o + 2] = digits[i + 2] << 6 | digits[i + 3];
            }
        }
    }
    Ok(blocks * 16)
}
