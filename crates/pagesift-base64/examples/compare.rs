//! Same-process A/B benchmark. Reports median per-call time over alternating
//! rounds; run pinned to a quiet CPU while holding tasks/0.80/bench.lock.
//!
//! Buffers are placed at a 64-byte boundary so both implementations see the
//! same cache-line alignment. (A source at 16 mod 64 costs this AVX2 decoder
//! roughly four percent because half of its 32-byte loads split a line.)
use base64::{Engine, engine::general_purpose::STANDARD};
use std::hint::black_box;
use std::time::Instant;

fn one_round(mut run: impl FnMut(), iterations: usize) -> f64 {
    let start = Instant::now();
    for _ in 0..iterations {
        run();
    }
    start.elapsed().as_secs_f64() / iterations as f64 * 1e6
}

fn median(values: &mut [f64]) -> f64 {
    values.sort_by(f64::total_cmp);
    values[values.len() / 2]
}

/// Storage with a 64-byte-aligned start offset inside the allocation.
fn aligned_storage(len: usize) -> (Vec<u8>, usize) {
    let storage = vec![0u8; len + 64];
    let start = (64 - (storage.as_ptr() as usize & 63)) & 63;
    (storage, start)
}

fn reference_decode(ignore_whitespace: bool, wire: &[u8], filtered: &mut [u8], output: &mut [u8]) {
    if ignore_whitespace {
        let mut len = 0;
        for &byte in black_box(wire) {
            if !matches!(byte, b' ' | b'\t' | b'\r' | b'\n') {
                filtered[len] = byte;
                len += 1;
            }
        }
        STANDARD
            .decode_slice(black_box(&filtered[..len]), black_box(output))
            .unwrap();
    } else {
        STANDARD
            .decode_slice(black_box(wire), black_box(output))
            .unwrap();
    }
}

fn main() {
    println!("size,mode,rounds,ours_encode_us,base64_encode_us,ours_decode_us,base64_decode_us");
    for size in [
        8, 16, 24, 32, 48, 63, 64, 96, 128, 4096, 65536, 1048576, 4194304,
    ] {
        for ignore_whitespace in [false, true] {
            let (mut input_storage, input_start) = aligned_storage(size);
            for i in 0..size {
                input_storage[input_start + i] = (i * 73 + 19) as u8;
            }
            let input = &input_storage[input_start..input_start + size];
            let encoded = STANDARD.encode(input);
            let wire_extra = if ignore_whitespace {
                encoded.len() / 76 * 2 + 2
            } else {
                0
            };
            let (mut wire_storage, wire_start) = aligned_storage(encoded.len() + wire_extra);
            let mut wire_end = wire_start;
            for chunk in encoded.as_bytes().chunks(76) {
                wire_storage[wire_end..wire_end + chunk.len()].copy_from_slice(chunk);
                wire_end += chunk.len();
                if ignore_whitespace {
                    wire_storage[wire_end..wire_end + 2].copy_from_slice(b"\r\n");
                    wire_end += 2;
                }
            }
            let wire = &wire_storage[wire_start..wire_end];
            let (mut filtered_storage, filtered_start) = aligned_storage(wire.len());
            let filtered = &mut filtered_storage[filtered_start..filtered_start + wire.len()];
            let (mut output_storage, output_start) = aligned_storage(encoded.len());
            let output = &mut output_storage[output_start..output_start + encoded.len()];
            let iterations = (16 * 1024 * 1024 / size).clamp(32, 500_000);
            let codec = pagesift_base64::Codec {
                ignore_whitespace,
                ..pagesift_base64::STANDARD
            };
            let rounds = 7;
            let mut ours_enc = Vec::with_capacity(rounds);
            let mut base_enc = Vec::with_capacity(rounds);
            let mut ours_dec = Vec::with_capacity(rounds);
            let mut base_dec = Vec::with_capacity(rounds);
            for round in 0..rounds {
                if round % 2 == 0 {
                    ours_enc.push(one_round(
                        || {
                            codec
                                .encode(black_box(input), black_box(&mut *output))
                                .unwrap();
                        },
                        iterations,
                    ));
                    base_enc.push(one_round(
                        || {
                            STANDARD
                                .encode_slice(black_box(input), black_box(&mut *output))
                                .unwrap();
                        },
                        iterations,
                    ));
                    ours_dec.push(one_round(
                        || {
                            codec
                                .decode(black_box(wire), black_box(&mut *output))
                                .unwrap();
                        },
                        iterations,
                    ));
                    base_dec.push(one_round(
                        || {
                            reference_decode(ignore_whitespace, wire, filtered, &mut *output);
                        },
                        iterations,
                    ));
                } else {
                    base_dec.push(one_round(
                        || {
                            reference_decode(ignore_whitespace, wire, filtered, &mut *output);
                        },
                        iterations,
                    ));
                    ours_dec.push(one_round(
                        || {
                            codec
                                .decode(black_box(wire), black_box(&mut *output))
                                .unwrap();
                        },
                        iterations,
                    ));
                    base_enc.push(one_round(
                        || {
                            STANDARD
                                .encode_slice(black_box(input), black_box(&mut *output))
                                .unwrap();
                        },
                        iterations,
                    ));
                    ours_enc.push(one_round(
                        || {
                            codec
                                .encode(black_box(input), black_box(&mut *output))
                                .unwrap();
                        },
                        iterations,
                    ));
                }
            }
            let mode = if ignore_whitespace {
                "mime_crlf_every_76"
            } else {
                "strict"
            };
            println!(
                "{size},{mode},{rounds},{:.3},{:.3},{:.3},{:.3}",
                median(&mut ours_enc),
                median(&mut base_enc),
                median(&mut ours_dec),
                median(&mut base_dec)
            );
        }
    }
}
