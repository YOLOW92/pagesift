//! 解开 Content-Encoding（gzip、br、zstd、deflate），限层数和解压后大小。

use super::*;

/// 最多逐层解开的 Content-Encoding 层数。正常服务器只有一层，偶见代理/CDN 重复压缩
/// 出现两层；超过 3 层几乎只会是恶意构造（每层都能放大输出），直接不解。
pub(super) const MAX_CONTENT_CODINGS: usize = 3;

/// [`decode_body`] 的结果。
#[derive(Debug)]
pub(crate) struct DecodedBody {
    pub body: Vec<u8>,
    /// 解压输出触到 `max`，或输入本身已被截断导致压缩流不完整。
    pub truncated: bool,
    /// 面向调用方的提示：声明与实际不符、压缩流损坏、不支持的编码等。
    pub warnings: Vec<String>,
}

/// 按 Content-Encoding 解压。多层编码（`gzip, br`）按 RFC 9110 §8.4 逆序逐层解开，最多
/// [`MAX_CONTENT_CODINGS`] 层；每层输出都限制在 `max` 字节以内，解压炸弹最多吐出 `max`。
///
/// - 含不认识的编码或层数超限：原样返回，保留 Content-Encoding，并给出 warning；
/// - 声明 gzip/zstd 但正文开头没有对应魔数：按原文处理并给出 warning（brotli、deflate 没有
///   可靠魔数，不做这项检查）；
/// - 压缩流损坏：已解出的部分照常返回并给出 warning；一个字节都解不出来时报解压失败。
///   `input_truncated` 为真时输入本来就不完整，流中断是预期的，只记为截断。
///
/// 解压后去掉 Content-Encoding 和 Content-Length（它们描述的是压缩前）。
pub(crate) fn decode_body(
    headers: &mut HeaderMap,
    raw: Vec<u8>,
    max: usize,
    input_truncated: bool,
) -> Result<DecodedBody> {
    let passthrough = |body, warnings| DecodedBody {
        body,
        truncated: input_truncated,
        warnings,
    };
    let mut codings = Vec::new();
    for value in headers.get_all(header::CONTENT_ENCODING) {
        let Ok(value) = value.to_str() else {
            return Ok(passthrough(
                raw,
                vec!["content_encoding_unsupported: Content-Encoding header is not ASCII; body left encoded".into()],
            ));
        };
        codings.extend(
            value
                .split(',')
                .map(|c| c.trim().to_ascii_lowercase())
                .filter(|c| !c.is_empty() && c != "identity"),
        );
    }
    if codings.is_empty() {
        return Ok(passthrough(raw, Vec::new()));
    }
    if codings.len() > MAX_CONTENT_CODINGS {
        return Ok(passthrough(
            raw,
            vec![format!(
                "content_encoding_unsupported: {} content codings exceed the limit of {MAX_CONTENT_CODINGS}; body left encoded",
                codings.len()
            )],
        ));
    }
    if let Some(unknown) = codings
        .iter()
        .find(|c| !matches!(c.as_str(), "gzip" | "x-gzip" | "deflate" | "br" | "zstd"))
    {
        return Ok(passthrough(
            raw,
            vec![format!(
                "content_encoding_unsupported: unknown content coding `{unknown}`; body left encoded"
            )],
        ));
    }

    let mut data = raw;
    // truncated：输入被截断或某层触到上限；incomplete：当前层的输入不完整（截断或上一层
    // 已损坏），这时流中断是预期的，不再单独报错。
    let mut truncated = input_truncated;
    let mut incomplete = input_truncated;
    let mut warnings = Vec::new();
    // 最后施加的编码在列表末尾，先解它。
    for coding in codings.iter().rev() {
        if data.is_empty() {
            break;
        }
        let magic: &[u8] = match coding.as_str() {
            "gzip" | "x-gzip" => &[0x1f, 0x8b],
            "zstd" => &[0x28, 0xb5, 0x2f, 0xfd],
            _ => &[],
        };
        if !data.starts_with(magic) {
            warnings.push(format!(
                "content_encoding_mismatch: declared {coding} but body is not {coding}; used as-is"
            ));
            break;
        }
        let (out, cut, err) = decode_layer(coding, &data, max);
        if let Some(err) = err {
            if !incomplete {
                if out.is_empty() {
                    return Err(Error::ContentDecoding(format!("{coding}: {err}")));
                }
                warnings.push(format!(
                    "content_decoding_failed: {coding} stream is corrupt or incomplete ({err}); kept {} decoded bytes",
                    out.len()
                ));
            }
            incomplete = true;
        }
        if cut {
            truncated = true;
            incomplete = true;
        }
        data = out;
    }
    headers.remove(header::CONTENT_ENCODING);
    headers.remove(header::CONTENT_LENGTH);
    Ok(DecodedBody {
        body: data,
        truncated,
        warnings,
    })
}

/// 解开一层编码，输出最多 `max` 字节。返回 (输出, 是否触到上限, 解压错误)。
pub(super) fn decode_layer(
    coding: &str,
    data: &[u8],
    max: usize,
) -> (Vec<u8>, bool, Option<std::io::Error>) {
    let mut reader: Box<dyn Read + '_> = match coding {
        "gzip" | "x-gzip" => Box::new(flate2::read::MultiGzDecoder::new(data)),
        "deflate" => {
            // 规范是 zlib 封装，但不少服务器发的是裸 deflate：看头两个字节判断
            let zlib = data.len() >= 2
                && data[0] & 0x0f == 8
                && u16::from_be_bytes([data[0], data[1]]).is_multiple_of(31);
            if zlib {
                Box::new(flate2::read::ZlibDecoder::new(data))
            } else {
                Box::new(flate2::read::DeflateDecoder::new(data))
            }
        }
        "br" => Box::new(brotli_decompressor::Decompressor::new(data, 16 * 1024)),
        "zstd" => match zstd::stream::read::Decoder::with_buffer(data) {
            Ok(d) => Box::new(d),
            Err(e) => return (Vec::new(), false, Some(e)),
        },
        _ => unreachable!("decode_body 只传入已知编码"),
    };
    let mut out = Vec::with_capacity(data.len().saturating_mul(4).min(max));
    let mut buf = [0u8; 16 * 1024];
    loop {
        match reader.read(&mut buf) {
            Ok(0) => return (out, false, None),
            Ok(n) => {
                let room = max - out.len();
                if n > room {
                    out.extend_from_slice(&buf[..room]);
                    return (out, true, None);
                }
                out.extend_from_slice(&buf[..n]);
            }
            Err(e) if e.kind() == std::io::ErrorKind::Interrupted => {}
            Err(e) => return (out, false, Some(e)),
        }
    }
}
