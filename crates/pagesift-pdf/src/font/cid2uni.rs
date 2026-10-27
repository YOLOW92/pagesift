//! 预定义字符集的 CID → Unicode（ISO 32000-1 §9.10.2）：复合字体没有 ToUnicode、
//! CIDSystemInfo 是 Adobe-GB1 / CNS1 / Japan1 / Korea1 时，按字符集本身的定义取码位。
//! LaTeX（ctex、Fandol 字体）、不少排版软件输出的中文 PDF 都是这样，不查这张表中文会整段丢掉。
//!
//! 数据来自 Adobe cmap-resources 的 cid2code.txt（BSD-3-Clause，见 cid2uni/LICENSE-Adobe.md），
//! 取 Adobe 为每个 CID 选定的 UCS-2 码位，存成连续区段：(起始 CID u16, 长度 u16, 起始码位 u32)，小端，按 CID 排好。
//! 生成脚本：tasks/0.80/scripts/gen_cid2uni.py。

/// 预定义字符集。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum Collection {
    Gb1,
    Cns1,
    Japan1,
    Korea1,
}

impl Collection {
    /// 按 CIDSystemInfo 的 Registry 和 Ordering 认字符集。
    pub(crate) fn from_system_info(registry: &[u8], ordering: &[u8]) -> Option<Self> {
        if registry != b"Adobe" {
            return None;
        }
        match ordering {
            b"GB1" => Some(Self::Gb1),
            b"CNS1" => Some(Self::Cns1),
            b"Japan1" => Some(Self::Japan1),
            b"Korea1" => Some(Self::Korea1),
            _ => None,
        }
    }

    fn data(self) -> &'static [u8] {
        match self {
            Self::Gb1 => include_bytes!("cid2uni/GB1.bin"),
            Self::Cns1 => include_bytes!("cid2uni/CNS1.bin"),
            Self::Japan1 => include_bytes!("cid2uni/Japan1.bin"),
            Self::Korea1 => include_bytes!("cid2uni/Korea1.bin"),
        }
    }

    /// CID 对应的字符；字符集里没有定义的返回 None。
    pub(crate) fn unicode(self, cid: u32) -> Option<char> {
        let data = self.data();
        let rec = |i: usize| {
            let r = &data[i * 8..i * 8 + 8];
            (
                u32::from(u16::from_le_bytes([r[0], r[1]])),
                u32::from(u16::from_le_bytes([r[2], r[3]])),
                u32::from_le_bytes([r[4], r[5], r[6], r[7]]),
            )
        };
        let n = data.len() / 8;
        // 最后一个起始 CID ≤ cid 的区段
        let (mut lo, mut hi) = (0usize, n);
        while lo < hi {
            let mid = (lo + hi) / 2;
            if rec(mid).0 <= cid {
                lo = mid + 1;
            } else {
                hi = mid;
            }
        }
        let (start, len, uni) = rec(lo.checked_sub(1)?);
        (cid < start + len)
            .then(|| char::from_u32(uni + (cid - start)))
            .flatten()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn known_cids() {
        // Adobe-GB1：CID 1 是空格，CID 4559 是“中”（UniGB-UCS2 里 4E2D）
        assert_eq!(Collection::Gb1.unicode(1), Some(' '));
        assert_eq!(Collection::Gb1.unicode(4559), Some('中'));
        assert_eq!(Collection::Gb1.unicode(0), None);
        // Adobe-Japan1：CID 843 是“あ”
        assert_eq!(Collection::Japan1.unicode(843), Some('あ'));
    }
}
