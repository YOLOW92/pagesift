//! AVX2 bulk decoder. All loads and stores are bounded; the caller handles tails.

use crate::Alphabet;
use std::arch::asm;
#[cfg(target_os = "linux")]
use std::arch::global_asm;
use std::arch::x86_64::*;
use std::mem::MaybeUninit;

/// Copies a prefix into `output`, omitting space, tab, CR and LF.
///
/// # Safety
/// AVX2 must be available. The returned output prefix is initialized.
#[target_feature(enable = "avx2")]
pub(crate) unsafe fn filter_whitespace(
    input: &[u8],
    output: &mut [MaybeUninit<u8>],
) -> (usize, usize) {
    // Consuming at most output.len() bytes guarantees room even if no bytes
    // are removed; input and output are disjoint safe references.
    let count = input.len().min(output.len());
    let (mut read, mut written) = (0, 0);
    let space = _mm256_set1_epi8(b' ' as i8);
    let tab = _mm256_set1_epi8(b'\t' as i8);
    let cr = _mm256_set1_epi8(b'\r' as i8);
    let lf = _mm256_set1_epi8(b'\n' as i8);
    while count - read >= 32 {
        // SAFETY: the loop ensures 32 readable input bytes remain.
        let bytes = unsafe { _mm256_loadu_si256(input.as_ptr().add(read).cast()) };
        let matches = _mm256_or_si256(
            _mm256_or_si256(
                _mm256_cmpeq_epi8(bytes, space),
                _mm256_cmpeq_epi8(bytes, tab),
            ),
            _mm256_or_si256(_mm256_cmpeq_epi8(bytes, cr), _mm256_cmpeq_epi8(bytes, lf)),
        );
        let mut mask = _mm256_movemask_epi8(matches) as u32;
        if mask == 0 {
            // SAFETY: at most `read` bytes were written so far, and the
            // 32-byte destination lies within the `count`-byte output prefix.
            unsafe { _mm256_storeu_si256(output.as_mut_ptr().add(written).cast(), bytes) };
            written += 32;
        } else {
            let mut start = 0;
            while mask != 0 {
                let end = mask.trailing_zeros() as usize;
                let len = end - start;
                // SAFETY: both ranges fit inside the current 32-byte input
                // block and the output prefix; the slices cannot overlap.
                unsafe {
                    std::ptr::copy_nonoverlapping(
                        input.as_ptr().add(read + start),
                        output.as_mut_ptr().add(written).cast::<u8>(),
                        len,
                    );
                }
                written += len;
                start = end + 1;
                mask &= mask - 1;
            }
            let len = 32 - start;
            // SAFETY: the remaining suffix is within the current block and
            // the output prefix; the slices cannot overlap.
            unsafe {
                std::ptr::copy_nonoverlapping(
                    input.as_ptr().add(read + start),
                    output.as_mut_ptr().add(written).cast::<u8>(),
                    len,
                );
            }
            written += len;
        }
        read += 32;
    }
    for &byte in &input[read..count] {
        if !matches!(byte, b' ' | b'\t' | b'\r' | b'\n') {
            output[written].write(byte);
            written += 1;
        }
    }
    (count, written)
}

/// LLVM 22 lowered the `_mm256_mulhi_epu16` intrinsic on this target into
/// widening, packing and lane permutation. The instruction exists, so pin it.
///
/// # Safety
/// AVX2 must be available.
#[inline]
#[target_feature(enable = "avx2")]
unsafe fn mulhi_epu16(a: __m256i, b: __m256i) -> __m256i {
    let out: __m256i;
    // SAFETY: the caller guarantees AVX2; this instruction has no memory
    // operands, reads only the two input registers and writes one register.
    // Rust's inline assembly defaults to Intel syntax: the destination comes
    // first. Tie it to the first input so it cannot be mistaken for a source.
    unsafe {
        asm!(
            "vpmulhuw {a}, {b}, {a}",
            a = inout(ymm_reg) a => out,
            b = in(ymm_reg) b,
            options(pure, nomem, nostack, preserves_flags)
        );
    }
    out
}

const fn membership(url: bool) -> [u8; 16] {
    let mut table = [0; 16];
    let mut byte = 0u8;
    while byte < 128 {
        if (byte >= b'A' && byte <= b'Z')
            || (byte >= b'a' && byte <= b'z')
            || (byte >= b'0' && byte <= b'9')
            || (!url && (byte == b'+' || byte == b'/'))
            || (url && (byte == b'-' || byte == b'_'))
        {
            table[(byte & 15) as usize] |= 1 << (byte >> 4);
        }
        byte += 1;
    }
    table
}
const STANDARD_MEMBERSHIP: [u8; 16] = membership(false);
const URL_MEMBERSHIP: [u8; 16] = membership(true);

#[cfg(target_os = "linux")]
const fn repeat16(bytes: [u8; 16]) -> [u8; 32] {
    let mut out = [0; 32];
    let mut i = 0;
    while i < 16 {
        out[i] = bytes[i];
        out[i + 16] = bytes[i];
        i += 1;
    }
    out
}

/// Constants consumed by the assembly bulk loop. Offsets are part of that
/// loop's interface; keep the field order when editing.
#[repr(C, align(16))]
#[cfg(target_os = "linux")]
struct DecodeParams {
    low: [u8; 16],
    roll: [u8; 16],
    high: [u8; 16],
    split: u8,
    nibble: u8,
    _pad: [u8; 14],
    pair: [u8; 32],
    quad: [u8; 32],
    pack: [u8; 32],
}

#[cfg(target_os = "linux")]
const fn decode_params(url: bool) -> DecodeParams {
    let mut pair = [0u8; 32];
    let mut quad = [0u8; 32];
    let mut i = 0;
    while i < 16 {
        pair[i * 2] = 0x40;
        pair[i * 2 + 1] = 0x01;
        i += 1;
    }
    let mut i = 0;
    while i < 8 {
        quad[i * 4] = 0x00;
        quad[i * 4 + 1] = 0x10;
        quad[i * 4 + 2] = 0x01;
        quad[i * 4 + 3] = 0x00;
        i += 1;
    }
    DecodeParams {
        low: if url {
            URL_MEMBERSHIP
        } else {
            STANDARD_MEMBERSHIP
        },
        roll: if url {
            [0, 0, 17, 4, 191, 191, 224, 185, 185, 0, 0, 0, 0, 0, 0, 0]
        } else {
            [0, 0, 19, 16, 4, 191, 191, 185, 185, 0, 0, 0, 0, 0, 0, 0]
        },
        high: [1, 2, 4, 8, 16, 32, 64, 128, 0, 0, 0, 0, 0, 0, 0, 0],
        split: if url { 94 } else { 46 },
        nibble: 15,
        _pad: [0; 14],
        pair,
        quad,
        pack: repeat16([2, 1, 0, 6, 5, 4, 10, 9, 8, 14, 13, 12, 128, 128, 128, 128]),
    }
}

#[cfg(target_os = "linux")]
static STANDARD_PARAMS: DecodeParams = decode_params(false);
#[cfg(target_os = "linux")]
static URL_PARAMS: DecodeParams = decode_params(true);

#[cfg(target_os = "linux")]
unsafe extern "C" {
    /// Decode `groups` groups of four 32-byte blocks. Returns `usize::MAX` on
    /// success, otherwise the offset of the first non-alphabet byte. The
    /// caller must leave at least four more blocks after the region so the
    /// trailing four-byte store spill lands in valid output.
    fn pagesift_base64_decode_groups4(
        src: *const u8,
        dst: *mut u8,
        groups: usize,
        params: *const u8,
    ) -> usize;
}

// Hand-allocated AVX2 inner loop. The Rust loop could not fold the multiply
// and shuffle constants into memory operands without spilling, so this keeps
// the same register discipline as the reference implementation.
#[cfg(target_os = "linux")]
global_asm!(
    r#"
    .globl pagesift_base64_decode_groups4
    .hidden pagesift_base64_decode_groups4
    .type pagesift_base64_decode_groups4,@function
    .p2align 5
pagesift_base64_decode_groups4:
    mov %rdi, %r9
    test %rdx, %rdx
    jz .Ldone
    vbroadcasti128 0(%rcx), %ymm14
    vbroadcasti128 16(%rcx), %ymm12
    vbroadcasti128 32(%rcx), %ymm13
    vpbroadcastb 48(%rcx), %ymm11
    vpbroadcastb 49(%rcx), %ymm15
    vmovdqu (%rdi), %ymm0
    vmovdqu 32(%rdi), %ymm1
    vpsrld $4, %ymm0, %ymm4
    vpsrld $4, %ymm1, %ymm5
    vpand %ymm15, %ymm4, %ymm4
    vpand %ymm15, %ymm5, %ymm5
    vpshufb %ymm0, %ymm14, %ymm6
    vpshufb %ymm1, %ymm14, %ymm7
    vpshufb %ymm4, %ymm13, %ymm8
    vpshufb %ymm5, %ymm13, %ymm9
    vpand %ymm8, %ymm6, %ymm6
    vpand %ymm9, %ymm7, %ymm7
    vpminub %ymm7, %ymm6, %ymm6
    vpxor %ymm7, %ymm7, %ymm7
    vpcmpeqb %ymm7, %ymm6, %ymm6
    vpmovmskb %ymm6, %eax
    test %eax, %eax
    jnz .LfailA
    vpcmpgtb %ymm11, %ymm0, %ymm6
    vpcmpgtb %ymm11, %ymm1, %ymm7
    vpsubb %ymm6, %ymm4, %ymm4
    vpsubb %ymm7, %ymm5, %ymm5
    vpshufb %ymm4, %ymm12, %ymm4
    vpshufb %ymm5, %ymm12, %ymm5
    vpaddb %ymm4, %ymm0, %ymm0
    vpaddb %ymm5, %ymm1, %ymm1
    .p2align 5
.Lloop:
    prefetcht0 1024(%rdi)
    prefetcht0 1088(%rdi)
    vmovdqu 64(%rdi), %ymm2
    vmovdqu 96(%rdi), %ymm3
    vpsrld $4, %ymm2, %ymm4
    vpsrld $4, %ymm3, %ymm5
    vpand %ymm15, %ymm4, %ymm4
    vpand %ymm15, %ymm5, %ymm5
    vpshufb %ymm2, %ymm14, %ymm6
    vpshufb %ymm3, %ymm14, %ymm7
    vpshufb %ymm4, %ymm13, %ymm8
    vpshufb %ymm5, %ymm13, %ymm9
    vpand %ymm8, %ymm6, %ymm6
    vpand %ymm9, %ymm7, %ymm7
    vpminub %ymm7, %ymm6, %ymm6
    vpxor %ymm7, %ymm7, %ymm7
    vpcmpeqb %ymm7, %ymm6, %ymm6
    vpmovmskb %ymm6, %eax
    test %eax, %eax
    jnz .LfailB
    vpcmpgtb %ymm11, %ymm2, %ymm6
    vpcmpgtb %ymm11, %ymm3, %ymm7
    vpsubb %ymm6, %ymm4, %ymm4
    vpsubb %ymm7, %ymm5, %ymm5
    vpshufb %ymm4, %ymm12, %ymm4
    vpshufb %ymm5, %ymm12, %ymm5
    vpaddb %ymm4, %ymm2, %ymm2
    vpaddb %ymm5, %ymm3, %ymm3
    vpmaddubsw 64(%rcx), %ymm0, %ymm0
    vpmaddwd 96(%rcx), %ymm0, %ymm0
    vpshufb 128(%rcx), %ymm0, %ymm0
    vmovdqu %xmm0, (%rsi)
    vextracti128 $1, %ymm0, 12(%rsi)
    vpmaddubsw 64(%rcx), %ymm1, %ymm1
    vpmaddwd 96(%rcx), %ymm1, %ymm1
    vpshufb 128(%rcx), %ymm1, %ymm1
    vmovdqu %xmm1, 24(%rsi)
    vextracti128 $1, %ymm1, 36(%rsi)
    vmovdqu 128(%rdi), %ymm0
    vmovdqu 160(%rdi), %ymm1
    vpsrld $4, %ymm0, %ymm4
    vpsrld $4, %ymm1, %ymm5
    vpand %ymm15, %ymm4, %ymm4
    vpand %ymm15, %ymm5, %ymm5
    vpshufb %ymm0, %ymm14, %ymm6
    vpshufb %ymm1, %ymm14, %ymm7
    vpshufb %ymm4, %ymm13, %ymm8
    vpshufb %ymm5, %ymm13, %ymm9
    vpand %ymm8, %ymm6, %ymm6
    vpand %ymm9, %ymm7, %ymm7
    vpminub %ymm7, %ymm6, %ymm6
    vpxor %ymm7, %ymm7, %ymm7
    vpcmpeqb %ymm7, %ymm6, %ymm6
    vpmovmskb %ymm6, %eax
    test %eax, %eax
    jnz .LfailC
    vpcmpgtb %ymm11, %ymm0, %ymm6
    vpcmpgtb %ymm11, %ymm1, %ymm7
    vpsubb %ymm6, %ymm4, %ymm4
    vpsubb %ymm7, %ymm5, %ymm5
    vpshufb %ymm4, %ymm12, %ymm4
    vpshufb %ymm5, %ymm12, %ymm5
    vpaddb %ymm4, %ymm0, %ymm0
    vpaddb %ymm5, %ymm1, %ymm1
    vpmaddubsw 64(%rcx), %ymm2, %ymm2
    vpmaddwd 96(%rcx), %ymm2, %ymm2
    vpshufb 128(%rcx), %ymm2, %ymm2
    vmovdqu %xmm2, 48(%rsi)
    vextracti128 $1, %ymm2, 60(%rsi)
    vpmaddubsw 64(%rcx), %ymm3, %ymm3
    vpmaddwd 96(%rcx), %ymm3, %ymm3
    vpshufb 128(%rcx), %ymm3, %ymm3
    vmovdqu %xmm3, 72(%rsi)
    vextracti128 $1, %ymm3, 84(%rsi)
    add $128, %rdi
    add $96, %rsi
    dec %rdx
    jnz .Lloop
.Ldone:
    mov $-1, %rax
    vzeroupper
    ret
.LfailA:
    xor %r8d, %r8d
    jmp .Lfail_pair
.LfailB:
    vmovdqa %ymm2, %ymm0
    vmovdqa %ymm3, %ymm1
    mov $64, %r8d
    jmp .Lfail_pair
.LfailC:
    mov $128, %r8d
.Lfail_pair:
    mov %rdi, %r10
    sub %r9, %r10
    add %r8, %r10
    vpsrld $4, %ymm0, %ymm4
    vpand %ymm15, %ymm4, %ymm4
    vpshufb %ymm0, %ymm14, %ymm6
    vpshufb %ymm4, %ymm13, %ymm7
    vpand %ymm7, %ymm6, %ymm6
    vpxor %ymm7, %ymm7, %ymm7
    vpcmpeqb %ymm7, %ymm6, %ymm6
    vpmovmskb %ymm6, %eax
    test %eax, %eax
    jnz .Lfound
    vpsrld $4, %ymm1, %ymm4
    vpand %ymm15, %ymm4, %ymm4
    vpshufb %ymm1, %ymm14, %ymm6
    vpshufb %ymm4, %ymm13, %ymm7
    vpand %ymm7, %ymm6, %ymm6
    vpxor %ymm7, %ymm7, %ymm7
    vpcmpeqb %ymm7, %ymm6, %ymm6
    vpmovmskb %ymm6, %eax
    add $32, %r10
.Lfound:
    bsf %eax, %eax
    add %r10, %rax
    vzeroupper
    ret
    .size pagesift_base64_decode_groups4, .-pagesift_base64_decode_groups4
"#,
    options(att_syntax)
);

/// Encode complete 24-byte groups; the caller handles the remainder.
///
/// # Safety
/// The processor must support AVX2, and output must accommodate 32 bytes for
/// each complete 24-byte group processed.
#[target_feature(enable = "avx2")]
pub(crate) unsafe fn encode(input: &[u8], output: &mut [u8], alphabet: Alphabet) -> usize {
    let mut at = 0;
    let mut out = 0;
    if input.len() < 24 {
        return 0;
    }
    let limit = input.len() - 24;
    // LLVM otherwise expands constant multiplications into a longer sequence
    // of widening, variable shifts and packing on the tested toolchain.
    let mul_high = std::hint::black_box(_mm256_set1_epi32(0x04000040));
    let mul_low = std::hint::black_box(_mm256_set1_epi32(0x01000010));
    let (s62, s63) = match alphabet {
        Alphabet::Standard => (-19, -16),
        Alphabet::UrlSafe => (-17, 32),
    };
    let offsets = _mm256_setr_epi8(
        65, 71, -4, -4, -4, -4, -4, -4, -4, -4, -4, -4, s62, s63, 0, 0, 65, 71, -4, -4, -4, -4, -4,
        -4, -4, -4, -4, -4, s62, s63, 0, 0,
    );
    let shifted = _mm256_setr_epi8(
        5, 4, 6, 5, 8, 7, 9, 8, 11, 10, 12, 11, 14, 13, 15, 14, 1, 0, 2, 1, 4, 3, 5, 4, 7, 6, 8, 7,
        10, 9, 11, 10,
    );
    let unshifted = _mm256_setr_epi8(
        1, 0, 2, 1, 4, 3, 5, 4, 7, 6, 8, 7, 10, 9, 11, 10, 5, 4, 6, 5, 8, 7, 9, 8, 11, 10, 12, 11,
        14, 13, 15, 14,
    );
    let mask_high = _mm256_set1_epi32(0x0fc0fc00);
    let mask_low = _mm256_set1_epi32(0x003f_03f0);
    let fifty_one = _mm256_set1_epi8(51);
    let twenty_five = _mm256_set1_epi8(25);
    macro_rules! sextets {
        ($bytes:expr, $shuffle:expr) => {{
            // Rule 1: multiply, shuffle and interval offsets implement the
            // independent 8-bit -> 6-bit repacking in each SIMD lane.
            let x = _mm256_shuffle_epi8($bytes, $shuffle);
            // SAFETY: this kernel only runs after AVX2 detection.
            let high = unsafe { mulhi_epu16(_mm256_and_si256(x, mask_high), mul_high) };
            let low = _mm256_mullo_epi16(_mm256_and_si256(x, mask_low), mul_low);
            _mm256_or_si256(high, low)
        }};
    }
    macro_rules! symbols {
        ($values:expr) => {{
            let class = _mm256_sub_epi8(
                _mm256_subs_epu8($values, fifty_one),
                _mm256_cmpgt_epi8($values, twenty_five),
            );
            _mm256_add_epi8($values, _mm256_shuffle_epi8(offsets, class))
        }};
    }
    macro_rules! convert {
        ($bytes:expr, $shuffle:expr) => {{
            let values = sextets!($bytes, $shuffle);
            symbols!(values)
        }};
    }
    // The first block cannot read four bytes before the input, so it uses two
    // bounded 16-byte loads and an unshifted shuffle. Handling it once also
    // removes the `at == 0` test from the hot loop.
    if input.len() >= 28 {
        // SAFETY: at least 24 bytes remain, so both 16-byte loads fit.
        let lo = unsafe { _mm_loadu_si128(input.as_ptr().cast()) };
        // SAFETY: the second load covers offsets 8 through 23.
        let hi = unsafe { _mm_loadu_si128(input.as_ptr().add(8).cast()) };
        let bytes = _mm256_set_m128i(hi, lo);
        let result = convert!(bytes, unshifted);
        // SAFETY: the caller guarantees room for one complete group.
        unsafe { _mm256_storeu_si256(output.as_mut_ptr().cast(), result) };
        at = 24;
        out = 32;
    }
    // Rule 2: one prefetch per 96 input bytes keeps the source ahead of the
    // hardware streamer once it no longer fits in L1.
    while input.len() - at >= 100 {
        if input.len() - at > 1024 {
            // SAFETY: the prefetch address is inside the input allocation.
            unsafe { _mm_prefetch(input.as_ptr().add(at + 1024).cast(), _MM_HINT_T0) };
        }
        // SAFETY: the four 32-byte windows start at at-4 and 24-byte
        // increments, ending at at+99, which is inside the input.
        let b0 = unsafe { _mm256_loadu_si256(input.as_ptr().add(at - 4).cast()) };
        // SAFETY: the loop condition covers this 32-byte window.
        let b1 = unsafe { _mm256_loadu_si256(input.as_ptr().add(at + 20).cast()) };
        // SAFETY: the loop condition covers this 32-byte window.
        let b2 = unsafe { _mm256_loadu_si256(input.as_ptr().add(at + 44).cast()) };
        // SAFETY: the loop condition covers this 32-byte window.
        let b3 = unsafe { _mm256_loadu_si256(input.as_ptr().add(at + 68).cast()) };
        // Rule 1: compute the multiplies for all four blocks first, then
        // translate them. Keeping the phases adjacent gives the scheduler
        // four independent chains instead of one long serial chain.
        let v0 = sextets!(b0, shifted);
        let v1 = sextets!(b1, shifted);
        let v2 = sextets!(b2, shifted);
        let v3 = sextets!(b3, shifted);
        let r0 = symbols!(v0);
        let r1 = symbols!(v1);
        let r2 = symbols!(v2);
        let r3 = symbols!(v3);
        // SAFETY: four complete 24-byte input blocks imply room for four
        // 32-byte stores by the caller's output-capacity contract.
        unsafe {
            _mm256_storeu_si256(output.as_mut_ptr().add(out).cast(), r0);
            _mm256_storeu_si256(output.as_mut_ptr().add(out + 32).cast(), r1);
            _mm256_storeu_si256(output.as_mut_ptr().add(out + 64).cast(), r2);
            _mm256_storeu_si256(output.as_mut_ptr().add(out + 96).cast(), r3);
        }
        at += 96;
        out += 128;
    }
    while at <= limit {
        if input.len() - at > 1024 {
            // SAFETY: the prefetch address is inside the input allocation.
            unsafe { _mm_prefetch(input.as_ptr().add(at + 1024).cast(), _MM_HINT_T0) };
        }
        let (bytes, shuffle) = if input.len() - at < 28 {
            // SAFETY: at least 24 bytes remain, so both 16-byte loads fit;
            // the second starts at offset 8 within the same 24-byte group.
            let lo = unsafe { _mm_loadu_si128(input.as_ptr().add(at).cast()) };
            // SAFETY: the second load covers offsets 8 through 23.
            let hi = unsafe { _mm_loadu_si128(input.as_ptr().add(at + 8).cast()) };
            (_mm256_set_m128i(hi, lo), unshifted)
        } else {
            // SAFETY: at >= 24 and at least 28 bytes remain; this 32-byte
            // window starts four bytes before at and ends inside the slice.
            let bytes = unsafe { _mm256_loadu_si256(input.as_ptr().add(at - 4).cast()) };
            (bytes, shifted)
        };
        // Rule 1: 8-bit to 6-bit repacking, in parallel per 24-byte group.
        // The arithmetic and table approach is based on moego; see its MIT
        // notice in THIRD-PARTY.md.
        let result = convert!(bytes, shuffle);
        // SAFETY: each iteration consumes 24 bytes and writes 32; the caller
        // guarantees enough output for all complete groups.
        unsafe { _mm256_storeu_si256(output.as_mut_ptr().add(out).cast(), result) };
        at += 24;
        out += 32;
    }
    // Encode the final complete block once more, shifted so its 32-byte store
    // ends exactly at the output for all complete groups. The overlap rewrites
    // identical bytes and replaces up to seven scalar groups with one SIMD
    // block. Both 16-byte loads stay inside the last 24 bytes.
    let groups = (input.len() - at) / 3;
    if groups > 0 && at + groups * 3 >= 24 {
        let end = at + groups * 3;
        let start = end - 24;
        // SAFETY: start >= 0 and the second load covers start+8..start+23,
        // which is inside the input because end <= input.len().
        let lo = unsafe { _mm_loadu_si128(input.as_ptr().add(start).cast()) };
        // SAFETY: the two loads together cover the complete 24-byte block.
        let hi = unsafe { _mm_loadu_si128(input.as_ptr().add(start + 8).cast()) };
        let bytes = _mm256_set_m128i(hi, lo);
        let result = convert!(bytes, unshifted);
        let out_end = end / 3 * 4;
        // SAFETY: out_end - 32 >= 0 because end >= 24, and out_end is at most
        // the encoded length of the complete groups, which the caller sized.
        unsafe { _mm256_storeu_si256(output.as_mut_ptr().add(out_end - 32).cast(), result) };
        at = end;
    }
    at
}

/// Decode complete blocks of 32 alphabet bytes to 24 bytes, validating in place.
///
/// # Safety
/// The processor must support AVX2; output must have room for
/// `input.len() / 32 * 24` bytes.
#[target_feature(enable = "avx2")]
pub(crate) unsafe fn decode(
    input: &[u8],
    output: &mut [u8],
    alphabet: Alphabet,
) -> Result<usize, usize> {
    let blocks = input.len() / 32;
    // Rule 1: construct the low-nibble membership bitmap for the selected
    // alphabet. High-nibble bitmaps intersect it to validate 32 characters
    // with two register shuffles. Inspired by moego's AVX2 nibble method.
    let (low_table, split, roll): (&[u8; 16], i8, [i8; 16]) = match alphabet {
        Alphabet::Standard => (
            &STANDARD_MEMBERSHIP,
            46,
            [0, 0, 19, 16, 4, -65, -65, -71, -71, 0, 0, 0, 0, 0, 0, 0],
        ),
        Alphabet::UrlSafe => (
            &URL_MEMBERSHIP,
            94,
            [0, 0, 17, 4, -65, -65, -32, -71, -71, 0, 0, 0, 0, 0, 0, 0],
        ),
    };
    // SAFETY: these arrays contain 16 bytes, exactly the size of the loads.
    let low_lut =
        unsafe { _mm256_broadcastsi128_si256(_mm_loadu_si128(low_table.as_ptr().cast())) };
    // SAFETY: the roll table contains 16 bytes and is loaded without alignment requirements.
    let roll_lut = unsafe { _mm256_broadcastsi128_si256(_mm_loadu_si128(roll.as_ptr().cast())) };
    let high_lut = _mm256_setr_epi8(
        1, 2, 4, 8, 16, 32, 64, -128, 0, 0, 0, 0, 0, 0, 0, 0, 1, 2, 4, 8, 16, 32, 64, -128, 0, 0,
        0, 0, 0, 0, 0, 0,
    );
    let pair_mul = _mm256_set1_epi16(0x0140);
    let quad_mul = _mm256_set1_epi32(0x0001_1000);
    // Rule 3: valid inputs dominate. Validate two independent blocks and OR
    // their error vectors before branching once; locate the first bad block
    // only on the slow path. Loads remain strictly within the input slice.
    macro_rules! translate {
        ($block:expr) => {{
            // SAFETY: the caller of the macro ensures this is a complete block.
            let bytes = unsafe { _mm256_loadu_si256(input.as_ptr().add($block * 32).cast()) };
            let high = _mm256_and_si256(_mm256_srli_epi16(bytes, 4), _mm256_set1_epi8(15));
            let valid = _mm256_and_si256(
                _mm256_shuffle_epi8(low_lut, bytes),
                _mm256_shuffle_epi8(high_lut, high),
            );
            let bad = _mm256_cmpeq_epi8(valid, _mm256_setzero_si256());
            let index = _mm256_sub_epi8(high, _mm256_cmpgt_epi8(bytes, _mm256_set1_epi8(split)));
            let values = _mm256_add_epi8(bytes, _mm256_shuffle_epi8(roll_lut, index));
            (values, bad)
        }};
    }
    macro_rules! pack_store {
        ($values:expr, $block:expr, $next:expr) => {{
            // Rule 1: two multiply-add stages repack four sextets into three
            // bytes, followed by a register shuffle and exact bounded stores.
            let pairs = _mm256_maddubs_epi16($values, pair_mul);
            let groups = _mm256_madd_epi16(pairs, quad_mul);
            let shuffle = _mm256_setr_epi8(
                2, 1, 0, 6, 5, 4, 10, 9, 8, 14, 13, 12, -1, -1, -1, -1, 2, 1, 0, 6, 5, 4, 10, 9, 8,
                14, 13, 12, -1, -1, -1, -1,
            );
            let packed = _mm256_shuffle_epi8(groups, shuffle);
            let out = $block * 24;
            if $next {
                // SAFETY: the next output block exists, so both stores fit.
                // On success its store overwrites these four extra bytes;
                // on error the caller is required to discard partial output.
                unsafe {
                    _mm_storeu_si128(
                        output.as_mut_ptr().add(out).cast(),
                        _mm256_castsi256_si128(packed),
                    );
                    _mm_storeu_si128(
                        output.as_mut_ptr().add(out + 12).cast(),
                        _mm256_extracti128_si256::<1>(packed),
                    );
                }
            } else {
                let reordered =
                    _mm256_permutevar8x32_epi32(packed, _mm256_setr_epi32(0, 1, 2, 4, 2, 4, 5, 6));
                // SAFETY: these stores remain in the block and their overlap
                // contains identical bytes.
                unsafe {
                    _mm_storeu_si128(
                        output.as_mut_ptr().add(out).cast(),
                        _mm256_castsi256_si128(reordered),
                    );
                    _mm_storeu_si128(
                        output.as_mut_ptr().add(out + 8).cast(),
                        _mm256_extracti128_si256::<1>(reordered),
                    );
                }
            }
        }};
    }
    macro_rules! check_errors {
        ($e0:expr, $e1:expr, $block:expr) => {{
            let errors = _mm256_or_si256($e0, $e1);
            // Rule 3: one flag-setting test replaces mask extraction and a
            // separate scalar compare on the common path.
            if _mm256_testz_si256(errors, errors) == 0 {
                for (i, error) in [$e0, $e1].into_iter().enumerate() {
                    let mask = _mm256_movemask_epi8(error) as u32;
                    if mask != 0 {
                        return Err(($block + i) * 32 + mask.trailing_zeros() as usize);
                    }
                }
            }
        }};
    }
    let mut block = 0;
    // Large inputs take the hand-scheduled loop. Four trailing blocks stay in
    // Rust so the assembly's overlapping stores always land in valid output,
    // and they also take care of the final partial-batch tail.
    #[cfg(target_os = "linux")]
    if blocks >= 8 {
        let groups = (blocks - 4) / 4;
        let params = match alphabet {
            Alphabet::Standard => &STANDARD_PARAMS,
            Alphabet::UrlSafe => &URL_PARAMS,
        };
        // SAFETY: AVX2 is guaranteed by this function's contract. The loop
        // reads at most `groups * 128 + 64` bytes, which is below
        // `blocks * 32`, and writes at most `groups * 96 + 4` bytes, which is
        // below `blocks * 24`. Both are inside the caller's slices.
        let status = unsafe {
            pagesift_base64_decode_groups4(
                input.as_ptr(),
                output.as_mut_ptr(),
                groups,
                params as *const DecodeParams as *const u8,
            )
        };
        if status != usize::MAX {
            return Err(status);
        }
        block = groups * 4;
    }
    if blocks - block >= 2 {
        // Rule 3: validate the next pair while the current pair packs and
        // stores. The check stays ahead of those stores, so an invalid block
        // is never followed by output from later blocks.
        let (mut p0, e0) = translate!(block);
        let (mut p1, e1) = translate!(block + 1);
        check_errors!(e0, e1, block);
        macro_rules! pair_step {
            ($block:expr) => {{
                let (v0, e0) = translate!($block);
                let (v1, e1) = translate!($block + 1);
                check_errors!(e0, e1, $block);
                pack_store!(p0, $block - 2, true);
                pack_store!(p1, $block - 1, true);
                p0 = v0;
                p1 = v1;
            }};
        }
        block += 2;
        // Rule 2: stay ahead of the hardware streamer on inputs that do not
        // fit in L1. Splitting the prefetching prefix out of the loop removes
        // a bounds compare and branch from every remaining iteration.
        let prefetch_end = input.len().saturating_sub(2080) / 32;
        while block < prefetch_end && blocks - block >= 2 {
            // SAFETY: `block < prefetch_end` proves both prefetch lines are
            // inside the input allocation.
            unsafe {
                _mm_prefetch(input.as_ptr().add(block * 32 + 2048).cast(), _MM_HINT_T0);
            }
            pair_step!(block);
            block += 2;
        }
        while blocks - block >= 2 {
            pair_step!(block);
            block += 2;
        }
        pack_store!(p0, block - 2, true);
        pack_store!(p1, block - 1, false);
    }
    for block in block..blocks {
        // SAFETY: each iteration accesses exactly one complete 32-byte block;
        // AVX2 support is required by this function's contract.
        let bytes = unsafe { _mm256_loadu_si256(input.as_ptr().add(block * 32).cast()) };
        // Rule 1: map the four ASCII intervals to six-bit values in parallel.
        let high = _mm256_and_si256(_mm256_srli_epi16(bytes, 4), _mm256_set1_epi8(15));
        // Rule 3: the common case validates all 32 bytes in the vector pass;
        // only an invalid block goes back to scalar error identification.
        let valid = _mm256_and_si256(
            _mm256_shuffle_epi8(low_lut, bytes),
            _mm256_shuffle_epi8(high_lut, high),
        );
        let invalid = _mm256_movemask_epi8(_mm256_cmpeq_epi8(valid, _mm256_setzero_si256())) as u32;
        if invalid != 0 {
            return Err(block * 32 + invalid.trailing_zeros() as usize);
        }
        let index = _mm256_sub_epi8(high, _mm256_cmpgt_epi8(bytes, _mm256_set1_epi8(split)));
        let values = _mm256_add_epi8(bytes, _mm256_shuffle_epi8(roll_lut, index));
        // Rule 1: multiply-add repacks adjacent 6-bit values into 12- and
        // 24-bit groups; byte shuffle selects their big-endian bytes.
        let pairs = _mm256_maddubs_epi16(values, _mm256_set1_epi32(0x01400140));
        let groups = _mm256_madd_epi16(pairs, _mm256_set1_epi32(0x00011000));
        let shuffle = _mm256_setr_epi8(
            2, 1, 0, 6, 5, 4, 10, 9, 8, 14, 13, 12, -1, -1, -1, -1, 2, 1, 0, 6, 5, 4, 10, 9, 8, 14,
            13, 12, -1, -1, -1, -1,
        );
        let packed = _mm256_shuffle_epi8(groups, shuffle);
        // Rule 1: reorder 32-bit lanes so two overlapping 16-byte stores
        // write precisely the 24 decoded bytes without stack copies.
        let reordered =
            _mm256_permutevar8x32_epi32(packed, _mm256_setr_epi32(0, 1, 2, 4, 2, 4, 5, 6));
        let out = block * 24;
        // SAFETY: both 16-byte stores lie within the 24-byte output block;
        // the overlapping eight bytes contain identical decoded data.
        unsafe {
            _mm_storeu_si128(
                output.as_mut_ptr().add(out).cast(),
                _mm256_castsi256_si128(reordered),
            );
            _mm_storeu_si128(
                output.as_mut_ptr().add(out + 8).cast(),
                _mm256_extracti128_si256::<1>(reordered),
            );
        }
    }
    Ok(blocks * 32)
}

#[cfg(test)]
mod tests {
    use super::*;
    use base64::Engine;

    #[test]
    fn whitespace_filter_matches_scalar_for_all_bytes_and_boundaries() {
        if !std::is_x86_feature_detected!("avx2") {
            return;
        }
        let mut input = [0u8; 192];
        for len in [0, 1, 31, 32, 33, 63, 64, 65, 127, 128, 129, 192] {
            for byte in 0..=255u8 {
                for (i, b) in input.iter_mut().enumerate() {
                    *b = if i % 7 == 0 { byte } else { (i * 73) as u8 };
                }
                for capacity in [0, 1, 31, 32, 33, 64, 127, 192] {
                    let mut storage = [MaybeUninit::new(0xa5u8); 208];
                    // SAFETY: AVX2 was detected and both disjoint slices are
                    // valid for their respective lengths.
                    let (consumed, written) =
                        unsafe { filter_whitespace(&input[..len], &mut storage[8..8 + capacity]) };
                    let expected: Vec<u8> = input[..consumed]
                        .iter()
                        .copied()
                        .filter(|b| !matches!(b, b' ' | b'\t' | b'\r' | b'\n'))
                        .collect();
                    assert_eq!(consumed, len.min(capacity));
                    assert_eq!(written, expected.len());
                    // SAFETY: the whole array was initialized before calling
                    // the filter, which only writes initialized bytes.
                    let bytes = unsafe {
                        std::slice::from_raw_parts(storage.as_ptr().cast::<u8>(), storage.len())
                    };
                    assert_eq!(&bytes[8..8 + written], expected);
                    assert!(bytes[..8].iter().all(|&b| b == 0xa5));
                    assert!(bytes[8 + capacity..].iter().all(|&b| b == 0xa5));
                }
            }
        }
    }

    #[cfg(target_os = "linux")]
    #[test]
    fn asm_all_a() {
        if !std::is_x86_feature_detected!("avx2") {
            return;
        }
        let encoded = [b'A'; 192];
        let mut out = [0x55u8; 144];
        // SAFETY: AVX2 was detected; the assembly reads 192 input bytes and
        // writes at most 100 bytes, both within these arrays.
        let status = unsafe {
            pagesift_base64_decode_groups4(
                encoded.as_ptr(),
                out.as_mut_ptr(),
                1,
                &STANDARD_PARAMS as *const DecodeParams as *const u8,
            )
        };
        assert_eq!(status, usize::MAX);
        assert_eq!(&out[..96], &[0u8; 96]);
    }

    #[cfg(target_os = "linux")]
    #[test]
    fn asm_groups_match_reference() {
        if !std::is_x86_feature_detected!("avx2") {
            return;
        }
        let decoded: Vec<u8> = (0..144).map(|i| (i * 37 + 11) as u8).collect();
        let encoded = base64::engine::general_purpose::STANDARD.encode(&decoded);
        let mut out = [0u8; 144];
        // SAFETY: AVX2 was detected, encoded has 192 bytes, and out has
        // enough space for the assembly's 96-byte output plus its spill.
        let status = unsafe {
            pagesift_base64_decode_groups4(
                encoded.as_ptr(),
                out.as_mut_ptr(),
                1,
                &STANDARD_PARAMS as *const DecodeParams as *const u8,
            )
        };
        assert_eq!(status, usize::MAX);
        assert_eq!(&out[..96], &decoded[..96]);
    }

    #[test]
    fn mulhi_matches_words() {
        if !std::is_x86_feature_detected!("avx2") {
            return;
        }
        // SAFETY: AVX2 was detected and the store writes exactly 32 bytes into
        // the 16-element u16 array.
        let out = unsafe {
            let a = _mm256_setr_epi16(
                0x1234, -1, -32767, 7, 0x4000, 0x3333, 0x1111, -21555, 0x1234, -1, -32767, 7,
                0x4000, 0x3333, 0x1111, -21555,
            );
            let b = _mm256_set1_epi32(0x04000040);
            let got = mulhi_epu16(a, b);
            let mut out = [0u16; 16];
            _mm256_storeu_si256(out.as_mut_ptr().cast(), got);
            out
        };
        for (i, &x) in out.iter().enumerate() {
            let m = if i % 2 == 0 { 0x40 } else { 0x400 };
            let input = [
                0x1234u16, 0xffff, 0x8001, 7, 0x4000, 0x3333, 0x1111, 0xabcd, 0x1234, 0xffff,
                0x8001, 7, 0x4000, 0x3333, 0x1111, 0xabcd,
            ][i];
            assert_eq!(x, ((input as u32 * m) >> 16) as u16, "word {i}");
        }
    }
}
