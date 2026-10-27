//! CSV 的字符集：和网页一样解码，而不是 pagesift-office 自己的"BOM、UTF-8、否则 Windows-1252"。
//!
//! 顺序和网页相同（`pagesift_net::decode`）：BOM，Content-Type 的 `charset`，正文探测
//! （chardetng 加自研统计）；声明和正文对不上时以正文为准并给 `decode:` 警告。Shift_JIS、GBK
//! 的表格以前整页乱码而且没有任何提示。
//!
//! 两处和网页不同，都是因为 CSV 可以很大而且整篇同一种编码：没有声明、没有 BOM、正文是合法
//! UTF-8 的直接按 UTF-8（省掉网页路径上针对 UTF-7 之类的全文扫描，这条路和 0.80.0 的开销相同）；
//! 需要探测时只看开头 64 KiB，定下编码后再解码整个正文（chardetng 对 1 MB 正文要跑几十亿条指令）。

use pagesift_net::Decoded;

/// 探测编码时看的正文长度。
const SAMPLE: usize = 64 * 1024;

/// 解码 CSV 字节。`content_type` 是响应头或上传时的 Content-Type（带 `charset` 参数才有用）；
/// 返回文本和解码警告（没有问题时为 None）。
pub(crate) fn decode(bytes: &[u8], content_type: Option<&str>) -> (String, Option<String>) {
    let declared = content_type.is_some_and(|c| c.to_ascii_lowercase().contains("charset="));
    let bom = bom_len(bytes);
    if !declared
        && bom == 0
        && !bytes.contains(&0)
        && let Ok(text) = std::str::from_utf8(bytes)
    {
        return (text.to_owned(), None);
    }
    if bytes.len() <= SAMPLE {
        let decoded = pagesift_net::decode(bytes, content_type, None);
        let warning = decoded.warning();
        return (decoded.text, warning);
    }
    // 大文件：在样本上定编码（声明、探测、声明是否说谎都在这一步），再按它解码全文
    let sample: Decoded = pagesift_net::decode(&bytes[..sample_end(bytes)], content_type, None);
    if matches!(
        sample.encoding_name,
        "utf-7" | "utf-32" | "utf-32le" | "utf-32be" | "cp500"
    ) {
        // 这几种编码只有网页解码器里的专用解码器会解，罕见，整篇交给它
        let decoded = pagesift_net::decode(bytes, content_type, None);
        let warning = decoded.warning();
        return (decoded.text, warning);
    }
    let warning = sample.warning();
    let (text, _) = sample.encoding.decode_without_bom_handling(&bytes[bom..]);
    (text.into_owned(), warning)
}

/// 样本的截止位置：退到 UTF-8 的字符边界上，免得切断的半个字符让"正文是合法 UTF-8"的判断失效。
fn sample_end(bytes: &[u8]) -> usize {
    let mut end = SAMPLE;
    while end > SAMPLE - 4 && bytes[end] & 0xC0 == 0x80 {
        end -= 1;
    }
    end
}

/// 开头 BOM 的长度（UTF-32、UTF-8、UTF-16）。
fn bom_len(bytes: &[u8]) -> usize {
    if bytes.starts_with(&[0xFF, 0xFE, 0x00, 0x00]) || bytes.starts_with(&[0x00, 0x00, 0xFE, 0xFF])
    {
        4
    } else if bytes.starts_with(&[0xEF, 0xBB, 0xBF]) {
        3
    } else if bytes.starts_with(&[0xFF, 0xFE]) || bytes.starts_with(&[0xFE, 0xFF]) {
        2
    } else {
        0
    }
}

#[cfg(test)]
mod tests {
    use super::{SAMPLE, decode};

    fn gbk_body(rows: usize) -> String {
        "编号,城市,数量,备注\n".to_string()
            + &(0..rows)
                .map(|i| format!("{i},北京上海广州深圳成都,{i},订单 {i} 的备注信息很长很长\n"))
                .collect::<String>()
    }

    #[test]
    fn header_charset_and_detection_both_work() {
        let (sjis, _, _) = encoding_rs::SHIFT_JIS.encode("番号,都市\n1,東京\n");
        let (text, warning) = decode(&sjis, Some("text/csv; charset=shift_jis"));
        assert_eq!(text, "番号,都市\n1,東京\n");
        assert_eq!(warning, None);
        // 没有 charset：探测。短文本靠统计特征，多给几行
        let body = gbk_body(40);
        let (gbk, _, _) = encoding_rs::GBK.encode(&body);
        let (text, _) = decode(&gbk, Some("text/csv"));
        assert_eq!(text, body);
        // 声明撒谎：正文是 GBK 却说 UTF-8，按正文纠正并警告
        let (text, warning) = decode(&gbk, Some("text/csv; charset=utf-8"));
        assert_eq!(text, body);
        assert!(
            warning.as_deref().is_some_and(|w| w.starts_with("decode:")),
            "{warning:?}"
        );
        // 纯 ASCII 和 UTF-8 照旧（快路径），BOM 去掉
        let (text, warning) = decode(b"a,b\n1,2\n", None);
        assert_eq!((text.as_str(), warning), ("a,b\n1,2\n", None));
        let (text, _) = decode("\u{feff}编号,城市\n".as_bytes(), None);
        assert_eq!(text, "编号,城市\n");
        // 声明是单字节编码、正文却是合法 UTF-8：按网页的规则以正文为准
        let (text, warning) = decode("编号,城市\n".as_bytes(), Some("text/csv; charset=latin1"));
        assert_eq!(text, "编号,城市\n");
        assert!(warning.is_some());
    }

    /// 超过样本长度的正文：编码在样本上定，全文按它解码；UTF-16（含 NUL）不会被当成 UTF-8。
    #[test]
    fn large_bodies_are_detected_on_a_sample_and_decoded_whole() {
        let body = gbk_body(6000);
        let (gbk, _, _) = encoding_rs::GBK.encode(&body);
        assert!(gbk.len() > SAMPLE);
        let (text, warning) = decode(&gbk, Some("text/csv"));
        assert_eq!(text, body);
        assert_eq!(warning, None);
        let (text, warning) = decode(&gbk, Some("text/csv; charset=utf-8"));
        assert_eq!(text, body);
        assert!(warning.is_some());
        let (text, _) = decode(body.as_bytes(), None);
        assert_eq!(text, body);
        let mut with_bom = vec![0xFF, 0xFE];
        with_bom.extend("a,b\n1,2\n".encode_utf16().flat_map(u16::to_le_bytes));
        assert_eq!(decode(&with_bom, None).0, "a,b\n1,2\n");
    }
}
