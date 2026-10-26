//! 自研 PDF 文件层：xref、对象语法、对象流、流解码、标准安全处理器。
//!
//! 按需读取：打开文件只读 xref 和 trailer，用到哪个对象才解析哪个，解析结果缓存；
//! 流数据留在输入缓冲区里不复制，解码时才取出来（加密的流在这时解密）。
//! 结构按 ISO 32000 组织；坏文件的容错参考 pdf.js 和 MuPDF：xref 读不出、偏移不对时
//! 扫描全文重建对象索引。

mod crypt;
mod doc;
mod filter;
mod parse;
mod repair;
mod xref;

pub(crate) use doc::{Document, OpenError};

/// 对象号 + 代号。
pub(crate) type ObjectId = (u32, u16);

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum StringFormat {
    Literal,
    Hexadecimal,
}

#[derive(Debug, Clone, PartialEq)]
pub(crate) enum Object {
    Null,
    Boolean(bool),
    Integer(i64),
    /// 与 lopdf 一致用 f32，字宽、矩阵等数值与原实现逐位相同。
    Real(f32),
    Name(Vec<u8>),
    String(Vec<u8>, StringFormat),
    Array(Vec<Object>),
    Dictionary(Dictionary),
    Stream(Box<Stream>),
    Reference(ObjectId),
}

impl Object {
    pub(crate) fn as_dict(&self) -> Option<&Dictionary> {
        match self {
            Object::Dictionary(d) => Some(d),
            Object::Stream(s) => Some(&s.dict),
            _ => None,
        }
    }

    pub(crate) fn as_i64(&self) -> Option<i64> {
        match self {
            Object::Integer(i) => Some(*i),
            _ => None,
        }
    }

    pub(crate) fn as_name(&self) -> Option<&[u8]> {
        match self {
            Object::Name(n) => Some(n),
            _ => None,
        }
    }
}

/// 字典按出现顺序存放；同一个键出现多次时以最后一次为准（取值从后往前找）。
#[derive(Debug, Clone, Default, PartialEq)]
pub(crate) struct Dictionary(Vec<(Vec<u8>, Object)>);

impl Dictionary {
    pub(crate) fn get(&self, key: &[u8]) -> Option<&Object> {
        self.0.iter().rev().find(|(k, _)| k == key).map(|(_, v)| v)
    }

    pub(crate) fn push(&mut self, key: Vec<u8>, value: Object) {
        self.0.push((key, value));
    }

    /// 直接写在字典里的 `/Type`（不跟随引用，与页树遍历的约定一致）。
    pub(crate) fn type_name(&self) -> Option<&[u8]> {
        self.get(b"Type").and_then(Object::as_name)
    }

    #[cfg(test)]
    pub(crate) fn len(&self) -> usize {
        self.0.len()
    }
}

/// 流：字典 + 数据在输入里的位置。数据只在解码时取出。
#[derive(Debug, Clone, PartialEq)]
pub(crate) struct Stream {
    pub(crate) dict: Dictionary,
    pub(crate) data: StreamData,
    /// 加密文件里需要解密的流记下自己的对象号；不需要解密的为 None。
    pub(crate) crypt_id: Option<ObjectId>,
    /// 流用 /Crypt 过滤器自己指定的加密过滤器名字；None 表示用文档默认的 /StmF。
    pub(crate) crypt_name: Option<Vec<u8>>,
}

#[derive(Debug, Clone, PartialEq)]
pub(crate) enum StreamData {
    /// 输入缓冲区里的 `[start, end)`。
    Range(usize, usize),
    /// 已经在内存里的数据（对象流里不会有流，这里只给测试和内联构造用）。
    #[allow(dead_code)]
    Owned(Vec<u8>),
}
