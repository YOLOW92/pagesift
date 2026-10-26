//! 对象流的解析范围（模糊测试找到的内存爆炸）。

use super::Document;
use super::Object;

/// 拼一个带 xref 流的 PDF：对象流 2 号里放 `count` 个对象号（10、11、…），偏移由 `offsets` 给出。
fn pdf_with_objstm(offsets: &[usize], body: &str) -> Vec<u8> {
    let mut out = b"%PDF-1.5\n".to_vec();
    let o1 = out.len();
    out.extend_from_slice(b"1 0 obj\n<< /Type /Catalog >>\nendobj\n");
    let header: String = offsets
        .iter()
        .enumerate()
        .map(|(i, off)| format!("{} {off} ", 10 + i))
        .collect();
    let data = format!("{header}{body}");
    let o2 = out.len();
    out.extend_from_slice(
        format!(
            "2 0 obj\n<< /Type /ObjStm /N {} /First {} /Length {} >>\nstream\n{data}\nendstream\nendobj\n",
            offsets.len(),
            header.len(),
            data.len()
        )
        .as_bytes(),
    );
    // xref 流：/W [1 4 2]，0 号空闲，1、2、3 号在文件里，10 号起在对象流 2 里
    let o3 = out.len();
    let size = 10 + offsets.len();
    let mut rows: Vec<u8> = Vec::new();
    let mut row = |t: u8, f2: u32, f3: u16| {
        rows.push(t);
        rows.extend_from_slice(&f2.to_be_bytes());
        rows.extend_from_slice(&f3.to_be_bytes());
    };
    row(0, 0, 65535);
    row(1, o1 as u32, 0);
    row(1, o2 as u32, 0);
    row(1, o3 as u32, 0);
    for _ in 4..10 {
        row(0, 0, 0);
    }
    for i in 0..offsets.len() {
        row(2, 2, i as u16);
    }
    out.extend_from_slice(
        format!(
            "3 0 obj\n<< /Type /XRef /Size {size} /W [1 4 2] /Root 1 0 R /Length {} >>\nstream\n",
            rows.len()
        )
        .as_bytes(),
    );
    out.extend_from_slice(&rows);
    out.extend_from_slice(format!("\nendstream\nendobj\nstartxref\n{o3}\n%%EOF\n").as_bytes());
    out
}

#[test]
fn objects_end_at_next_offset() {
    let pdf = pdf_with_objstm(&[0, 4], "(a) (b)");
    let doc = Document::open(&pdf, 1 << 20).ok().unwrap();
    assert!(matches!(doc.get_object((10, 0)), Some(Object::String(s, _)) if s == b"a"));
    assert!(matches!(doc.get_object((11, 0)), Some(Object::String(s, _)) if s == b"b"));
}

/// 几百个对象号指向同一个偏移的一大段：只有头部里第一个算数，其余读不出来，
/// 不再每个都解析、缓存一份（曾经 44 万字节的文件吃掉 3 GB 以上）。
#[test]
fn duplicate_offsets_parsed_once() {
    let body = format!("[{}]", "/F ".repeat(2000));
    let pdf = pdf_with_objstm(&[0; 500], &body);
    let doc = Document::open(&pdf, 1 << 20).ok().unwrap();
    assert!(matches!(doc.get_object((10, 0)), Some(Object::Array(a)) if a.len() == 2000));
    for n in 11..510 {
        assert!(doc.get_object((n, 0)).is_none(), "{n}");
    }
}
