//! 全文扫描重建对象索引：xref 读不出、或者偏移指错时用（与 pdf.js 的 indexObjects、MuPDF 的 repair 同思路）。
//!
//! 找每个 `N G obj` 的位置（同一个号以后出现的为准，增量更新是往后追加的）；跳过流数据，
//! 免得流里碰巧像对象头的字节被当真。顺带收集 trailer 字典、xref 流、对象流和 Catalog 的候选。

use std::collections::HashMap;

use super::Dictionary;
use super::parse::{Parser, is_whitespace};
use super::xref::find;

/// 扫描结果里最多记这么多个对象，防止伪造的对象头把内存撑大。
const MAX_OBJECTS: usize = 4_000_000;
/// 对象头后面最多看这么远来判断它是不是 xref 流、对象流或 Catalog。
const PEEK: usize = 2048;
/// 扫描时每隔这么多字节检查一次截止时间。
const CHECK_EVERY: usize = 1 << 20;

#[derive(Default)]
pub(crate) struct Scan {
    /// 对象号 → (偏移, 代号)。
    pub(crate) objects: HashMap<u32, (usize, u16)>,
    /// 按出现顺序的 `trailer` 字典。
    pub(crate) trailers: Vec<Dictionary>,
    pub(crate) xref_streams: Vec<u32>,
    pub(crate) object_streams: Vec<u32>,
    pub(crate) catalogs: Vec<u32>,
}

pub(crate) fn scan(data: &[u8]) -> Scan {
    let mut out = Scan::default();
    let mut pos = 0;
    let mut line_start = true;
    // 某处往后已经找不到 endstream 时，后面所有的 stream 都不用再找（否则大量没有
    // endstream 的 stream 关键字会让扫描变成平方复杂度，模糊测试找到的）
    let mut endstream_left = true;
    // 每扫 1 MB 看一次截止时间；超时就带着已经扫到的部分返回，由调用方报超时
    let mut next_check = CHECK_EVERY;
    while pos < data.len() {
        if pos >= next_check {
            if crate::budget::expired() {
                break;
            }
            next_check = pos + CHECK_EVERY;
        }
        let b = data[pos];
        if b == b's'
            && endstream_left
            && data[pos..].starts_with(b"stream")
            && !data[..pos].ends_with(b"end")
            && matches!(data.get(pos + 6), Some(b'\r' | b'\n'))
        {
            // 没有 endstream 时逐字节继续，至少后面的对象还能找到
            match find(&data[pos + 6..], b"endstream") {
                Some(rel) => {
                    pos += 6 + rel + 9;
                    line_start = false;
                    continue;
                }
                None => endstream_left = false,
            }
        }
        if b == b't' && data[pos..].starts_with(b"trailer") {
            let mut p = Parser::new(data, pos + 7);
            p.skip_ws();
            if data[p.pos..].starts_with(b"<<") {
                p.pos += 2;
                out.trailers.push(p.dict_body(1));
                pos = p.pos;
                line_start = false;
                continue;
            }
        }
        if line_start && b.is_ascii_digit() {
            let mut p = Parser::new(data, pos);
            if let Some((num, generation)) = p.object_header()
                && out.objects.len() < MAX_OBJECTS
            {
                out.objects.insert(num, (pos, generation));
                classify(&data[p.pos..(p.pos + PEEK).min(data.len())], num, &mut out);
                pos = p.pos;
                line_start = false;
                continue;
            }
        }
        line_start = match b {
            b'\r' | b'\n' => true,
            b' ' | b'\t' => line_start,
            _ => false,
        };
        // `endobj 5 0 obj` 写在同一行的情况
        if b == b'j' && pos >= 5 && &data[pos - 5..=pos] == b"endobj" {
            line_start = true;
        }
        pos += 1;
    }
    out
}

/// 只看对象开头的字典部分（到 `stream` 或 `endobj` 为止）。
fn classify(head: &[u8], num: u32, out: &mut Scan) {
    let end = [find(head, b"stream"), find(head, b"endobj")]
        .into_iter()
        .flatten()
        .min()
        .unwrap_or(head.len());
    let dict = &head[..end];
    let has_type = |name: &[u8]| {
        let mut i = 0;
        while let Some(rel) = find(&dict[i..], b"/Type") {
            let mut j = i + rel + 5;
            while j < dict.len() && is_whitespace(dict[j]) {
                j += 1;
            }
            if dict[j..].starts_with(name)
                && dict
                    .get(j + name.len())
                    .is_none_or(|&c| !c.is_ascii_alphanumeric())
            {
                return true;
            }
            i = j;
        }
        false
    };
    if has_type(b"/XRef") {
        out.xref_streams.push(num);
    } else if has_type(b"/ObjStm") {
        out.object_streams.push(num);
    } else if has_type(b"/Catalog") {
        out.catalogs.push(num);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn finds_objects_and_skips_stream_payloads() {
        let data = b"%PDF-1.7\n1 0 obj\n<< /Type /Catalog /Pages 2 0 R >>\nendobj\n\
2 0 obj << /Length 20 >>\nstream\n9 0 obj fake\nendstream\nendobj 3 0 obj\n(x)\nendobj\n\
1 0 obj\n<< /Type /Catalog /Pages 2 0 R /V 2 >>\nendobj\ntrailer\n<< /Root 1 0 R >>\n";
        let s = scan(data);
        assert_eq!(s.objects.len(), 3);
        assert!(!s.objects.contains_key(&9));
        // 1 号出现两次，以后面的为准
        let (off, _) = s.objects[&1];
        assert!(data[off..].starts_with(b"1 0 obj\n<< /Type /Catalog /Pages 2 0 R /V"));
        assert_eq!(s.trailers.len(), 1);
        assert_eq!(s.catalogs, vec![1, 1]);
    }
}
