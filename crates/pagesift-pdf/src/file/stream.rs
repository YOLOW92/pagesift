//! 取流数据：先解密，再按 /Filter 逐层解码。图片导出要停在图像编码那一层（DCT、JPX、CCITT、
//! JBIG2 不解，原样交出），所以把“解密”“过滤器链”“逐层解码”分开。

use std::borrow::Cow;

use super::doc::Document;
use super::filter;
use super::{Dictionary, Object, Stream, StreamData};

/// 图像编码层的过滤器（ISO 32000-2 §7.4.1 表 6 的最后四种）：只取原始字节，不解码。
pub(crate) fn is_image_filter(name: &[u8]) -> bool {
    matches!(
        name,
        b"DCTDecode" | b"DCT" | b"JPXDecode" | b"CCITTFaxDecode" | b"CCF" | b"JBIG2Decode"
    )
}

/// 图片流解到图像编码层为止的结果。
pub(crate) struct ImageData<'d> {
    /// 去掉前面的通用过滤器（Flate、ASCII85 等）之后的字节：图像编码的原始码流，
    /// 或者没有图像编码时解出的像素。
    pub(crate) bytes: Vec<u8>,
    /// 图像编码层的过滤器名和参数；None 表示数据已经是像素。
    pub(crate) codec: Option<(&'d [u8], Option<&'d Dictionary>)>,
}

impl<'a> Document<'a> {
    /// 解密后、还没解码的流数据。
    pub(crate) fn raw_stream<'s>(&'s self, s: &'s Stream) -> Option<Cow<'s, [u8]>> {
        let raw: Cow<'_, [u8]> = match &s.data {
            StreamData::Range(a, b) => Cow::Borrowed(self.data.get(*a..*b)?),
            StreamData::Owned(v) => Cow::Borrowed(v),
        };
        Some(match (s.crypt_id, &self.crypt) {
            (Some(id), Some(c)) => Cow::Owned(match &s.crypt_name {
                Some(name) => c.decrypt_named(name, id, &raw),
                None => c.decrypt_stream(id, &raw),
            }),
            _ => raw,
        })
    }

    /// 过滤器链：每项是过滤器名和它的参数字典。/Filter 数组里有不是名字的项时返回 None。
    pub(crate) fn filters<'s>(
        &'s self,
        s: &'s Stream,
    ) -> Option<Vec<(&'s [u8], Option<&'s Dictionary>)>> {
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
        Some(
            names
                .into_iter()
                .enumerate()
                .map(|(i, n)| (n, params.get(i).copied().flatten()))
                .collect(),
        )
    }

    /// 取流解码后的数据（先解密）。超过 `limit`、或用到不支持的过滤器时返回 None。
    pub(crate) fn stream_data(&self, s: &Stream, limit: usize) -> Option<Vec<u8>> {
        let raw = self.raw_stream(s)?;
        let filters = self.filters(s)?;
        filter::decode(&raw, &filters, limit).ok()
    }

    /// 图片流：解密，解掉图像编码层之前的通用过滤器，图像编码层原样留下。
    /// 图像编码层后面还有过滤器（规范不允许）、或通用过滤器解不开时返回 None。
    pub(crate) fn image_data<'s>(&'s self, s: &'s Stream, limit: usize) -> Option<ImageData<'s>> {
        let raw = self.raw_stream(s)?;
        let filters = self.filters(s)?;
        let cut = filters
            .iter()
            .position(|(n, _)| is_image_filter(n))
            .unwrap_or(filters.len());
        if cut + 1 < filters.len() {
            return None;
        }
        let bytes = filter::decode(&raw, &filters[..cut], limit).ok()?;
        Some(ImageData {
            bytes,
            codec: filters.get(cut).copied(),
        })
    }
}
