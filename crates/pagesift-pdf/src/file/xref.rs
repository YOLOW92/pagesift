//! 交叉引用（ISO 32000-1 §7.5.4–7.5.8）：xref 表、xref 流、混合引用、增量更新的 /Prev 链。
//!
//! 读不出来时返回错误，由调用方改走全文扫描重建（见 `repair`）。

use std::collections::{HashMap, HashSet};

use super::filter;
use super::parse::Parser;
use super::{Dictionary, Object};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum Entry {
    Free,
    /// 对象在文件里的偏移和代号。
    Offset {
        offset: usize,
        generation: u16,
    },
    /// 对象在对象流 `stream` 里的第 `index` 个。
    Compressed {
        stream: u32,
        index: u32,
    },
}

pub(crate) type Entries = HashMap<u32, Entry>;

/// 一个文件最多跟随这么多段 xref（增量更新、线性化、混合引用都算），防止 /Prev 成环。
const MAX_SECTIONS: usize = 1024;

/// 从 `startxref` 出发读完整条 xref 链。返回合并后的条目（新修订优先）和最新的 trailer
/// （旧 trailer 里有而新的没有的键补进来）。
pub(crate) fn read(data: &[u8], limit: usize) -> Option<(Entries, Dictionary)> {
    let start = startxref(data)?;
    let mut entries = Entries::new();
    let mut trailer: Option<Dictionary> = None;
    let mut queue = vec![start];
    let mut seen = HashSet::new();
    while let Some(offset) = queue.pop() {
        if !seen.insert(offset) || seen.len() > MAX_SECTIONS {
            continue;
        }
        let offset = correct_offset(data, offset);
        let (section, dict) = read_section(data, offset, limit)?;
        for (num, entry) in section {
            entries.entry(num).or_insert(entry);
        }
        // 先读 /Prev（压栈在下面），再读同一修订里的 /XRefStm：栈是后进先出
        if let Some(prev) = dict.get(b"Prev").and_then(offset_value) {
            queue.push(prev);
        }
        if let Some(stm) = dict.get(b"XRefStm").and_then(offset_value) {
            queue.push(stm);
        }
        match &mut trailer {
            None => trailer = Some(dict),
            Some(t) => {
                for (k, v) in dict.0 {
                    if t.get(&k).is_none() && k != b"Prev" && k != b"XRefStm" {
                        t.push(k, v);
                    }
                }
            }
        }
    }
    Some((entries, trailer?))
}

fn offset_value(o: &Object) -> Option<usize> {
    match o {
        Object::Integer(i) => usize::try_from(*i).ok(),
        Object::Real(r) if *r >= 0.0 && r.fract() == 0.0 => Some(*r as usize),
        _ => None,
    }
}

/// 文件末尾的 `startxref N`。从后往前找，只看最后 4 KB（有的文件在 %%EOF 后面还有垃圾）。
fn startxref(data: &[u8]) -> Option<usize> {
    let tail_start = data.len().saturating_sub(4096);
    let rel = data[tail_start..]
        .windows(9)
        .rposition(|w| w == b"startxref")?;
    let mut p = Parser::new(data, tail_start + rel + 9);
    p.skip_ws();
    usize::try_from(p.uint()?).ok()
}

/// 偏移没指向 `xref` 或 `N G obj` 时，在附近 64 字节里找最近的 `xref` 关键字（常见的写偏一行）。
fn correct_offset(data: &[u8], offset: usize) -> usize {
    const WINDOW: usize = 64;
    if offset >= data.len() {
        return offset;
    }
    let rest = &data[offset..];
    if rest.starts_with(b"xref") || Parser::new(data, offset).object_header().is_some() {
        return offset;
    }
    let lo = offset.saturating_sub(WINDOW);
    let hi = (offset + WINDOW).min(data.len());
    (lo..hi.saturating_sub(4))
        .filter(|&p| data[p..].starts_with(b"xref") && !(p >= 5 && &data[p - 5..p] == b"start"))
        .min_by_key(|&p| p.abs_diff(offset))
        .unwrap_or(offset)
}

/// 读一段：`xref` 表 + `trailer`，或一个 xref 流对象。
fn read_section(
    data: &[u8],
    offset: usize,
    limit: usize,
) -> Option<(Vec<(u32, Entry)>, Dictionary)> {
    let mut p = Parser::new(data, offset);
    if p.eat_keyword(b"xref") {
        return table(&mut p);
    }
    p.object_header()?;
    let Some(Object::Dictionary(dict)) = p.object() else {
        return None;
    };
    if !p.eat_keyword(b"stream") {
        return None;
    }
    let length = dict.get(b"Length").and_then(Object::as_i64);
    let (start, end) = stream_extent(data, p.pos, length);
    let entries = xref_stream_entries(&dict, &data[start..end], limit)?;
    Some((entries, dict))
}

/// xref 表：若干小节，每节 `起始号 个数`，后面每行 `偏移 代号 n|f`。按词读，不要求每行恰好 20 字节。
fn table(p: &mut Parser<'_>) -> Option<(Vec<(u32, Entry)>, Dictionary)> {
    let mut out = Vec::new();
    loop {
        if p.eat_keyword(b"trailer") {
            break;
        }
        let first = p.uint()?;
        let count = p.uint()?;
        let mut first = u32::try_from(first).ok()?;
        for i in 0..count {
            let offset = p.uint()?;
            let generation = p.uint()?;
            p.skip_ws();
            let kind = if p.eat_keyword(b"n") {
                b'n'
            } else if p.eat_keyword(b"f") {
                b'f'
            } else {
                return None;
            };
            // 常见错误：小节从 1 开始，但第一条其实是 0 号空闲项
            if i == 0 && first == 1 && kind == b'f' && offset == 0 && generation == 65535 {
                first = 0;
            }
            let Some(num) = u32::try_from(i).ok().and_then(|i| first.checked_add(i)) else {
                continue;
            };
            let entry = match (kind, u16::try_from(generation), usize::try_from(offset)) {
                (b'n', Ok(generation), Ok(offset)) => Entry::Offset { offset, generation },
                (b'n', ..) => continue,
                _ => Entry::Free,
            };
            out.push((num, entry));
        }
    }
    let Some(Object::Dictionary(dict)) = p.object() else {
        return None;
    };
    // 表里后出现的同号条目覆盖先出现的：倒过来，合并时先到先得
    out.reverse();
    Some((out, dict))
}

/// xref 流：/W 给出三栏宽度，/Index 给出号段（缺省为 [0 Size]）。
pub(crate) fn xref_stream_entries(
    dict: &Dictionary,
    raw: &[u8],
    limit: usize,
) -> Option<Vec<(u32, Entry)>> {
    let w: Vec<usize> = match dict.get(b"W") {
        Some(Object::Array(a)) => a
            .iter()
            .map(|o| o.as_i64().and_then(|v| usize::try_from(v).ok()))
            .collect::<Option<_>>()?,
        _ => return None,
    };
    if w.len() < 3 || w.iter().any(|&n| n > 8) {
        return None;
    }
    let filters = direct_filters(dict);
    let bytes = filter::decode(raw, &filters, limit).ok()?;
    let size = dict
        .get(b"Size")
        .and_then(Object::as_i64)
        .unwrap_or(0)
        .max(0);
    let index: Vec<i64> = match dict.get(b"Index") {
        Some(Object::Array(a)) => a.iter().filter_map(Object::as_i64).collect(),
        _ => vec![0, size],
    };
    let row = w[0] + w[1] + w[2];
    if row == 0 {
        return None;
    }
    let mut rows = bytes.chunks_exact(row);
    let mut out = Vec::new();
    for pair in index.as_chunks::<2>().0 {
        let (Ok(first), Ok(count)) = (u32::try_from(pair[0]), u64::try_from(pair[1])) else {
            continue;
        };
        for i in 0..count {
            let Some(r) = rows.next() else {
                break;
            };
            let field = |k: usize| -> u64 {
                let start: usize = w[..k].iter().sum();
                r[start..start + w[k]]
                    .iter()
                    .fold(0u64, |v, &b| v << 8 | u64::from(b))
            };
            let kind = if w[0] == 0 { 1 } else { field(0) };
            let Some(num) = u32::try_from(i).ok().and_then(|i| first.checked_add(i)) else {
                break;
            };
            let entry = match kind {
                0 => Entry::Free,
                1 => match (usize::try_from(field(1)), u16::try_from(field(2))) {
                    (Ok(offset), Ok(generation)) => Entry::Offset { offset, generation },
                    _ => continue,
                },
                2 => match (u32::try_from(field(1)), u32::try_from(field(2))) {
                    (Ok(stream), Ok(index)) => Entry::Compressed { stream, index },
                    _ => continue,
                },
                _ => continue,
            };
            out.push((num, entry));
        }
    }
    out.reverse();
    Some(out)
}

/// 只认直接写出的过滤器和参数（xref 流不允许间接引用，读它时也还没有 xref 可查）。
pub(crate) fn direct_filters(dict: &Dictionary) -> Vec<(&[u8], Option<&Dictionary>)> {
    let names: Vec<&[u8]> = match dict.get(b"Filter") {
        Some(Object::Name(n)) => vec![n.as_slice()],
        Some(Object::Array(a)) => a.iter().filter_map(Object::as_name).collect(),
        _ => Vec::new(),
    };
    let params: Vec<Option<&Dictionary>> = match dict.get(b"DecodeParms") {
        Some(Object::Array(a)) => a.iter().map(Object::as_dict).collect(),
        Some(Object::Dictionary(d)) => vec![Some(d); names.len()],
        _ => Vec::new(),
    };
    names
        .into_iter()
        .enumerate()
        .map(|(i, n)| (n, params.get(i).copied().flatten()))
        .collect()
}

/// `stream` 关键字之后（`pos`）的数据范围。/Length 对得上（后面紧跟 `endstream`）就用它；
/// 否则往后找 `endstream`（或者 `endobj`），去掉数据末尾的一个行尾。
pub(crate) fn stream_extent(data: &[u8], pos: usize, length: Option<i64>) -> (usize, usize) {
    let mut start = pos;
    while matches!(data.get(start), Some(b' ' | b'\t')) {
        start += 1;
    }
    if data.get(start) == Some(&b'\r') {
        start += 1;
    }
    if data.get(start) == Some(&b'\n') {
        start += 1;
    }
    let start = start.min(data.len());
    if let Some(len) = length.and_then(|l| usize::try_from(l).ok())
        && let Some(end) = start.checked_add(len)
        && end <= data.len()
    {
        let mut q = end;
        while q < data.len() && q < end + 32 && super::parse::is_whitespace(data[q]) {
            q += 1;
        }
        if data[q..].starts_with(b"endstream") {
            return (start, end);
        }
    }
    let rest = &data[start..];
    let found = find(rest, b"endstream").or_else(|| find(rest, b"endobj"));
    let mut end = found.map_or(data.len(), |i| start + i);
    if end > start && data[end - 1] == b'\n' {
        end -= 1;
    }
    if end > start && data[end - 1] == b'\r' {
        end -= 1;
    }
    (start, end)
}

pub(crate) fn find(hay: &[u8], needle: &[u8]) -> Option<usize> {
    hay.windows(needle.len()).position(|w| w == needle)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn build(body: &[(u32, &str)], trailer_extra: &str) -> Vec<u8> {
        let mut out = b"%PDF-1.4\n".to_vec();
        let mut offsets = Vec::new();
        for (num, text) in body {
            offsets.push((*num, out.len()));
            out.extend_from_slice(format!("{num} 0 obj\n{text}\nendobj\n").as_bytes());
        }
        let xref = out.len();
        let max = body.iter().map(|(n, _)| *n).max().unwrap_or(0);
        out.extend_from_slice(format!("xref\n0 {}\n0000000000 65535 f \n", max + 1).as_bytes());
        for n in 1..=max {
            match offsets.iter().find(|(m, _)| *m == n) {
                Some((_, o)) => out.extend_from_slice(format!("{o:010} 00000 n \n").as_bytes()),
                None => out.extend_from_slice(b"0000000000 65535 f \n"),
            }
        }
        out.extend_from_slice(
            format!(
                "trailer\n<< /Size {} /Root 1 0 R {trailer_extra}>>\nstartxref\n{xref}\n%%EOF\n",
                max + 1
            )
            .as_bytes(),
        );
        out
    }

    #[test]
    fn reads_classic_table() {
        let pdf = build(&[(1, "<< /Type /Catalog >>"), (2, "(x)")], "");
        let (entries, trailer) = read(&pdf, 1 << 20).unwrap();
        assert!(matches!(
            entries.get(&1),
            Some(Entry::Offset { generation: 0, .. })
        ));
        assert_eq!(entries.get(&0), Some(&Entry::Free));
        assert_eq!(trailer.get(b"Root"), Some(&Object::Reference((1, 0))));
    }

    #[test]
    fn tolerates_off_by_line_startxref() {
        let mut pdf = build(&[(1, "<< /Type /Catalog >>")], "");
        let s = String::from_utf8_lossy(&pdf).to_string();
        let xref = s.find("xref\n0").unwrap();
        let fixed = s.replace(
            &format!("startxref\n{xref}\n"),
            &format!("startxref\n{}\n", xref + 5),
        );
        pdf = fixed.into_bytes();
        assert!(read(&pdf, 1 << 20).is_some());
    }

    #[test]
    fn stream_extent_recovers_wrong_length() {
        let data = b"stream\r\nHELLO\r\nendstream";
        assert_eq!(stream_extent(data, 6, Some(5)), (8, 13));
        assert_eq!(stream_extent(data, 6, Some(99)), (8, 13));
        assert_eq!(stream_extent(data, 6, None), (8, 13));
    }
}
