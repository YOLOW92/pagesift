//! 缓存里的文档：带格式版本的二进制（`codec`）。头部不对、版本不同或解不出来都当作没有缓存，
//! 所以格式一变，旧缓存整体失效，不会读出半新半旧的结果。
//!
//! 和 JSON 缓存相比多存了两项不进 Firecrawl JSON 的字段（锚文本、hreflang 版本），
//! 缓存命中的页面在爬取时也能用锚文本给链接打分。

use pagesift_api::types::Document;
use serde::{Deserialize, Serialize};

use crate::codec;

/// [协议] 本格式的标识。
const MAGIC: [u8; 4] = *b"PSDC";
/// [协议] 格式版本：字段或编码一变就加一。
const VERSION: u16 = 1;
const HEADER: usize = MAGIC.len() + std::mem::size_of::<u16>();

#[derive(Serialize)]
struct Out<'a> {
    doc: &'a Document,
    link_text: &'a Option<Vec<String>>,
    alternates: &'a [(String, String)],
}

#[derive(Deserialize)]
struct In {
    doc: Document,
    link_text: Option<Vec<String>>,
    alternates: Vec<(String, String)>,
}

pub fn encode(doc: &Document) -> Result<Vec<u8>, codec::Error> {
    let mut out = Vec::with_capacity(HEADER + doc.markdown.as_ref().map_or(0, String::len));
    out.extend_from_slice(&MAGIC);
    out.extend_from_slice(&VERSION.to_le_bytes());
    codec::to_writer(
        &mut out,
        &Out {
            doc,
            link_text: &doc.link_text,
            alternates: &doc.alternates,
        },
    )?;
    Ok(out)
}

/// 解出缓存的文档；不是本版本的格式时返回 `None`。
pub fn decode(bytes: &[u8]) -> Option<Document> {
    let (head, body) = bytes.split_at_checked(HEADER)?;
    if head[..MAGIC.len()] != MAGIC || head[MAGIC.len()..] != VERSION.to_le_bytes() {
        return None;
    }
    let In {
        mut doc,
        link_text,
        alternates,
    } = codec::from_slice(body).ok()?;
    doc.link_text = link_text;
    doc.alternates = alternates;
    Some(doc)
}

#[cfg(test)]
mod tests {
    use super::*;
    use pagesift_api::types::{Metadata, PagesiftMeta};

    #[test]
    fn attachment_bytes_and_asset_flag_survive_binary_encoding() {
        let mut attachment = pagesift_api::types::Attachment::new(b"image".to_vec(), "png", None);
        attachment.inline_data = true;
        let doc = Document {
            attachments: vec![attachment],
            ..Default::default()
        };
        assert_eq!(decode(&encode(&doc).unwrap()), Some(doc));
    }

    #[test]
    fn round_trips_the_fields_json_drops_and_rejects_other_versions() {
        let mut meta = Metadata {
            title: Some("T".into()),
            status_code: 200,
            proxy_used: "basic".into(),
            pagesift: Some(PagesiftMeta::default()),
            ..Default::default()
        };
        meta.extra.insert("ogImage".into(), "x.png".into());
        let doc = Document {
            markdown: Some("# T\n\n\"quoted\"\n".into()),
            links: Some(vec!["https://a/".into()]),
            link_text: Some(vec!["anchor".into()]),
            alternates: vec![("en".into(), "https://a/en".into())],
            metadata: meta,
            ..Default::default()
        };
        let bytes = encode(&doc).unwrap();
        assert_eq!(decode(&bytes), Some(doc.clone()));
        // 旧的 JSON 缓存、别的版本、截断的数据都当作没有缓存
        assert_eq!(decode(&serde_json::to_vec(&doc).unwrap()), None);
        let mut other = bytes.clone();
        other[MAGIC.len()] ^= 1;
        assert_eq!(decode(&other), None);
        assert_eq!(decode(&bytes[..bytes.len() - 1]), None);
    }
}
