//! 文档：按需读取对象并缓存，对象流、解密、流解码都在这里接起来。

use std::borrow::Cow;
use std::cell::{OnceCell, RefCell};
use std::collections::HashMap;

use super::crypt::Crypt;
use super::filter;
use super::parse::Parser;
use super::repair;
use super::xref::{self, Entries, Entry, find, stream_extent};
use super::{Dictionary, Object, ObjectId, Stream, StreamData};

#[derive(Debug)]
pub(crate) enum OpenError {
    /// 加密且空口令打不开（或不是标准安全处理器）。
    Encrypted,
    /// 找不到任何可用的对象结构。
    Malformed(&'static str),
}

/// 引用链最多跟随这么多跳，防止成环。
const MAX_DEREF: usize = 16;
/// 页树最大深度（与 lopdf 相同）。
const MAX_TREE_DEPTH: usize = 256;

struct Slot {
    entry: Entry,
    /// 读过一次就缓存；None 表示读不出来（也缓存，避免反复尝试）。
    cell: OnceCell<Option<Box<Object>>>,
}

pub(crate) struct Document<'a> {
    data: &'a [u8],
    pub(crate) trailer: Dictionary,
    slots: HashMap<u32, Slot>,
    /// 正在读的对象号：读对象 A 时又要读 A（/Length 指向自己、对象流套自己）就当读不出。
    loading: RefCell<Vec<u32>>,
    crypt: Option<Crypt>,
    encrypt_ref: Option<ObjectId>,
    /// 偏移指错时的后备索引，第一次需要时才扫描全文。
    rescue: OnceCell<HashMap<u32, (usize, u16)>>,
    limit: usize,
}

impl<'a> Document<'a> {
    /// 打开文档。`limit` 是单个流解码后的大小上限。
    pub(crate) fn open(data: &'a [u8], limit: usize) -> Result<Document<'a>, OpenError> {
        // 偏移从 %PDF- 算起（文件前面可能有垃圾，与 lopdf、pdf.js 一致）
        let head = &data[..data.len().min(1024)];
        let data = &data[find(head, b"%PDF-").unwrap_or(0)..];
        let normal = match xref::read(data, limit) {
            Some((entries, trailer)) => {
                let mut doc = Document::new(data, entries, trailer, limit);
                doc.setup_crypt()?;
                if doc.healthy() {
                    return Ok(doc);
                }
                Some(doc)
            }
            None => None,
        };
        // 正常读出的文档没有页面时，修复结果有页面才用它
        match (Document::repaired(data, limit), normal) {
            (Some(mut doc), normal) => {
                doc.setup_crypt()?;
                match normal {
                    Some(n) if !doc.healthy() => Ok(n),
                    _ => Ok(doc),
                }
            }
            (None, normal) => normal.ok_or(OpenError::Malformed("no objects found")),
        }
    }

    fn new(data: &'a [u8], entries: Entries, trailer: Dictionary, limit: usize) -> Self {
        let slots = entries
            .into_iter()
            .map(|(num, entry)| {
                let slot = Slot {
                    entry,
                    cell: OnceCell::new(),
                };
                (num, slot)
            })
            .collect();
        Document {
            data,
            trailer,
            slots,
            loading: RefCell::new(Vec::new()),
            crypt: None,
            encrypt_ref: None,
            rescue: OnceCell::new(),
            limit,
        }
    }

    /// 有 Catalog 且页树里至少有一页。
    fn healthy(&self) -> bool {
        !self.pages().is_empty()
    }

    fn setup_crypt(&mut self) -> Result<(), OpenError> {
        let Some(enc) = self.trailer.get(b"Encrypt").cloned() else {
            return Ok(());
        };
        if let Object::Reference(id) = enc {
            self.encrypt_ref = Some(id);
        }
        let mut dict = match self.resolve(&enc) {
            Object::Dictionary(d) => d.clone(),
            _ => return Err(OpenError::Encrypted),
        };
        // /CF 和其中的过滤器字典也可能是间接引用
        if let Some(Object::Dictionary(cf)) = dict.get(b"CF").map(|o| self.resolve(o)) {
            let mut cf = cf.clone();
            for (_, v) in cf.0.iter_mut() {
                *v = self.resolve(v).clone();
            }
            dict.push(b"CF".to_vec(), Object::Dictionary(cf));
        }
        let id0 = match self.trailer.get(b"ID").map(|o| self.resolve(o)) {
            Some(Object::Array(a)) => match a.first().map(|o| self.resolve(o)) {
                Some(Object::String(s, _)) => s.clone(),
                _ => Vec::new(),
            },
            _ => Vec::new(),
        };
        let crypt = Crypt::open(&dict, &id0).map_err(|_| OpenError::Encrypted)?;
        // 解密前可能已经缓存了对象（读 /Encrypt 时），都是未解密的，清掉重读；/Encrypt 本身不加密
        for (num, slot) in self.slots.iter_mut() {
            if Some(*num) != self.encrypt_ref.map(|r| r.0) {
                slot.cell = OnceCell::new();
            }
        }
        self.crypt = Some(crypt);
        Ok(())
    }

    /// 修复模式：全文扫描出对象位置，再从 xref 流和对象流里补上压缩对象，挑一个能用的 trailer。
    fn repaired(data: &'a [u8], limit: usize) -> Option<Self> {
        let scan = repair::scan(data);
        if scan.objects.is_empty() {
            return None;
        }
        let mut entries: Entries = scan
            .objects
            .iter()
            .map(|(&num, &(offset, generation))| (num, Entry::Offset { offset, generation }))
            .collect();
        let probe = Document::new(data, entries.clone(), Dictionary::default(), limit);
        let mut extra: Vec<(u32, Entry)> = Vec::new();
        // xref 流里记着压缩对象的位置
        for &num in &scan.xref_streams {
            if let Some(Object::Stream(s)) = probe.get_object((num, scan.objects[&num].1))
                && let StreamData::Range(a, b) = s.data
            {
                let dict = &s.dict;
                if let Some(list) = xref::xref_stream_entries(dict, &data[a..b], limit) {
                    extra.extend(
                        list.into_iter()
                            .filter(|(_, e)| matches!(e, Entry::Compressed { .. })),
                    );
                }
            }
        }
        // 对象流的头上直接列着里面的对象号
        for &num in &scan.object_streams {
            if let Some(Object::Stream(s)) = probe.get_object((num, scan.objects[&num].1))
                && let Some(bytes) = probe.stream_data(s, limit)
            {
                for (index, (obj, _)) in objstm_header(&s.dict, &bytes).into_iter().enumerate() {
                    extra.push((
                        obj,
                        Entry::Compressed {
                            stream: num,
                            index: index as u32,
                        },
                    ));
                }
            }
        }
        for (num, entry) in extra {
            entries.entry(num).or_insert(entry);
        }
        let trailer = pick_trailer(&probe, &scan);
        drop(probe);
        Some(Document::new(data, entries, trailer, limit))
    }

    /// 按对象号和代号取对象（不跟随引用）。
    pub(crate) fn get_object(&self, id: ObjectId) -> Option<&Object> {
        let slot = self.slots.get(&id.0)?;
        match slot.entry {
            Entry::Free => return None,
            Entry::Offset { generation, .. } if generation != id.1 => return None,
            Entry::Compressed { .. } if id.1 != 0 => return None,
            _ => {}
        }
        if let Some(v) = slot.cell.get() {
            return v.as_deref();
        }
        if self.loading.borrow().contains(&id.0) {
            return None;
        }
        self.loading.borrow_mut().push(id.0);
        let obj = match slot.entry {
            Entry::Offset { offset, .. } => self.load_at(id, offset),
            Entry::Compressed { stream, index } => self.load_compressed(id.0, stream, index),
            Entry::Free => None,
        };
        self.loading.borrow_mut().pop();
        // 读对象流时可能已经顺带填好了这一格
        let _ = slot.cell.set(obj.map(Box::new));
        slot.cell.get().and_then(|v| v.as_deref())
    }

    /// 跟随引用直到得到非引用对象；断链返回 Null。
    pub(crate) fn resolve<'b>(&'b self, mut obj: &'b Object) -> &'b Object {
        for _ in 0..MAX_DEREF {
            match obj {
                Object::Reference(id) => match self.get_object(*id) {
                    Some(o) => obj = o,
                    None => return &Object::Null,
                },
                _ => return obj,
            }
        }
        &Object::Null
    }

    /// 解引用后是字典（不含流）。
    fn dictionary(&self, id: ObjectId) -> Option<&Dictionary> {
        match self.resolve(self.get_object(id)?) {
            Object::Dictionary(d) => Some(d),
            _ => None,
        }
    }

    fn load_at(&self, id: ObjectId, offset: usize) -> Option<Object> {
        let mut p = Parser::new(self.data, offset);
        if p.object_header().is_none_or(|(num, _)| num != id.0) {
            // xref 的偏移不对：查全文扫描的结果
            let &(rescued, _) = self.rescue().get(&id.0)?;
            if rescued == offset {
                return None;
            }
            p = Parser::new(self.data, rescued);
            p.object_header()?;
        }
        let mut obj = p.object().unwrap_or(Object::Null);
        if matches!(obj, Object::Dictionary(_)) && p.eat_keyword(b"stream") {
            let Object::Dictionary(dict) = obj else {
                unreachable!("checked above")
            };
            let (start, end) = stream_extent(self.data, p.pos, self.length(&dict));
            let (crypt_id, crypt_name) = match self.stream_crypt(id, &dict) {
                Some((id, name)) => (Some(id), name),
                None => (None, None),
            };
            obj = Object::Stream(Box::new(Stream {
                dict,
                data: StreamData::Range(start, end),
                crypt_id,
                crypt_name,
            }));
        }
        if let Some(c) = &self.crypt
            && c.strings_encrypted()
            && Some(id) != self.encrypt_ref
        {
            decrypt_strings(&mut obj, &|s| c.decrypt_string(id, s));
        }
        Some(obj)
    }

    fn rescue(&self) -> &HashMap<u32, (usize, u16)> {
        self.rescue.get_or_init(|| repair::scan(self.data).objects)
    }

    /// /Length：直接的整数，或者指向整数的引用；实数写法（`42.`）也认。
    fn length(&self, dict: &Dictionary) -> Option<i64> {
        let v = match dict.get(b"Length")? {
            Object::Reference(id) => self.get_object(*id)?,
            o => o,
        };
        match v {
            Object::Integer(i) => Some(*i),
            Object::Real(r) if r.fract() == 0.0 => Some(*r as i64),
            _ => None,
        }
    }

    /// 这个流要不要解密，用哪个加密过滤器（None 名字表示文档默认的 /StmF）。
    fn stream_crypt(&self, id: ObjectId, dict: &Dictionary) -> Option<(ObjectId, Option<Vec<u8>>)> {
        let crypt = self.crypt.as_ref()?;
        match dict.type_name() {
            Some(b"XRef") => return None,
            Some(b"Metadata") if !crypt.encrypt_metadata => return None,
            _ => {}
        }
        // /Filter 里的 /Crypt 用对应 DecodeParms 的 /Name 指定加密过滤器，缺省是 Identity（不加密）
        let names: Vec<&Object> = match dict.get(b"Filter").map(|o| self.resolve(o)) {
            Some(Object::Array(a)) => a.iter().map(|o| self.resolve(o)).collect(),
            Some(o) => vec![o],
            None => Vec::new(),
        };
        let Some(pos) = names.iter().position(|o| o.as_name() == Some(b"Crypt")) else {
            return Some((id, None));
        };
        let params = match dict.get(b"DecodeParms").map(|o| self.resolve(o)) {
            Some(Object::Array(a)) => a.get(pos).map(|o| self.resolve(o)),
            Some(o) => Some(o),
            None => None,
        };
        let name = match params {
            Some(Object::Dictionary(p)) => p.get(b"Name").and_then(Object::as_name),
            _ => None,
        };
        match name {
            None | Some(b"Identity") => None,
            Some(n) => Some((id, Some(n.to_vec()))),
        }
    }

    fn load_compressed(&self, num: u32, stream: u32, index: u32) -> Option<Object> {
        let Object::Stream(s) = self.get_object((stream, 0))? else {
            return None;
        };
        let bytes = self.stream_data(s, self.limit)?;
        let header = objstm_header(&s.dict, &bytes);
        let first = s
            .dict
            .get(b"First")
            .and_then(Object::as_i64)
            .and_then(|v| usize::try_from(v).ok())?;
        let parse = |offset: usize| {
            let at = first.checked_add(offset)?;
            Parser::new(&bytes, at).object()
        };
        // 同一个对象流里的其他对象顺带读好（它们迟早要用，流只解一次）
        let loading = self.loading.borrow().clone();
        for (k, &(other, offset)) in header.iter().enumerate() {
            if other == num || loading.contains(&other) {
                continue;
            }
            if let Some(slot) = self.slots.get(&other)
                && let Entry::Compressed {
                    stream: s2,
                    index: i2,
                } = slot.entry
                && s2 == stream
                && (i2 as usize == k || header.get(i2 as usize).is_none_or(|h| h.0 != other))
                && slot.cell.get().is_none()
            {
                let _ = slot.cell.set(parse(offset).map(Box::new));
            }
        }
        let offset = match header.get(index as usize) {
            Some(&(n, off)) if n == num => off,
            _ => header.iter().find(|h| h.0 == num)?.1,
        };
        parse(offset)
    }

    /// 取流解码后的数据（先解密）。超过 `limit`、或用到不支持的过滤器时返回 None。
    pub(crate) fn stream_data(&self, s: &Stream, limit: usize) -> Option<Vec<u8>> {
        let raw: Cow<'_, [u8]> = match &s.data {
            StreamData::Range(a, b) => Cow::Borrowed(self.data.get(*a..*b)?),
            StreamData::Owned(v) => Cow::Borrowed(v),
        };
        let raw = match (s.crypt_id, &self.crypt) {
            (Some(id), Some(c)) => Cow::Owned(match &s.crypt_name {
                Some(name) => c.decrypt_named(name, id, &raw),
                None => c.decrypt_stream(id, &raw),
            }),
            _ => raw,
        };
        let names: Vec<&[u8]> = match s.dict.get(b"Filter").map(|o| self.resolve(o)) {
            Some(Object::Name(n)) => vec![n.as_slice()],
            Some(Object::Array(a)) => a
                .iter()
                .map(|o| self.resolve(o).as_name())
                .collect::<Option<_>>()?,
            _ => Vec::new(),
        };
        let params: Vec<Option<&Dictionary>> =
            match s.dict.get(b"DecodeParms").map(|o| self.resolve(o)) {
                // 只有一份参数字典时每一层都用它（只有 Flate/LZW 读参数），与 lopdf 一致
                Some(Object::Dictionary(d)) => vec![Some(d); names.len()],
                Some(Object::Array(a)) => a
                    .iter()
                    .map(|o| match self.resolve(o) {
                        Object::Dictionary(d) => Some(d),
                        _ => None,
                    })
                    .collect(),
                _ => Vec::new(),
            };
        let filters: Vec<(&[u8], Option<&Dictionary>)> = names
            .into_iter()
            .enumerate()
            .map(|(i, n)| (n, params.get(i).copied().flatten()))
            .collect();
        filter::decode(&raw, &filters, limit).ok()
    }

    /// 页树里的页面，按顺序。规则与 lopdf 的页树遍历相同：
    /// 子节点必须是引用，`/Type` 必须直接写成 /Page 或 /Pages，深度不超过 256。
    pub(crate) fn pages(&self) -> Vec<ObjectId> {
        let mut out = Vec::new();
        let Some(Object::Reference(root)) = self.trailer.get(b"Root") else {
            return out;
        };
        let Some(Object::Reference(tree)) = self.dictionary(*root).and_then(|c| c.get(b"Pages"))
        else {
            return out;
        };
        let kids_of = |id: ObjectId| -> Option<&[Object]> {
            match self.resolve(self.dictionary(id)?.get(b"Kids")?) {
                Object::Array(a) => Some(a.as_slice()),
                _ => None,
            }
        };
        let mut budget = self.slots.len();
        let mut stack: Vec<&[Object]> = Vec::new();
        let mut kids = kids_of(*tree);
        loop {
            while let Some((kid, rest)) = kids.and_then(|k| k.split_first()) {
                if budget == 0 {
                    return out;
                }
                budget -= 1;
                kids = Some(rest);
                let Object::Reference(id) = kid else { continue };
                match self.dictionary(*id).and_then(Dictionary::type_name) {
                    Some(b"Page") => out.push(*id),
                    Some(b"Pages") if stack.len() < MAX_TREE_DEPTH => {
                        if !rest.is_empty() {
                            stack.push(rest);
                        }
                        kids = kids_of(*id);
                    }
                    _ => {}
                }
            }
            match stack.pop() {
                Some(k) => kids = Some(k),
                None => return out,
            }
        }
    }
}

/// 对象流头部：`N` 对 `对象号 偏移`。
fn objstm_header(dict: &Dictionary, bytes: &[u8]) -> Vec<(u32, usize)> {
    let n = dict
        .get(b"N")
        .and_then(Object::as_i64)
        .unwrap_or(0)
        .clamp(0, 1 << 20) as usize;
    let mut p = Parser::new(bytes, 0);
    let mut out = Vec::with_capacity(n.min(4096));
    for _ in 0..n {
        let (Some(num), Some(off)) = (p.uint(), p.uint()) else {
            break;
        };
        let (Ok(num), Ok(off)) = (u32::try_from(num), usize::try_from(off)) else {
            break;
        };
        out.push((num, off));
    }
    out
}

fn decrypt_strings(obj: &mut Object, f: &dyn Fn(&[u8]) -> Vec<u8>) {
    match obj {
        Object::String(s, _) => *s = f(s),
        Object::Array(a) => a.iter_mut().for_each(|o| decrypt_strings(o, f)),
        Object::Dictionary(d) => d.0.iter_mut().for_each(|(_, o)| decrypt_strings(o, f)),
        Object::Stream(s) => s.dict.0.iter_mut().for_each(|(_, o)| decrypt_strings(o, f)),
        _ => {}
    }
}

/// 修复模式下挑 trailer：从后往前第一个 /Root 能解出字典的 `trailer`；没有就看 xref 流的字典；
/// 再没有就用扫描到的最后一个 Catalog 现造一个。缺的 /Info、/Encrypt、/ID 从别的候选补。
fn pick_trailer(probe: &Document<'_>, scan: &repair::Scan) -> Dictionary {
    let mut candidates: Vec<Dictionary> = scan.trailers.iter().rev().cloned().collect();
    for &num in scan.xref_streams.iter().rev() {
        if let Some(Object::Stream(s)) = probe.get_object((num, scan.objects[&num].1)) {
            candidates.push(s.dict.clone());
        }
    }
    let root_ok = |d: &Dictionary| match d.get(b"Root") {
        Some(Object::Reference(id)) => probe.dictionary(*id).is_some(),
        _ => false,
    };
    let mut trailer = match candidates.iter().position(root_ok) {
        Some(i) => candidates[i].clone(),
        None => {
            let mut t = Dictionary::default();
            if let Some(&num) = scan.catalogs.last() {
                t.push(
                    b"Root".to_vec(),
                    Object::Reference((num, scan.objects[&num].1)),
                );
            }
            t
        }
    };
    for key in [&b"Info"[..], b"Encrypt", b"ID"] {
        if trailer.get(key).is_none()
            && let Some(v) = candidates.iter().find_map(|c| c.get(key))
        {
            trailer.push(key.to_vec(), v.clone());
        }
    }
    trailer
}
