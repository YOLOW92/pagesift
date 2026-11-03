//! 附件：随页面一起导出的文件（PDF 内嵌图片、Office 媒体、网页图片）。
//!
//! 对外的 JSON 只有 `{id, format, bytes, hash}`（加上来源 `source`）；文件内容不进 JSON，
//! HTTP API 用单独的接口下载（`GET /v2/attachments/{id}`）。在 pagesift 内部（缓存、多服务器的
//! 任务结果）用非人类可读的二进制编码时连同内容一起编码，结果交给网关时附件不丢。

use std::sync::Arc;

use serde::de::{self, Deserializer, MapAccess, Visitor};
use serde::ser::{SerializeStruct, Serializer};
use serde::{Deserialize, Serialize};

/// 一个附件。`id` 是内容寻址的文件名：SHA-256 的前 32 位十六进制加扩展名（与 PDF、Office 的
/// 导出文件名同一规则），同一内容只有一个 id。
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct Attachment {
    pub id: String,
    /// 格式（文件扩展名）：jpg、png、gif、webp、svg、jp2、tif、jb2……
    pub format: String,
    /// 字节数。
    pub bytes: u64,
    /// 内容的 SHA-256（小写十六进制）。
    pub hash: String,
    /// 来源：网页图片的原地址、Office 文档里的部件路径（与 Markdown 里的引用一致）；PDF 图片为空
    /// （Markdown 里直接引用 `images/<id>`）。
    pub source: Option<String>,
    /// 文件内容。
    pub data: Option<Arc<[u8]>>,
}

impl Attachment {
    /// 由内容构造：算哈希、定 id。
    pub fn new(data: Vec<u8>, format: &str, source: Option<String>) -> Self {
        let hash = sha256_hex(&data);
        Self {
            id: format!("{}.{format}", &hash[..32]), // [协议] 与 PDF/Office 导出同一规则：32 位十六进制 = 128 位哈希前缀
            format: format.to_string(),
            bytes: data.len() as u64,
            hash,
            source,
            data: Some(data.into()),
        }
    }

    /// 已经算好哈希（PDF、Office 的导出）时直接构造。
    pub fn with_hash(hash: String, format: &str, data: Vec<u8>, source: Option<String>) -> Self {
        Self {
            id: format!("{}.{format}", &hash[..hash.len().min(32)]), // [协议] 同上
            format: format.to_string(),
            bytes: data.len() as u64,
            hash,
            source,
            data: Some(data.into()),
        }
    }

    /// 按格式给的 MIME 类型（下载接口的 Content-Type）。
    pub fn mime(&self) -> &'static str {
        mime_of(&self.format)
    }
}

/// 扩展名 → MIME 类型（IANA 媒体类型注册表）。
pub fn mime_of(format: &str) -> &'static str {
    match format {
        "jpg" | "jpeg" => "image/jpeg",
        "png" => "image/png",
        "gif" => "image/gif",
        "webp" => "image/webp",
        "svg" => "image/svg+xml",
        "bmp" => "image/bmp",
        "avif" => "image/avif",
        "ico" => "image/vnd.microsoft.icon",
        "tif" | "tiff" => "image/tiff",
        "jp2" => "image/jp2",
        "j2k" => "image/j2c",
        "jb2" => "image/x-jbig2",
        "emf" => "image/emf",
        "wmf" => "image/wmf",
        _ => "application/octet-stream",
    }
}

fn sha256_hex(data: &[u8]) -> String {
    let digest = ring::digest::digest(&ring::digest::SHA256, data);
    digest.as_ref().iter().map(|b| format!("{b:02x}")).collect()
}

/// 原样写成 serde 的字节串（二进制编码里是一个长度前缀 + 原样字节，不是逐字节的数组）。
struct Raw<'a>(&'a [u8]);

impl Serialize for Raw<'_> {
    fn serialize<S: Serializer>(&self, s: S) -> Result<S::Ok, S::Error> {
        s.serialize_bytes(self.0)
    }
}

impl Serialize for Attachment {
    fn serialize<S: Serializer>(&self, s: S) -> Result<S::Ok, S::Error> {
        // 人类可读（JSON）的出口只列元数据；内部二进制编码连内容一起带上
        let data = self.data.as_deref().filter(|_| !s.is_human_readable());
        // [协议] 固定字段 id、format、bytes、hash 共 4 个
        let fields = 4 + usize::from(self.source.is_some()) + usize::from(data.is_some());
        let mut st = s.serialize_struct("Attachment", fields)?;
        st.serialize_field("id", &self.id)?;
        st.serialize_field("format", &self.format)?;
        st.serialize_field("bytes", &self.bytes)?;
        st.serialize_field("hash", &self.hash)?;
        if let Some(src) = &self.source {
            st.serialize_field("source", src)?;
        }
        if let Some(d) = data {
            st.serialize_field("data", &Raw(d))?;
        }
        st.end()
    }
}

impl<'de> Deserialize<'de> for Attachment {
    fn deserialize<D: Deserializer<'de>>(d: D) -> Result<Self, D::Error> {
        d.deserialize_map(AttachmentVisitor)
    }
}

struct AttachmentVisitor;

impl<'de> Visitor<'de> for AttachmentVisitor {
    type Value = Attachment;

    fn expecting(&self, f: &mut std::fmt::Formatter) -> std::fmt::Result {
        f.write_str("an attachment")
    }

    fn visit_map<A: MapAccess<'de>>(self, mut m: A) -> Result<Attachment, A::Error> {
        let mut a = Attachment::default();
        while let Some(key) = m.next_key::<String>()? {
            match key.as_str() {
                "id" => a.id = m.next_value()?,
                "format" => a.format = m.next_value()?,
                "bytes" => a.bytes = m.next_value()?,
                "hash" => a.hash = m.next_value()?,
                "source" => a.source = m.next_value()?,
                "data" => a.data = Some(m.next_value::<Bytes>()?.0.into()),
                _ => {
                    m.next_value::<de::IgnoredAny>()?;
                }
            }
        }
        if a.id.is_empty() {
            return Err(de::Error::missing_field("id"));
        }
        Ok(a)
    }
}

/// 读字节串：二进制编码给 bytes，自描述格式也可能给数组。
struct Bytes(Vec<u8>);

impl<'de> Deserialize<'de> for Bytes {
    fn deserialize<D: Deserializer<'de>>(d: D) -> Result<Self, D::Error> {
        struct V;
        impl<'de> Visitor<'de> for V {
            type Value = Bytes;
            fn expecting(&self, f: &mut std::fmt::Formatter) -> std::fmt::Result {
                f.write_str("bytes")
            }
            fn visit_bytes<E: de::Error>(self, v: &[u8]) -> Result<Bytes, E> {
                Ok(Bytes(v.to_vec()))
            }
            fn visit_byte_buf<E: de::Error>(self, v: Vec<u8>) -> Result<Bytes, E> {
                Ok(Bytes(v))
            }
            fn visit_seq<A: de::SeqAccess<'de>>(self, mut s: A) -> Result<Bytes, A::Error> {
                let mut out = Vec::new();
                while let Some(b) = s.next_element::<u8>()? {
                    out.push(b);
                }
                Ok(Bytes(out))
            }
        }
        d.deserialize_bytes(V)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn json_lists_metadata_only() {
        let a = Attachment::new(
            b"\x89PNG\r\n\x1a\nxyz".to_vec(),
            "png",
            Some("https://e.x/a.png".into()),
        );
        assert_eq!(a.id.len(), 32 + 4);
        assert_eq!(a.bytes, 11);
        let v = serde_json::to_value(&a).unwrap();
        assert_eq!(v["format"], "png");
        assert_eq!(v["hash"].as_str().unwrap().len(), 64);
        assert!(v.get("data").is_none(), "内容不进 JSON");
        let back: Attachment = serde_json::from_value(v).unwrap();
        assert_eq!(back.data, None);
        assert_eq!(back.id, a.id);
    }
}
