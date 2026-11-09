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
    /// Include Base64 data in human-readable output for the opt-in asset API.
    pub inline_data: bool,
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
            inline_data: false,
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
            inline_data: false,
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

/// Identify an image from its bytes rather than a filename or HTTP header.
/// SVG has no binary magic; this recognizes an SVG root but is not a sanitizer.
pub fn sniff_image_format(body: &[u8]) -> Option<&'static str> {
    let starts = |sig: &[u8]| body.starts_with(sig);
    if starts(b"\x89PNG\r\n\x1a\n") {
        Some("png")
    } else if starts(&[0xff, 0xd8, 0xff]) {
        Some("jpg")
    } else if starts(b"GIF87a") || starts(b"GIF89a") {
        Some("gif")
    } else if starts(b"RIFF") && body.get(8..12) == Some(b"WEBP") {
        Some("webp")
    } else if body.get(4..12) == Some(b"ftypavif") {
        Some("avif")
    } else if starts(b"BM") {
        Some("bmp")
    } else if starts(&[0, 0, 1, 0]) {
        Some("ico")
    } else if starts(b"II\x2a\0")
        || starts(b"MM\0\x2a")
        || starts(b"II\x2b\0")
        || starts(b"MM\0\x2b")
    {
        Some("tif")
    } else if starts(b"\0\0\0\x0cjP  \r\n\x87\n") {
        Some("jp2")
    } else if starts(&[0xff, 0x4f, 0xff, 0x51]) {
        Some("j2k")
    } else if starts(b"\x97JB2\r\n\x1a\n") {
        Some("jb2")
    } else if starts(&[1, 0, 0, 0]) && body.get(40..44) == Some(b" EMF") {
        Some("emf")
    } else if starts(&[0xd7, 0xcd, 0xc6, 0x9a]) || starts(&[1, 0, 9, 0]) || starts(&[2, 0, 9, 0]) {
        Some("wmf")
    } else if svg_root(body) {
        Some("svg")
    } else {
        None
    }
}

fn svg_root(body: &[u8]) -> bool {
    let mut rest = body.strip_prefix(b"\xef\xbb\xbf").unwrap_or(body);
    loop {
        rest = rest.trim_ascii_start();
        let end = if rest.starts_with(b"<?xml") {
            rest.windows(2).position(|w| w == b"?>").map(|p| p + 2)
        } else if rest.starts_with(b"<!--") {
            rest.windows(3).position(|w| w == b"-->").map(|p| p + 3)
        } else {
            break;
        };
        let Some(end) = end else { return false };
        rest = &rest[end..];
    }
    rest.starts_with(b"<svg")
        && rest
            .get(4)
            .is_some_and(|b| b.is_ascii_whitespace() || matches!(b, b'>' | b'/'))
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
        let human = s.is_human_readable();
        let data = self.data.as_deref().filter(|_| !human || self.inline_data);
        // [协议] 固定字段 id、format、bytes、hash 共 4 个
        let fields = 4
            + usize::from(self.source.is_some())
            + usize::from(data.is_some())
            + usize::from(human && self.inline_data)
            + usize::from(self.inline_data);
        let mut st = s.serialize_struct("Attachment", fields)?;
        st.serialize_field("id", &self.id)?;
        st.serialize_field("format", &self.format)?;
        st.serialize_field("bytes", &self.bytes)?;
        st.serialize_field("hash", &self.hash)?;
        if let Some(src) = &self.source {
            st.serialize_field("source", src)?;
        }
        if let Some(d) = data {
            if human {
                let encoded = pagesift_base64::STANDARD
                    .encode_to_string(d)
                    .map_err(serde::ser::Error::custom)?;
                st.serialize_field("data", &encoded)?;
            } else {
                st.serialize_field("data", &Raw(d))?;
            }
        }
        if human && self.inline_data {
            st.serialize_field("mime", self.mime())?;
        }
        if self.inline_data {
            st.serialize_field("inlineData", &true)?;
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
                "data" => {
                    let Bytes(data, encoded) = m.next_value::<Bytes>()?;
                    a.data = Some(data.into());
                    a.inline_data |= encoded;
                }
                "inlineData" => a.inline_data = m.next_value()?,
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
struct Bytes(Vec<u8>, bool);

impl<'de> Deserialize<'de> for Bytes {
    fn deserialize<D: Deserializer<'de>>(d: D) -> Result<Self, D::Error> {
        let human = d.is_human_readable();
        struct V(bool);
        impl<'de> Visitor<'de> for V {
            type Value = Bytes;
            fn expecting(&self, f: &mut std::fmt::Formatter) -> std::fmt::Result {
                f.write_str("bytes")
            }
            fn visit_bytes<E: de::Error>(self, v: &[u8]) -> Result<Bytes, E> {
                if self.0 {
                    pagesift_base64::STANDARD
                        .decode_to_vec(v)
                        .map(|b| Bytes(b, true))
                        .map_err(E::custom)
                } else {
                    Ok(Bytes(v.to_vec(), false))
                }
            }
            fn visit_byte_buf<E: de::Error>(self, v: Vec<u8>) -> Result<Bytes, E> {
                Ok(Bytes(v, false))
            }
            fn visit_seq<A: de::SeqAccess<'de>>(self, mut s: A) -> Result<Bytes, A::Error> {
                let mut out = Vec::new();
                while let Some(b) = s.next_element::<u8>()? {
                    out.push(b);
                }
                Ok(Bytes(out, false))
            }
            fn visit_str<E: de::Error>(self, v: &str) -> Result<Bytes, E> {
                pagesift_base64::STANDARD
                    .decode_to_vec(v.as_bytes())
                    .map(|bytes| Bytes(bytes, true))
                    .map_err(E::custom)
            }
        }
        d.deserialize_bytes(V(human))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn image_format_comes_from_bytes() {
        assert_eq!(sniff_image_format(b"\x89PNG\r\n\x1a\nrest"), Some("png"));
        assert_eq!(sniff_image_format(b"\xff\xd8\xff\xe0"), Some("jpg"));
        assert_eq!(sniff_image_format(b"GIF89a"), Some("gif"));
        assert_eq!(sniff_image_format(b"II\x2a\0"), Some("tif"));
        assert_eq!(sniff_image_format(b"\x97JB2\r\n\x1a\n"), Some("jb2"));
        assert_eq!(
            sniff_image_format(
                b"<?xml version=\"1.0\"?>\n<svg xmlns=\"http://www.w3.org/2000/svg\"/>"
            ),
            Some("svg")
        );
        assert_eq!(sniff_image_format(b"<svg-not-an-image>"), None);
        assert_eq!(sniff_image_format(b"<html><svg/></html>"), None);
    }

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

    #[test]
    fn opt_in_json_contains_base64_and_round_trips() {
        let mut a = Attachment::new(
            b"\x89PNG\r\n\x1a\nxyz".to_vec(),
            "png",
            Some("https://e.x/a".into()),
        );
        a.inline_data = true;
        let value = serde_json::to_value(&a).unwrap();
        assert_eq!(value["mime"], "image/png");
        assert_eq!(value["data"], "iVBORw0KGgp4eXo=");
        let back: Attachment = serde_json::from_value(value).unwrap();
        assert_eq!(back, a);
        let raw = serde_json::to_vec(&a).unwrap();
        let back: Attachment = serde_json::from_slice(&raw).unwrap();
        assert_eq!(back, a);
    }

    #[test]
    fn deduplicated_attachment_round_trips_without_data() {
        let mut a = Attachment::new(b"image".to_vec(), "png", None);
        a.inline_data = true;
        a.data = None;
        let raw = serde_json::to_vec(&a).unwrap();
        let value: serde_json::Value = serde_json::from_slice(&raw).unwrap();
        assert!(value.get("data").is_none());
        assert_eq!(value["mime"], "image/png");
        let back: Attachment = serde_json::from_slice(&raw).unwrap();
        assert_eq!(back, a);
    }
}
