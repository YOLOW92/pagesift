//! 嵌入字体程序里的内建编码：没有 /Encoding 也没有 ToUnicode 的字体（TeX 的 CMMI、CMSY、CMEX、
//! 用 OT1 编码的 CMR 等）只能从字体程序本身知道每个字符码是哪个字形。做法和 pdf.js、poppler 一样：
//! Type1（FontFile）读明文段里的 `dup <码> /<字形名> put`；CFF（FontFile3/Type1C）按规范读
//! charset 和 Encoding，得到 字符码 → 字形号 → 字形名。

use lopdf::{Dictionary, Document, Object};

use crate::obj::{get, get_num, stream_bytes};

/// 字符码 0..=255 对应的字形名。字体程序用标准编码、或者读不出来时返回 None。
pub(crate) fn builtin_encoding(
    doc: &Document,
    descriptor: &Dictionary,
    limit: usize,
) -> Option<Vec<Option<String>>> {
    if let Ok(obj @ Object::Reference(_) | obj @ Object::Stream(_)) = descriptor.get(b"FontFile") {
        let length1 = match crate::obj::deref(doc, obj) {
            Object::Stream(s) => get_num(doc, &s.dict, b"Length1"),
            _ => None,
        };
        let bytes = stream_bytes(doc, obj, limit)?;
        let clear = match length1 {
            Some(n) if n > 0.0 && (n as usize) <= bytes.len() => &bytes[..n as usize],
            _ => &bytes[..],
        };
        // PFB 封装：段头 0x80 0x01 + 长度
        let clear = if clear.len() > 6 && clear[0] == 0x80 && clear[1] == 1 {
            &clear[6..]
        } else {
            clear
        };
        return type1(clear);
    }
    let obj = get(doc, descriptor, b"FontFile3")?;
    let bytes = stream_bytes(doc, obj, limit)?;
    cff(&bytes)
}

/// Type1 明文段的 /Encoding。
fn type1(clear: &[u8]) -> Option<Vec<Option<String>>> {
    let at = find(clear, b"/Encoding")?;
    let rest = &clear[at + 9..];
    let head = &rest[..rest.len().min(40)];
    if find(head, b"StandardEncoding").is_some() {
        return None;
    }
    let mut out = vec![None; 256];
    let mut found = false;
    let end = find(rest, b"readonly def")
        .or_else(|| find(rest, b"currentdict end"))
        .or_else(|| find(rest, b"eexec"))
        .unwrap_or(rest.len());
    let text = String::from_utf8_lossy(&rest[..end]);
    let toks: Vec<&str> = text.split_ascii_whitespace().collect();
    for w in toks.windows(4) {
        if w[0] == "dup"
            && w[3] == "put"
            && let Ok(code) = w[1].parse::<usize>()
            && let Some(name) = w[2].strip_prefix('/')
            && code < 256
        {
            out[code] = Some(name.to_string());
            found = true;
        }
    }
    found.then_some(out)
}

fn find(hay: &[u8], needle: &[u8]) -> Option<usize> {
    hay.windows(needle.len()).position(|w| w == needle)
}

struct Reader<'a> {
    b: &'a [u8],
}

impl<'a> Reader<'a> {
    fn u8(&self, at: usize) -> Option<usize> {
        self.b.get(at).map(|&v| v as usize)
    }

    fn u16(&self, at: usize) -> Option<usize> {
        Some(self.u8(at)? << 8 | self.u8(at + 1)?)
    }

    fn offset(&self, at: usize, size: usize) -> Option<usize> {
        (0..size).try_fold(0usize, |v, i| Some(v << 8 | self.u8(at + i)?))
    }

    /// INDEX：返回各项的区间和 INDEX 之后的位置。
    fn index(&self, at: usize) -> Option<(Vec<&'a [u8]>, usize)> {
        let count = self.u16(at)?;
        if count == 0 {
            return Some((Vec::new(), at + 2));
        }
        let size = self.u8(at + 2)?;
        if !(1..=4).contains(&size) {
            return None;
        }
        let offs = at + 3;
        let data = offs + (count + 1) * size - 1;
        let mut items = Vec::with_capacity(count);
        for i in 0..count {
            let a = data + self.offset(offs + i * size, size)?;
            let b = data + self.offset(offs + (i + 1) * size, size)?;
            items.push(self.b.get(a..b)?);
        }
        let end = data + self.offset(offs + count * size, size)?;
        Some((items, end))
    }
}

/// DICT 里的 (操作符, 操作数)。
fn dict(b: &[u8]) -> Vec<(u16, Vec<f64>)> {
    let mut out = Vec::new();
    let mut ops: Vec<f64> = Vec::new();
    let mut i = 0;
    while i < b.len() {
        let b0 = b[i];
        match b0 {
            0..=21 => {
                let op = if b0 == 12 {
                    i += 1;
                    1200 + u16::from(*b.get(i).unwrap_or(&0))
                } else {
                    u16::from(b0)
                };
                out.push((op, std::mem::take(&mut ops)));
                i += 1;
            }
            28 => {
                let v =
                    i16::from_be_bytes([*b.get(i + 1).unwrap_or(&0), *b.get(i + 2).unwrap_or(&0)]);
                ops.push(f64::from(v));
                i += 3;
            }
            29 => {
                let mut v = [0u8; 4];
                for (k, x) in v.iter_mut().enumerate() {
                    *x = *b.get(i + 1 + k).unwrap_or(&0);
                }
                ops.push(f64::from(i32::from_be_bytes(v)));
                i += 5;
            }
            30 => {
                // 实数：半字节直到 0xf，这里只需要跳过
                i += 1;
                while i < b.len() {
                    let v = b[i];
                    i += 1;
                    if v & 0x0f == 0x0f || v >> 4 == 0x0f {
                        break;
                    }
                }
                ops.push(0.0);
            }
            32..=246 => {
                ops.push(f64::from(b0) - 139.0);
                i += 1;
            }
            247..=250 => {
                let b1 = f64::from(*b.get(i + 1).unwrap_or(&0));
                ops.push((f64::from(b0) - 247.0) * 256.0 + b1 + 108.0);
                i += 2;
            }
            251..=254 => {
                let b1 = f64::from(*b.get(i + 1).unwrap_or(&0));
                ops.push(-(f64::from(b0) - 251.0) * 256.0 - b1 - 108.0);
                i += 2;
            }
            _ => i += 1,
        }
    }
    out
}

/// CFF 的内建编码。CID 字体和用标准编码的字体返回 None。
fn cff(b: &[u8]) -> Option<Vec<Option<String>>> {
    let r = Reader { b };
    let hdr = r.u8(2)?;
    let (_, after_name) = r.index(hdr)?;
    let (tops, after_top) = r.index(after_name)?;
    let (strings, _) = r.index(after_top)?;
    let top = dict(tops.first()?);
    let operand = |op: u16| {
        top.iter()
            .find(|(o, _)| *o == op)
            .and_then(|(_, v)| v.first().copied())
    };
    if top.iter().any(|(o, _)| *o == 1230) {
        return None; // ROS：CID 字体
    }
    let enc_off = operand(16).unwrap_or(0.0) as usize;
    if enc_off <= 1 {
        return None; // 标准编码或 Expert 编码
    }
    let charstrings = operand(17)? as usize;
    let n_glyphs = r.u16(charstrings)?;
    let charset_off = operand(15).unwrap_or(0.0) as usize;
    let name = |sid: usize| -> Option<String> {
        if sid < STANDARD_STRINGS.len() {
            Some(STANDARD_STRINGS[sid].to_string())
        } else {
            strings
                .get(sid - STANDARD_STRINGS.len())
                .map(|s| String::from_utf8_lossy(s).into_owned())
        }
    };
    // charset：字形号 → SID（0 号字形是 .notdef）
    let mut sids = vec![0usize; n_glyphs];
    if charset_off == 0 {
        for (g, s) in sids.iter_mut().enumerate() {
            *s = g; // ISOAdobe：SID 和字形号一致
        }
    } else if charset_off > 2 {
        let format = r.u8(charset_off)?;
        let mut at = charset_off + 1;
        let mut g = 1;
        match format {
            0 => {
                while g < n_glyphs {
                    sids[g] = r.u16(at)?;
                    at += 2;
                    g += 1;
                }
            }
            1 | 2 => {
                while g < n_glyphs {
                    let first = r.u16(at)?;
                    let left = if format == 1 {
                        r.u8(at + 2)?
                    } else {
                        r.u16(at + 2)?
                    };
                    at += if format == 1 { 3 } else { 4 };
                    for k in 0..=left {
                        if g >= n_glyphs {
                            break;
                        }
                        sids[g] = first + k;
                        g += 1;
                    }
                }
            }
            _ => return None,
        }
    } else {
        return None;
    }
    let mut out: Vec<Option<String>> = vec![None; 256];
    let format = r.u8(enc_off)?;
    let mut at = enc_off + 1;
    match format & 0x7f {
        0 => {
            let n = r.u8(at)?;
            for g in 1..=n {
                let code = r.u8(at + g)?;
                if let Some(&sid) = sids.get(g) {
                    out[code] = name(sid);
                }
            }
            at += 1 + n;
        }
        1 => {
            let n = r.u8(at)?;
            let mut g = 1;
            for k in 0..n {
                let first = r.u8(at + 1 + 2 * k)?;
                let left = r.u8(at + 2 + 2 * k)?;
                for slot in out.iter_mut().take((first + left).min(255) + 1).skip(first) {
                    if let Some(&sid) = sids.get(g) {
                        *slot = name(sid);
                    }
                    g += 1;
                }
            }
            at += 1 + 2 * n;
        }
        _ => return None,
    }
    if format & 0x80 != 0 {
        let n = r.u8(at)?;
        for k in 0..n {
            let code = r.u8(at + 1 + 3 * k)?;
            let sid = r.u16(at + 2 + 3 * k)?;
            out[code] = name(sid);
        }
    }
    Some(out)
}

/// CFF 规范附录 A 的标准字符串（SID 0..=390）。
const STANDARD_STRINGS: [&str; 391] = [
    ".notdef",
    "space",
    "exclam",
    "quotedbl",
    "numbersign",
    "dollar",
    "percent",
    "ampersand",
    "quoteright",
    "parenleft",
    "parenright",
    "asterisk",
    "plus",
    "comma",
    "hyphen",
    "period",
    "slash",
    "zero",
    "one",
    "two",
    "three",
    "four",
    "five",
    "six",
    "seven",
    "eight",
    "nine",
    "colon",
    "semicolon",
    "less",
    "equal",
    "greater",
    "question",
    "at",
    "A",
    "B",
    "C",
    "D",
    "E",
    "F",
    "G",
    "H",
    "I",
    "J",
    "K",
    "L",
    "M",
    "N",
    "O",
    "P",
    "Q",
    "R",
    "S",
    "T",
    "U",
    "V",
    "W",
    "X",
    "Y",
    "Z",
    "bracketleft",
    "backslash",
    "bracketright",
    "asciicircum",
    "underscore",
    "quoteleft",
    "a",
    "b",
    "c",
    "d",
    "e",
    "f",
    "g",
    "h",
    "i",
    "j",
    "k",
    "l",
    "m",
    "n",
    "o",
    "p",
    "q",
    "r",
    "s",
    "t",
    "u",
    "v",
    "w",
    "x",
    "y",
    "z",
    "braceleft",
    "bar",
    "braceright",
    "asciitilde",
    "exclamdown",
    "cent",
    "sterling",
    "fraction",
    "yen",
    "florin",
    "section",
    "currency",
    "quotesingle",
    "quotedblleft",
    "guillemotleft",
    "guilsinglleft",
    "guilsinglright",
    "fi",
    "fl",
    "endash",
    "dagger",
    "daggerdbl",
    "periodcentered",
    "paragraph",
    "bullet",
    "quotesinglbase",
    "quotedblbase",
    "quotedblright",
    "guillemotright",
    "ellipsis",
    "perthousand",
    "questiondown",
    "grave",
    "acute",
    "circumflex",
    "tilde",
    "macron",
    "breve",
    "dotaccent",
    "dieresis",
    "ring",
    "cedilla",
    "hungarumlaut",
    "ogonek",
    "caron",
    "emdash",
    "AE",
    "ordfeminine",
    "Lslash",
    "Oslash",
    "OE",
    "ordmasculine",
    "ae",
    "dotlessi",
    "lslash",
    "oslash",
    "oe",
    "germandbls",
    "onesuperior",
    "logicalnot",
    "mu",
    "trademark",
    "Eth",
    "onehalf",
    "plusminus",
    "Thorn",
    "onequarter",
    "divide",
    "brokenbar",
    "degree",
    "thorn",
    "threequarters",
    "twosuperior",
    "registered",
    "minus",
    "eth",
    "multiply",
    "threesuperior",
    "copyright",
    "Aacute",
    "Acircumflex",
    "Adieresis",
    "Agrave",
    "Aring",
    "Atilde",
    "Ccedilla",
    "Eacute",
    "Ecircumflex",
    "Edieresis",
    "Egrave",
    "Iacute",
    "Icircumflex",
    "Idieresis",
    "Igrave",
    "Ntilde",
    "Oacute",
    "Ocircumflex",
    "Odieresis",
    "Ograve",
    "Otilde",
    "Scaron",
    "Uacute",
    "Ucircumflex",
    "Udieresis",
    "Ugrave",
    "Yacute",
    "Ydieresis",
    "Zcaron",
    "aacute",
    "acircumflex",
    "adieresis",
    "agrave",
    "aring",
    "atilde",
    "ccedilla",
    "eacute",
    "ecircumflex",
    "edieresis",
    "egrave",
    "iacute",
    "icircumflex",
    "idieresis",
    "igrave",
    "ntilde",
    "oacute",
    "ocircumflex",
    "odieresis",
    "ograve",
    "otilde",
    "scaron",
    "uacute",
    "ucircumflex",
    "udieresis",
    "ugrave",
    "yacute",
    "ydieresis",
    "zcaron",
    "exclamsmall",
    "Hungarumlautsmall",
    "dollaroldstyle",
    "dollarsuperior",
    "ampersandsmall",
    "Acutesmall",
    "parenleftsuperior",
    "parenrightsuperior",
    "twodotenleader",
    "onedotenleader",
    "zerooldstyle",
    "oneoldstyle",
    "twooldstyle",
    "threeoldstyle",
    "fouroldstyle",
    "fiveoldstyle",
    "sixoldstyle",
    "sevenoldstyle",
    "eightoldstyle",
    "nineoldstyle",
    "commasuperior",
    "threequartersemdash",
    "periodsuperior",
    "questionsmall",
    "asuperior",
    "bsuperior",
    "centsuperior",
    "dsuperior",
    "esuperior",
    "isuperior",
    "lsuperior",
    "msuperior",
    "nsuperior",
    "osuperior",
    "rsuperior",
    "ssuperior",
    "tsuperior",
    "ff",
    "ffi",
    "ffl",
    "parenleftinferior",
    "parenrightinferior",
    "Circumflexsmall",
    "hyphensuperior",
    "Gravesmall",
    "Asmall",
    "Bsmall",
    "Csmall",
    "Dsmall",
    "Esmall",
    "Fsmall",
    "Gsmall",
    "Hsmall",
    "Ismall",
    "Jsmall",
    "Ksmall",
    "Lsmall",
    "Msmall",
    "Nsmall",
    "Osmall",
    "Psmall",
    "Qsmall",
    "Rsmall",
    "Ssmall",
    "Tsmall",
    "Usmall",
    "Vsmall",
    "Wsmall",
    "Xsmall",
    "Ysmall",
    "Zsmall",
    "colonmonetary",
    "onefitted",
    "rupiah",
    "Tildesmall",
    "exclamdownsmall",
    "centoldstyle",
    "Lslashsmall",
    "Scaronsmall",
    "Zcaronsmall",
    "Dieresissmall",
    "Brevesmall",
    "Caronsmall",
    "Dotaccentsmall",
    "Macronsmall",
    "figuredash",
    "hypheninferior",
    "Ogoneksmall",
    "Ringsmall",
    "Cedillasmall",
    "questiondownsmall",
    "oneeighth",
    "threeeighths",
    "fiveeighths",
    "seveneighths",
    "onethird",
    "twothirds",
    "zerosuperior",
    "foursuperior",
    "fivesuperior",
    "sixsuperior",
    "sevensuperior",
    "eightsuperior",
    "ninesuperior",
    "zeroinferior",
    "oneinferior",
    "twoinferior",
    "threeinferior",
    "fourinferior",
    "fiveinferior",
    "sixinferior",
    "seveninferior",
    "eightinferior",
    "nineinferior",
    "centinferior",
    "dollarinferior",
    "periodinferior",
    "commainferior",
    "Agravesmall",
    "Aacutesmall",
    "Acircumflexsmall",
    "Atildesmall",
    "Adieresissmall",
    "Aringsmall",
    "AEsmall",
    "Ccedillasmall",
    "Egravesmall",
    "Eacutesmall",
    "Ecircumflexsmall",
    "Edieresissmall",
    "Igravesmall",
    "Iacutesmall",
    "Icircumflexsmall",
    "Idieresissmall",
    "Ethsmall",
    "Ntildesmall",
    "Ogravesmall",
    "Oacutesmall",
    "Ocircumflexsmall",
    "Otildesmall",
    "Odieresissmall",
    "OEsmall",
    "Oslashsmall",
    "Ugravesmall",
    "Uacutesmall",
    "Ucircumflexsmall",
    "Udieresissmall",
    "Yacutesmall",
    "Thornsmall",
    "Ydieresissmall",
    "001.000",
    "001.001",
    "001.002",
    "001.003",
    "Black",
    "Bold",
    "Book",
    "Light",
    "Medium",
    "Regular",
    "Roman",
    "Semibold",
];

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn type1_builtin_encoding() {
        let clear = b"%!PS-AdobeFont-1.0: CMSY10\n/Encoding 256 array\n0 1 255 {1 index exch /.notdef put} for\n\
dup 0 /minus put\ndup 20 /lessequal put\ndup 50 /element put\nreadonly def\ncurrentfile eexec";
        let enc = type1(clear).unwrap();
        assert_eq!(enc[0].as_deref(), Some("minus"));
        assert_eq!(enc[20].as_deref(), Some("lessequal"));
        assert_eq!(enc[50].as_deref(), Some("element"));
        assert_eq!(enc[1], None);
        assert!(type1(b"/Encoding StandardEncoding def").is_none());
    }

    /// 手工拼一个最小的 CFF：两个字形（.notdef 和自定义名 "summationtext"），编码把 0x58 指向 1 号字形。
    #[test]
    fn cff_builtin_encoding() {
        fn index(items: &[&[u8]]) -> Vec<u8> {
            if items.is_empty() {
                return vec![0, 0];
            }
            let mut out = vec![0, items.len() as u8, 1];
            let mut off = 1u8;
            out.push(off);
            for it in items {
                off += it.len() as u8;
                out.push(off);
            }
            for it in items {
                out.extend_from_slice(it);
            }
            out
        }
        let enc_int = |v: i32| -> Vec<u8> {
            let b = v.to_be_bytes();
            vec![29, b[0], b[1], b[2], b[3]]
        };
        let header = [1u8, 0, 4, 1];
        let names = index(&[b"F"]);
        let strings = index(&[b"summationtext"]);
        let gsubrs = index(&[]);
        // 先算出 top dict 长度固定：三个 5 字节整数 + 3 个操作符 = 18 字节
        let top_len = 18;
        let top_index_len = 2 + 1 + 2 + top_len;
        let base = header.len() + names.len() + top_index_len + strings.len() + gsubrs.len();
        let charset_off = base;
        let charset = [0u8, 1, 135]; // format 0：1 号字形 SID 391
        let enc_off = charset_off + charset.len();
        let encoding = [0u8, 1, 0x58]; // format 0：1 号字形 → 码 0x58
        let cs_off = enc_off + encoding.len();
        let charstrings = index(&[&[14], &[14]]);
        let mut top = Vec::new();
        top.extend(enc_int(charset_off as i32));
        top.push(15);
        top.extend(enc_int(enc_off as i32));
        top.push(16);
        top.extend(enc_int(cs_off as i32));
        top.push(17);
        assert_eq!(top.len(), top_len);
        let mut font = header.to_vec();
        font.extend(names);
        font.extend(index(&[&top]));
        font.extend(strings);
        font.extend(gsubrs);
        font.extend(charset);
        font.extend(encoding);
        font.extend(charstrings);
        let enc = cff(&font).unwrap();
        assert_eq!(enc[0x58].as_deref(), Some("summationtext"));
        assert_eq!(
            crate::encoding::glyph_to_unicode(b"summationtext").as_deref(),
            Some("\u{2211}")
        );
        assert_eq!(
            crate::encoding::glyph_to_unicode(b"parenleftBigg").as_deref(),
            Some("(")
        );
    }
}
