use base64::Engine;
use base64::engine::general_purpose::{STANDARD, STANDARD_NO_PAD, URL_SAFE, URL_SAFE_NO_PAD};
use pagesift_base64::{Alphabet, Codec, Error};

#[test]
fn differential_and_roundtrip() {
    let mut state = 0x8251_8719_a11c_e52du64;
    let sizes = [
        0, 1, 2, 3, 4, 7, 63, 64, 65, 255, 1024, 4096, 65536, 1048576,
    ];
    for size in sizes {
        let mut input = vec![0; size];
        for b in &mut input {
            state ^= state << 13;
            state ^= state >> 7;
            state ^= state << 17;
            *b = state as u8;
        }
        for (alphabet, padding, reference) in [
            (Alphabet::Standard, true, &STANDARD),
            (Alphabet::Standard, false, &STANDARD_NO_PAD),
            (Alphabet::UrlSafe, true, &URL_SAFE),
            (Alphabet::UrlSafe, false, &URL_SAFE_NO_PAD),
        ] {
            let codec = Codec {
                alphabet,
                padding,
                ignore_whitespace: false,
            };
            let encoded = codec.encode_to_string(&input).unwrap();
            assert_eq!(encoded, reference.encode(&input));
            assert_eq!(codec.validate(encoded.as_bytes()), Ok(size));
            assert_eq!(codec.decode_to_vec(encoded.as_bytes()).unwrap(), input);
        }
    }
}

#[test]
fn encode_large_constant_length_and_unroll_boundaries() {
    // Exercise the previously broken constant-length AVX2 tail as well as
    // the transition between the four- and eight-block loops.
    let input: Vec<u8> = (0..(1 << 20)).map(|i| (i * 73 + 19) as u8).collect();
    let fixed = pagesift_base64::STANDARD.encode_to_string(&input).unwrap();
    assert_eq!(fixed, STANDARD.encode(&input));
    for size in (131_072 - 24)..=(131_072 + 24) {
        for (codec, reference) in [
            (pagesift_base64::STANDARD, &STANDARD),
            (
                Codec {
                    alphabet: Alphabet::UrlSafe,
                    padding: false,
                    ignore_whitespace: false,
                },
                &URL_SAFE_NO_PAD,
            ),
        ] {
            let expected = reference.encode(&input[..size]);
            let mut output = vec![0xa5; expected.len() + 16];
            let written = codec
                .encode(&input[..size], &mut output[..expected.len()])
                .unwrap();
            assert_eq!(&output[..written], expected.as_bytes(), "size {size}");
            assert!(output[written..].iter().all(|&b| b == 0xa5));
        }
    }
}

#[test]
fn random_lengths_and_all_small_boundaries() {
    let mut state = 0x31ba_ab19_c0de_7351u64;
    let mut bytes = vec![0; 1 << 20];
    for byte in &mut bytes {
        state ^= state << 13;
        state ^= state >> 7;
        state ^= state << 17;
        *byte = state as u8;
    }
    for size in (0..4096).chain((0..128).map(|i| ((i * 8111 + 19) % (1 << 20)) as usize)) {
        for (alphabet, padding, reference) in [
            (Alphabet::Standard, true, &STANDARD),
            (Alphabet::Standard, false, &STANDARD_NO_PAD),
            (Alphabet::UrlSafe, true, &URL_SAFE),
            (Alphabet::UrlSafe, false, &URL_SAFE_NO_PAD),
        ] {
            let codec = Codec {
                alphabet,
                padding,
                ignore_whitespace: false,
            };
            let encoded = codec.encode_to_string(&bytes[..size]).unwrap();
            assert_eq!(encoded, reference.encode(&bytes[..size]));
            assert_eq!(
                codec.decode_to_vec(encoded.as_bytes()).unwrap(),
                &bytes[..size]
            );
        }
    }
}

#[test]
fn whitespace_padding_and_invalid_inputs() {
    let codec = Codec {
        ignore_whitespace: true,
        ..pagesift_base64::STANDARD
    };
    assert_eq!(codec.decode_to_vec(b" aG\tVs\nbG8= ").unwrap(), b"hello");
    assert!(codec.decode_to_vec(b"aG!sbG8=").is_err());
    assert!(codec.decode_to_vec(b"aGk").is_err());
    assert!(codec.decode_to_vec(b"aGk===").is_err());
    assert!(codec.decode_to_vec(b"aGl=").is_err());
    assert!(matches!(
        codec.decode(b"aGk=", &mut [0]),
        Err(Error::OutputTooSmall)
    ));
    assert_eq!(
        pagesift_base64::encode_data_url("image/png", b"hi").unwrap(),
        "data:image/png;base64,aGk="
    );
}

#[test]
fn streaming_all_split_positions() {
    for (alphabet, padding) in [
        (Alphabet::Standard, true),
        (Alphabet::Standard, false),
        (Alphabet::UrlSafe, true),
        (Alphabet::UrlSafe, false),
    ] {
        let codec = Codec {
            alphabet,
            padding,
            ignore_whitespace: true,
        };
        for length in 0..128 {
            let data: Vec<u8> = (0..length).map(|i| (i * 37) as u8).collect();
            let encoded = codec.encode_to_string(&data).unwrap();
            for split in 0..=encoded.len() {
                let mut decoder = pagesift_base64::Decoder::new(codec);
                let mut out = Vec::new();
                decoder
                    .feed(&encoded.as_bytes()[..split], &mut out)
                    .unwrap();
                decoder.feed(b" \r\n", &mut out).unwrap();
                decoder
                    .feed(&encoded.as_bytes()[split..], &mut out)
                    .unwrap();
                decoder.finish(&mut out).unwrap();
                assert_eq!(out, data);
            }
        }
    }
}

#[test]
fn mutations_match_reference_acceptance_and_output() {
    let mut state = 0xd35f_5c91_0138_4a62u64;
    for size in 0..512 {
        let mut input = vec![0u8; size];
        for b in &mut input {
            state ^= state << 13;
            state ^= state >> 7;
            state ^= state << 17;
            *b = state as u8;
        }
        let original = STANDARD.encode(&input).into_bytes();
        for i in 0..original.len().min(64) {
            for replacement in [0, b'!', b'=', b'/', b'A', 255] {
                let mut changed = original.clone();
                changed[i] = replacement;
                let expected = STANDARD.decode(&changed);
                let actual = pagesift_base64::STANDARD.decode_to_vec(&changed);
                assert_eq!(actual.is_ok(), expected.is_ok(), "{changed:?}");
                if let (Ok(actual), Ok(expected)) = (actual, expected) {
                    assert_eq!(actual, expected);
                }
            }
        }
    }
}

#[test]
fn vector_boundaries_reject_every_non_alphabet_byte() {
    for size in [47, 48, 49, 71, 72, 73, 95, 96, 97, 192] {
        let input: Vec<u8> = (0..size).map(|i| (i * 41 + 23) as u8).collect();
        let original = STANDARD.encode(&input).into_bytes();
        for position in 0..original.len() {
            for byte in 0u8..=255 {
                let mut changed = original.clone();
                changed[position] = byte;
                let expected = STANDARD.decode(&changed);
                let mut out = vec![0; changed.len()];
                let actual = pagesift_base64::STANDARD.decode(&changed, &mut out);
                assert_eq!(
                    actual.is_ok(),
                    expected.is_ok(),
                    "size={size} position={position} byte={byte}"
                );
                if let (Ok(len), Ok(expected)) = (actual, expected) {
                    assert_eq!(&out[..len], expected);
                }
            }
        }
    }
}

#[test]
fn whitespace_at_every_position_of_large_inputs() {
    let codec = Codec {
        ignore_whitespace: true,
        ..pagesift_base64::STANDARD
    };
    for size in [48, 49, 95, 96, 97, 255] {
        let input: Vec<u8> = (0..size).map(|i| (i * 53) as u8).collect();
        let encoded = STANDARD.encode(&input).into_bytes();
        for position in 0..=encoded.len() {
            let mut modified = encoded.clone();
            modified.splice(position..position, *b" \r\n\t");
            assert_eq!(codec.decode_to_vec(&modified).unwrap(), input);
            let mut output = vec![0u8; size];
            assert_eq!(codec.decode(&modified, &mut output), Ok(size));
            assert_eq!(output, input);
        }
    }
}

#[test]
fn whitespace_across_staging_boundaries() {
    let codec = Codec {
        ignore_whitespace: true,
        ..pagesift_base64::STANDARD
    };
    for size in [3068, 3069, 3070, 3071, 3072, 3073, 8192, 16384, 1048576] {
        let input: Vec<u8> = (0..size).map(|i| (i * 37 + 13) as u8).collect();
        let encoded = STANDARD.encode(&input);
        let mut wire = Vec::new();
        for chunk in encoded.as_bytes().chunks(76) {
            wire.extend_from_slice(chunk);
            wire.extend_from_slice(b"\r\n");
        }
        let mut decoded = vec![0; size];
        assert_eq!(codec.decode(&wire, &mut decoded), Ok(size));
        assert_eq!(decoded, input);
        for position in [0, wire.len() / 2, wire.len() - 1] {
            let mut bad = wire.clone();
            bad[position] = b'!';
            assert!(codec.decode(&bad, &mut decoded).is_err());
        }
        if size != 0 {
            assert!(matches!(
                codec.decode(&wire, &mut decoded[..size - 1]),
                Err(Error::OutputTooSmall)
            ));
        }
    }
}

#[test]
fn whitespace_simd_errors_and_dense_boundaries() {
    for (alphabet, padding, reference) in [
        (Alphabet::Standard, true, &STANDARD),
        (Alphabet::Standard, false, &STANDARD_NO_PAD),
        (Alphabet::UrlSafe, true, &URL_SAFE),
        (Alphabet::UrlSafe, false, &URL_SAFE_NO_PAD),
    ] {
        let codec = Codec {
            alphabet,
            padding,
            ignore_whitespace: true,
        };
        let input: Vec<u8> = (0..8193).map(|i| (i * 61 + 7) as u8).collect();
        let encoded = reference.encode(&input);
        let mut wire = Vec::new();
        for (i, &byte) in encoded.as_bytes().iter().enumerate() {
            if i % 3 == 0 {
                wire.extend_from_slice(b" \t\r\n");
            }
            wire.push(byte);
        }
        let mut output = vec![0; input.len()];
        assert_eq!(codec.decode(&wire, &mut output), Ok(input.len()));
        assert_eq!(output, input);
        assert_eq!(
            codec.decode(&wire, &mut output[..input.len() - 1]),
            Err(Error::OutputTooSmall)
        );
        for position in [0, 31, 32, 255, 256, 4094, 4095, 4096, wire.len() - 1] {
            let mut invalid = wire.clone();
            invalid[position] = b'!';
            assert_eq!(
                codec.decode(&invalid, &mut output),
                Err(Error::InvalidByte {
                    byte: b'!',
                    offset: position
                })
            );
        }
    }
}

#[test]
fn whitespace_short_and_vector_thresholds() {
    let codec = Codec {
        ignore_whitespace: true,
        ..pagesift_base64::STANDARD
    };
    let input: Vec<u8> = (0..1024).map(|i| (i * 31 + 9) as u8).collect();
    for size in [
        0, 1, 16, 23, 24, 44, 45, 46, 47, 48, 49, 189, 190, 191, 192, 380, 381, 382, 383, 384, 385,
    ] {
        let encoded = STANDARD.encode(&input[..size]);
        for whitespace in [&b""[..], &b"\r\n"[..], &b" \t\r\n"[..]] {
            let mut wire = encoded.as_bytes().to_vec();
            wire.extend_from_slice(whitespace);
            assert_eq!(codec.decode_to_vec(&wire).unwrap(), &input[..size]);
            if !wire.is_empty() {
                let offset = wire.len() / 2;
                wire[offset] = 0x0b; // Vertical tab is not accepted whitespace.
                assert_eq!(
                    codec.decode_to_vec(&wire).map(|_| ()),
                    codec.validate(&wire).map(|_| ())
                );
            }
        }
    }
}

#[test]
fn standard_error_text_matches_existing_crate() {
    for a in 0u8..=255 {
        for b in 0u8..=255 {
            for sample in [
                &[a][..],
                &[a, b][..],
                &[a, b, b'='][..],
                &[b'A', a, b, b'='][..],
            ] {
                let expected = STANDARD.decode(sample).map_err(|e| e.to_string());
                let actual = pagesift_base64::decode_standard_compat(sample);
                assert_eq!(actual, expected, "input={sample:?}");
            }
        }
    }
    let mut state = 42u64;
    for size in 0..256 {
        let data: Vec<u8> = (0..size).map(|i| (i * 73) as u8).collect();
        let original = STANDARD.encode(data).into_bytes();
        for _ in 0..100 {
            let mut changed = original.clone();
            if !changed.is_empty() {
                state ^= state << 13;
                state ^= state >> 7;
                state ^= state << 17;
                let index = state as usize % changed.len();
                changed[index] = (state >> 24) as u8;
            }
            assert_eq!(
                pagesift_base64::decode_standard_compat(&changed),
                STANDARD.decode(&changed).map_err(|e| e.to_string()),
                "input={changed:?}"
            );
        }
    }
}
