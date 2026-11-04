//! PowerPoint 97–2003（.ppt，MS-PPT）取文字：`PowerPoint Document` 流是嵌套记录。
//! 按持久化目录走当前版本的幻灯片（增量保存留下的旧内容不进结果）；目录读不出来时退回顺序扫描。
//! 母版的占位文字不要，备注排在所在幻灯片后面。

use std::collections::{BTreeMap, HashMap, HashSet};

use crate::md::{self, Block};
use crate::ole::Ole;
use crate::{Converted, Error, Options, budget, check_deadline};

const SLIDE: u16 = 0x03EE;
const NOTES: u16 = 0x03F0;
const NOTES_ATOM: u16 = 0x03F1;
const SLIDE_PERSIST: u16 = 0x03F3;
const MAIN_MASTER: u16 = 0x03F8;
const DOCUMENT: u16 = 0x03E8;
const SLIDE_LIST: u16 = 0x0FF0;
const USER_EDIT: u16 = 0x0FF5;
const PERSIST_DIR: u16 = 0x1772;
const TEXT_CHARS: u16 = 0x0FA0;
const TEXT_BYTES: u16 = 0x0FA8;

fn u32_at(d: &[u8], i: usize) -> Option<u32> {
    Some(u32::from_le_bytes(d.get(i..i + 4)?.try_into().ok()?))
}

struct Rec<'a> {
    container: bool,
    kind: u16,
    instance: u16,
    body: &'a [u8],
    next: usize,
}

fn rec_at(d: &[u8], p: usize) -> Option<Rec<'_>> {
    let h = d.get(p..p + 8)?;
    let ver = u16::from_le_bytes([h[0], h[1]]);
    let kind = u16::from_le_bytes([h[2], h[3]]);
    let len = u32::from_le_bytes([h[4], h[5], h[6], h[7]]) as usize;
    let end = (p + 8).saturating_add(len).min(d.len());
    Some(Rec {
        container: ver & 0xF == 0xF,
        kind,
        instance: ver >> 4,
        body: &d[p + 8..end],
        next: end,
    })
}

/// 文字原子转成字符串。`\r` 分段，`\u{b}` 换行。
fn atom_text(r: &Rec<'_>) -> Option<String> {
    match r.kind {
        TEXT_CHARS => {
            let units: Vec<u16> = r
                .body
                .as_chunks::<2>()
                .0
                .iter()
                .map(|c| u16::from_le_bytes([c[0], c[1]]))
                .collect();
            Some(String::from_utf16_lossy(&units))
        }
        TEXT_BYTES => Some(r.body.iter().map(|&b| char::from(b)).collect()),
        _ => None,
    }
}

/// 收集一个记录树里的文字原子。嵌套层数有上限，防恶意构造。
fn collect(d: &[u8], out: &mut Vec<String>, depth: usize) {
    if depth > 32 {
        return;
    }
    let mut p = 0usize;
    while let Some(r) = rec_at(d, p) {
        if r.next <= p {
            break;
        }
        if let Some(t) = atom_text(&r) {
            out.push(t);
        } else if r.container {
            collect(r.body, out, depth + 1);
        }
        p = r.next;
    }
}

/// 持久化目录：对象号 → 流内偏移。从 CurrentUser 指向的最新 UserEdit 往前走，先见到的版本优先。
fn persist_map(stream: &[u8], mut edit_at: usize) -> Option<(BTreeMap<u32, usize>, u32)> {
    let mut map = BTreeMap::new();
    let mut doc_ref = None;
    let mut seen = HashSet::new();
    loop {
        if !seen.insert(edit_at) {
            break;
        }
        let r = rec_at(stream, edit_at)?;
        if r.kind != USER_EDIT {
            return None;
        }
        let last = u32_at(r.body, 8)? as usize;
        let dir_at = u32_at(r.body, 12)? as usize;
        if doc_ref.is_none() {
            doc_ref = u32_at(r.body, 16);
        }
        let dir = rec_at(stream, dir_at)?;
        if dir.kind != PERSIST_DIR {
            return None;
        }
        let mut p = 0usize;
        while let Some(entry) = u32_at(dir.body, p) {
            let first = entry & 0xF_FFFF;
            let count = entry >> 20;
            p += 4;
            for k in 0..count {
                let Some(off) = u32_at(dir.body, p) else {
                    break;
                };
                p += 4;
                map.entry(first + k).or_insert(off as usize);
            }
        }
        if last == 0 {
            break;
        }
        edit_at = last;
    }
    Some((map, doc_ref?))
}

pub(crate) fn convert(bytes: &[u8], opts: &Options<'_>) -> Result<Converted, Error> {
    let ole = Ole::open(bytes)?;
    let root = ole.children(None);
    if root.iter().any(|e| e.name == "EncryptedSummary") {
        return Err(Error::Encrypted);
    }
    let entry = root
        .iter()
        .find(|e| e.is_stream && e.name.eq_ignore_ascii_case("PowerPoint Document"))
        .ok_or_else(|| Error::Malformed("no PowerPoint Document stream".into()))?;
    let stream = ole.read(entry)?;
    check_deadline(opts)?;
    // CurrentUser 流第 16 字节是最新 UserEdit 的偏移
    let current = root
        .iter()
        .find(|e| e.is_stream && e.name == "Current User")
        .and_then(|e| ole.read(e).ok())
        .and_then(|c| u32_at(&c, 16));
    let slides = current
        .and_then(|at| persist_map(&stream, at as usize))
        .and_then(|(map, doc)| by_persist(&stream, &map, doc))
        .unwrap_or_else(|| by_scan(&stream));
    let mut blocks = Vec::new();
    for (i, texts) in slides.iter().enumerate() {
        check_deadline(opts)?;
        blocks.push(Block::Raw(format!("<!-- Slide {} -->", i + 1)));
        let mut seen = HashSet::new();
        for t in texts {
            for para in t.split(['\r', '\u{b}']) {
                let para = para.trim();
                if para.is_empty() || !seen.insert(para.to_owned()) {
                    continue;
                }
                budget::spend(para.len());
                blocks.push(Block::Para(md::escape(para)));
            }
        }
        if budget::exhausted() {
            break;
        }
    }
    let mut out = Converted {
        markdown: if blocks.is_empty() {
            String::new()
        } else {
            md::render(&blocks)
        },
        parts: slides.len(),
        ..Default::default()
    };
    super::props::apply(&ole, &mut out);
    Ok(out)
}

/// 当前版本：Document 的 SlideListWithText（实例 0 是幻灯片、2 是备注）列出每页的持久化号，
/// 页面容器里是文本框，列表里跟着的是占位符文字。备注按 slideIdRef 接到所在幻灯片后面。
fn by_persist(stream: &[u8], map: &BTreeMap<u32, usize>, doc_ref: u32) -> Option<Vec<Vec<String>>> {
    let doc = rec_at(stream, *map.get(&doc_ref)?)?;
    if doc.kind != DOCUMENT {
        return None;
    }
    let mut slides: Vec<(u32, Vec<String>)> = Vec::new();
    let mut notes: HashMap<u32, Vec<String>> = HashMap::new();
    let mut p = 0usize;
    while let Some(r) = rec_at(doc.body, p) {
        if r.next <= p {
            break;
        }
        if r.kind == SLIDE_LIST && matches!(r.instance, 0 | 2) {
            let mut q = 0usize;
            while let Some(a) = rec_at(r.body, q) {
                if a.next <= q {
                    break;
                }
                if a.kind == SLIDE_PERSIST {
                    let page = u32_at(a.body, 0)
                        .and_then(|id| map.get(&id))
                        .and_then(|&at| rec_at(stream, at));
                    if r.instance == 0 {
                        let mut texts = Vec::new();
                        if let Some(page) = page.filter(|pg| pg.kind == SLIDE) {
                            collect(page.body, &mut texts, 0);
                        }
                        slides.push((u32_at(a.body, 12).unwrap_or(0), texts));
                    } else if let Some(page) = page.filter(|pg| pg.kind == NOTES) {
                        let mut texts = Vec::new();
                        collect(page.body, &mut texts, 0);
                        if let Some(id) = notes_slide_id(page.body) {
                            notes.entry(id).or_default().extend(texts);
                        }
                    }
                } else if r.instance == 0
                    && let Some(t) = atom_text(&a)
                    && let Some((_, last)) = slides.last_mut()
                {
                    last.insert(0, t);
                }
                q = a.next;
            }
        }
        p = r.next;
    }
    if slides.is_empty() {
        return None;
    }
    Some(
        slides
            .into_iter()
            .map(|(id, mut texts)| {
                if let Some(n) = notes.remove(&id) {
                    texts.extend(n);
                }
                texts
            })
            .collect(),
    )
}

/// 备注页属于哪张幻灯片：NotesAtom 的第一个字段。
fn notes_slide_id(body: &[u8]) -> Option<u32> {
    let mut p = 0usize;
    while let Some(r) = rec_at(body, p) {
        if r.next <= p {
            break;
        }
        if r.kind == NOTES_ATOM {
            return u32_at(r.body, 0);
        }
        p = r.next;
    }
    None
}

/// 没有可用目录时按出现顺序扫：每个 Slide 容器一页，母版跳过。旧版本的内容可能混进来。
fn by_scan(stream: &[u8]) -> Vec<Vec<String>> {
    let mut slides = Vec::new();
    let mut p = 0usize;
    while let Some(r) = rec_at(stream, p) {
        if r.next <= p {
            break;
        }
        match r.kind {
            SLIDE | NOTES => {
                let mut texts = Vec::new();
                collect(r.body, &mut texts, 0);
                if r.kind == SLIDE || !texts.is_empty() {
                    slides.push(texts);
                }
            }
            MAIN_MASTER => {}
            _ => {}
        }
        p = r.next;
    }
    slides
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::ole::tests::{Node, build};

    fn record(ver: u16, kind: u16, body: &[u8]) -> Vec<u8> {
        let mut out = Vec::with_capacity(body.len() + 8);
        out.extend_from_slice(&ver.to_le_bytes());
        out.extend_from_slice(&kind.to_le_bytes());
        out.extend_from_slice(&(body.len() as u32).to_le_bytes());
        out.extend_from_slice(body);
        out
    }

    fn utf16(s: &str) -> Vec<u8> {
        s.encode_utf16().flat_map(u16::to_le_bytes).collect()
    }

    #[test]
    fn scan_reads_slides_and_skips_masters() {
        let text = record(0, TEXT_CHARS, &utf16("Title\rBody \u{4e2d}\u{6587}"));
        let bytes_atom = record(0, TEXT_BYTES, b"More");
        let slide = record(0xF, SLIDE, &[text, bytes_atom].concat());
        let master = record(0xF, MAIN_MASTER, &record(0, TEXT_BYTES, b"Click to edit"));
        let stream = [master, slide.clone(), slide].concat();
        let bytes = build(&[Node {
            name: "PowerPoint Document",
            data: &stream,
            storage: false,
            parent: 0,
        }]);
        let d = convert(&bytes, &Options::default()).unwrap();
        assert_eq!(d.parts, 2);
        assert_eq!(
            d.markdown,
            "<!-- Slide 1 -->\n\nTitle\n\nBody \u{4e2d}\u{6587}\n\nMore\n\n<!-- Slide 2 -->\n\nTitle\n\nBody \u{4e2d}\u{6587}\n\nMore\n"
        );
    }

    #[test]
    fn persist_directory_picks_the_current_version() {
        // 旧版幻灯片在流里但不被目录引用
        let old = record(0xF, SLIDE, &record(0, TEXT_BYTES, b"Old text"));
        let new = record(0xF, SLIDE, &record(0, TEXT_BYTES, b"New text"));
        let new_at = old.len() as u32;
        let persist = record(0, SLIDE_PERSIST, &1u32.to_le_bytes());
        let list = record(0xF, SLIDE_LIST, &persist);
        let doc = record(0xF, DOCUMENT, &list);
        let doc_at = new_at + new.len() as u32;
        let dir_body = [
            (1u32 | 2 << 20).to_le_bytes(),
            new_at.to_le_bytes(),
            doc_at.to_le_bytes(),
        ]
        .concat();
        let dir = record(0, PERSIST_DIR, &dir_body);
        let dir_at = doc_at + doc.len() as u32;
        let edit_body = [
            0u32.to_le_bytes(),
            0u32.to_le_bytes(),
            0u32.to_le_bytes(),
            dir_at.to_le_bytes(),
            2u32.to_le_bytes(),
        ]
        .concat();
        let edit = record(0, USER_EDIT, &edit_body);
        let edit_at = dir_at + dir.len() as u32;
        let stream = [old, new, doc, dir, edit].concat();
        let mut current = vec![0u8; 20];
        current[16..20].copy_from_slice(&edit_at.to_le_bytes());
        let bytes = build(&[
            Node {
                name: "PowerPoint Document",
                data: &stream,
                storage: false,
                parent: 0,
            },
            Node {
                name: "Current User",
                data: &current,
                storage: false,
                parent: 0,
            },
        ]);
        let d = convert(&bytes, &Options::default()).unwrap();
        assert_eq!(d.markdown, "<!-- Slide 1 -->\n\nNew text\n");
    }

    #[test]
    fn hostile_records_stop() {
        // 声明的长度超过流、深层嵌套
        let mut deep = record(0, TEXT_BYTES, b"x");
        for _ in 0..64 {
            deep = record(0xF, SLIDE, &deep);
        }
        let mut stream = deep;
        stream.extend_from_slice(&[0xFF; 7]);
        let bytes = build(&[Node {
            name: "PowerPoint Document",
            data: &stream,
            storage: false,
            parent: 0,
        }]);
        assert!(convert(&bytes, &Options::default()).is_ok());
    }
}
