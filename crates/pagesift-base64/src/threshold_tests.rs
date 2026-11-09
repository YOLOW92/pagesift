//! Ignored exploratory crossover test; run under the shared benchmark lock.
//! wall-clock-file: ignored benchmark helper; it prints measurements and asserts no timing bound.
#[cfg(target_arch = "x86_64")]
use crate::{Alphabet, STANDARD};
#[cfg(target_arch = "x86_64")]
use std::{hint::black_box, time::Instant};

#[test]
#[ignore = "timing, run separately under the benchmark lock"]
#[cfg(target_arch = "x86_64")]
fn decode_crossover() {
    if !crate::avx2_available() {
        return;
    }
    println!("encoded_bytes,scalar_ns,short_ns,simd_ns");
    for size in [32usize, 48, 64, 96, 128, 192, 256, 512, 1024, 4096] {
        let bytes = vec![b'A'; size];
        let mut dst = vec![0u8; size / 4 * 3];
        let iterations = 200_000;
        let start = Instant::now();
        for _ in 0..iterations {
            STANDARD
                .decode_scalar(black_box(&bytes), black_box(&mut dst))
                .unwrap();
        }
        let scalar = start.elapsed().as_nanos() as f64 / iterations as f64;
        let start = Instant::now();
        for _ in 0..iterations {
            STANDARD
                .decode_short(black_box(&bytes), black_box(&mut dst))
                .unwrap();
        }
        let short = start.elapsed().as_nanos() as f64 / iterations as f64;
        let start = Instant::now();
        for _ in 0..iterations {
            // SAFETY: runtime detection succeeded; input is a whole number of
            // 32-byte valid blocks and output has 24 bytes per block.
            unsafe {
                crate::avx2::decode(black_box(&bytes), black_box(&mut dst), Alphabet::Standard)
            }
            .unwrap();
        }
        let simd = start.elapsed().as_nanos() as f64 / iterations as f64;
        println!("{size},{scalar:.1},{short:.1},{simd:.1}");
    }
}
