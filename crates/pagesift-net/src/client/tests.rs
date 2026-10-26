use std::io::Write;

use super::*;

#[test]
fn happy_eyeballs_interleaves_address_families() {
    let addresses = vec![
        "2001:db8::1".parse().unwrap(),
        "2001:db8::2".parse().unwrap(),
        "192.0.2.1".parse().unwrap(),
        "192.0.2.2".parse().unwrap(),
    ];
    let interleaved = interleave_address_families(addresses);
    assert!(interleaved[0].is_ipv6());
    assert!(interleaved[1].is_ipv4());
    assert!(interleaved[2].is_ipv6());
    assert!(interleaved[3].is_ipv4());
}

fn decode(encoding: &str, raw: Vec<u8>, max: usize) -> (Vec<u8>, bool, HeaderMap) {
    let (d, h) = decode_full(encoding, raw, max, false);
    let d = d.expect("decodes");
    (d.body, d.truncated, h)
}

fn decode_full(
    encoding: &str,
    raw: Vec<u8>,
    max: usize,
    input_truncated: bool,
) -> (Result<DecodedBody>, HeaderMap) {
    let mut h = HeaderMap::new();
    h.insert(
        header::CONTENT_ENCODING,
        HeaderValue::from_str(encoding).unwrap(),
    );
    h.insert(header::CONTENT_LENGTH, HeaderValue::from_static("1"));
    let d = decode_body(&mut h, raw, max, input_truncated);
    (d, h)
}

fn gzip(data: &[u8]) -> Vec<u8> {
    let mut g = flate2::write::GzEncoder::new(Vec::new(), flate2::Compression::fast());
    g.write_all(data).unwrap();
    g.finish().unwrap()
}

#[test]
fn decodes_every_supported_encoding() {
    let text = b"hello hello hello hello".repeat(50);
    let mut z = flate2::write::ZlibEncoder::new(Vec::new(), flate2::Compression::fast());
    z.write_all(&text).unwrap();
    let mut d = flate2::write::DeflateEncoder::new(Vec::new(), flate2::Compression::fast());
    d.write_all(&text).unwrap();
    let zs = zstd::encode_all(&text[..], 1).unwrap();
    for (enc, raw) in [
        ("deflate", z.finish().unwrap()),
        ("deflate", d.finish().unwrap()),
        ("zstd", zs),
    ] {
        let (body, cut, h) = decode(enc, raw, 1 << 20);
        assert_eq!(body, text, "{enc}");
        assert!(!cut);
        assert!(h.get(header::CONTENT_ENCODING).is_none());
        assert!(h.get(header::CONTENT_LENGTH).is_none());
    }
    // 未压缩元数据块的 brotli 流："hello"
    let (body, _, _) = decode(
        "br",
        vec![0x0b, 0x02, 0x80, b'h', b'e', b'l', b'l', b'o', 0x03],
        100,
    );
    assert_eq!(body, b"hello");
}

#[test]
fn caps_decoded_size_and_keeps_unknown_encodings() {
    let mut g = flate2::write::GzEncoder::new(Vec::new(), flate2::Compression::fast());
    g.write_all(&[b'a'; 100_000]).unwrap();
    let (body, cut, _) = decode("gzip", g.finish().unwrap(), 1000);
    assert_eq!(body.len(), 1000);
    assert!(cut, "decompression bombs stop at the body limit");

    let (d, h) = decode_full("compress", b"raw".to_vec(), 100, false);
    let d = d.unwrap();
    assert_eq!(d.body, b"raw");
    assert!(!d.truncated);
    assert!(h.get(header::CONTENT_ENCODING).is_some());
    assert!(
        d.warnings[0]
            .starts_with("content_encoding_unsupported: unknown content coding `compress`"),
        "{:?}",
        d.warnings
    );
}

#[test]
fn decodes_stacked_codings_in_reverse_order() {
    let text = b"stacked body ".repeat(40);
    // gzip, gzip：同一编码重复两次
    let (body, cut, h) = decode("gzip, gzip", gzip(&gzip(&text)), 1 << 20);
    assert_eq!(body, text);
    assert!(!cut);
    assert!(h.get(header::CONTENT_ENCODING).is_none());
    // gzip, zstd：先 gzip 后 zstd，解码时先解 zstd
    let raw = zstd::encode_all(&gzip(&text)[..], 1).unwrap();
    let (d, _) = decode_full("gzip, zstd", raw, 1 << 20, false);
    let d = d.unwrap();
    assert_eq!(d.body, text);
    assert!(d.warnings.is_empty(), "{:?}", d.warnings);
    // 多个 Content-Encoding 头行等价于逗号拼接
    let mut h = HeaderMap::new();
    h.append(header::CONTENT_ENCODING, HeaderValue::from_static("gzip"));
    h.append(header::CONTENT_ENCODING, HeaderValue::from_static("gzip"));
    let d = decode_body(&mut h, gzip(&gzip(&text)), 1 << 20, false).unwrap();
    assert_eq!(d.body, text);
}

#[test]
fn stacked_codings_keep_the_size_cap_and_layer_limit() {
    // 两层压缩的炸弹：输出仍停在 max
    let bomb = gzip(&gzip(&[b'a'; 200_000]));
    let (body, cut, _) = decode("gzip, gzip", bomb, 1000);
    assert_eq!(body.len(), 1000);
    assert!(cut);
    // 超过 3 层：不解，原样返回并给出提示
    let raw = gzip(&gzip(&gzip(&gzip(b"deep"))));
    let (d, h) = decode_full("gzip, gzip, gzip, gzip", raw.clone(), 1 << 20, false);
    let d = d.unwrap();
    assert_eq!(d.body, raw);
    assert!(h.get(header::CONTENT_ENCODING).is_some());
    assert!(
        d.warnings[0].contains("exceed the limit of 3"),
        "{:?}",
        d.warnings
    );
    // 恰好 3 层可以解
    let (body, _, _) = decode("gzip, gzip, gzip", gzip(&gzip(&gzip(b"three"))), 100);
    assert_eq!(body, b"three");
}

#[test]
fn plaintext_declared_as_compressed_is_used_as_is() {
    let html = b"<html><body>plain</body></html>".to_vec();
    for enc in ["gzip", "zstd", "x-gzip"] {
        let (d, h) = decode_full(enc, html.clone(), 1 << 20, false);
        let d = d.unwrap();
        assert_eq!(d.body, html, "{enc}");
        assert!(!d.truncated, "{enc}: must not claim the size limit");
        assert_eq!(
            d.warnings,
            [format!(
                "content_encoding_mismatch: declared {enc} but body is not {enc}; used as-is"
            )]
        );
        assert!(h.get(header::CONTENT_ENCODING).is_none());
    }
    // 内层是明文：外层解开后按原文使用
    let (d, _) = decode_full("gzip, gzip", gzip(&html), 1 << 20, false);
    let d = d.unwrap();
    assert_eq!(d.body, html);
    assert_eq!(d.warnings.len(), 1);
    // 反例：正常 gzip 没有任何提示
    let (d, _) = decode_full("gzip", gzip(&html), 1 << 20, false);
    let d = d.unwrap();
    assert_eq!(d.body, html);
    assert!(d.warnings.is_empty());
    assert!(!d.truncated);
}

#[test]
fn corrupt_streams_report_decoding_failure_not_size_limit() {
    // 有 gzip 魔数但内容是垃圾：一个字节都解不出 → 解压失败
    let mut garbage = vec![0x1f, 0x8b, 0x08, 0, 0, 0, 0, 0, 0, 0xff];
    garbage.extend_from_slice(&[0xff; 64]);
    let (d, _) = decode_full("gzip", garbage, 1 << 20, false);
    let err = d.unwrap_err();
    assert!(matches!(err, Error::ContentDecoding(_)), "{err}");
    assert!(err.to_string().contains("content decoding failed"), "{err}");

    // 尾部缺失：保留已解出的部分，给出损坏提示，不算截断
    let text = b"0123456789".repeat(500);
    let mut cut_stream = gzip(&text);
    cut_stream.truncate(cut_stream.len() - 4);
    let (d, _) = decode_full("gzip", cut_stream.clone(), 1 << 20, false);
    let d = d.unwrap();
    assert!(!d.body.is_empty());
    assert!(!d.truncated);
    assert!(
        d.warnings[0].starts_with("content_decoding_failed: gzip"),
        "{:?}",
        d.warnings
    );

    // 输入本来就被截断（超出 body 上限）：只记截断，不报损坏
    let (d, _) = decode_full("gzip", cut_stream, 1 << 20, true);
    let d = d.unwrap();
    assert!(d.truncated);
    assert!(d.warnings.is_empty(), "{:?}", d.warnings);
}
