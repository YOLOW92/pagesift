//! 拉取式 XML 读取器，只做 OOXML 和 EPUB 用得到的部分（XML 1.0 的子集）。
//!
//! - 元素、属性、文本、CDATA；注释、处理指令、DOCTYPE（含内部子集）跳过。
//! - 只解码五个预定义实体和数字字符引用；DOCTYPE 里声明的实体一律不展开，原样保留：
//!   外部实体（XXE）和“十亿笑声”在这里根本不会发生。
//! - 空元素 `<a/>` 拆成开始、结束两个事件，调用方按配对处理即可。
//! - 嵌套层数超过 [`MAX_DEPTH`] 报错，防恶意构造的深层嵌套。

use std::borrow::Cow;

use crate::Error;

pub(crate) const MAX_DEPTH: usize = 512;

#[derive(Debug, Clone, PartialEq)]
pub(crate) enum Tok<'a> {
    /// 开始标签：带前缀的名字和原始属性串。
    Start(&'a str, Attrs<'a>),
    End(&'a str),
    Text(Cow<'a, str>),
}

/// 一个开始标签的属性串，按需查找、解码。
#[derive(Debug, Clone, Copy, PartialEq)]
pub(crate) struct Attrs<'a>(&'a str);

impl<'a> Attrs<'a> {
    /// 按本地名（去掉命名空间前缀）取属性值。
    pub(crate) fn get(&self, local: &str) -> Option<Cow<'a, str>> {
        self.iter()
            .find(|(k, _)| local_name(k) == local)
            .map(|(_, v)| v)
    }

    pub(crate) fn iter(&self) -> impl Iterator<Item = (&'a str, Cow<'a, str>)> {
        let mut s = self.0;
        std::iter::from_fn(move || {
            s = s.trim_start();
            let eq = s.find('=')?;
            let key = s[..eq].trim();
            let rest = s[eq + 1..].trim_start();
            let quote = rest.chars().next().filter(|c| *c == '"' || *c == '\'')?;
            let end = rest[1..].find(quote)? + 1;
            let value = unescape(&rest[1..end]);
            s = &rest[end + 1..];
            Some((key, value))
        })
    }
}

/// `w:p` → `p`。
pub(crate) fn local_name(name: &str) -> &str {
    name.rsplit_once(':').map_or(name, |(_, l)| l)
}

pub(crate) struct Reader<'a> {
    s: &'a str,
    pos: usize,
    depth: usize,
    /// 空元素的结束事件，下一次 `next` 先给出。
    pending_end: Option<&'a str>,
}

impl<'a> Reader<'a> {
    pub(crate) fn new(s: &'a str) -> Self {
        Reader {
            s,
            pos: 0,
            depth: 0,
            pending_end: None,
        }
    }

    /// 下一个事件；文档结束时返回 `None`。只有空白的文本照样返回（排版有时依赖它）。
    pub(crate) fn next(&mut self) -> Result<Option<Tok<'a>>, Error> {
        if let Some(name) = self.pending_end.take() {
            self.depth -= 1;
            return Ok(Some(Tok::End(name)));
        }
        loop {
            let rest = &self.s[self.pos..];
            if rest.is_empty() {
                return Ok(None);
            }
            if !rest.starts_with('<') {
                let end = rest.find('<').unwrap_or(rest.len());
                self.pos += end;
                return Ok(Some(Tok::Text(unescape(&rest[..end]))));
            }
            if let Some(body) = rest.strip_prefix("<![CDATA[") {
                let end = body.find("]]>").ok_or_else(|| bad("unterminated CDATA"))?;
                self.pos += 9 + end + 3;
                return Ok(Some(Tok::Text(Cow::Borrowed(&body[..end]))));
            }
            if rest.starts_with("<!--") {
                let end = rest
                    .find("-->")
                    .ok_or_else(|| bad("unterminated comment"))?;
                self.pos += end + 3;
                continue;
            }
            if rest.starts_with("<?") {
                let end = rest
                    .find("?>")
                    .ok_or_else(|| bad("unterminated processing instruction"))?;
                self.pos += end + 2;
                continue;
            }
            if rest.starts_with("<!") {
                self.pos += skip_declaration(rest)?;
                continue;
            }
            let end = tag_end(rest).ok_or_else(|| bad("unterminated tag"))?;
            let inner = &rest[1..end];
            self.pos += end + 1;
            if let Some(name) = inner.strip_prefix('/') {
                self.depth = self.depth.saturating_sub(1);
                return Ok(Some(Tok::End(name.trim())));
            }
            let (inner, empty) = match inner.strip_suffix('/') {
                Some(i) => (i, true),
                None => (inner, false),
            };
            let split = inner
                .find(|c: char| c.is_ascii_whitespace())
                .unwrap_or(inner.len());
            let name = &inner[..split];
            if name.is_empty() {
                return Err(bad("empty tag name"));
            }
            self.depth += 1;
            if self.depth > MAX_DEPTH {
                return Err(Error::TooLarge("XML nesting depth"));
            }
            if empty {
                self.pending_end = Some(name);
            }
            return Ok(Some(Tok::Start(name, Attrs(&inner[split..]))));
        }
    }

    /// 跳过当前元素剩下的内容（刚读到它的开始标签之后调用），到它的结束标签为止。
    pub(crate) fn skip(&mut self) -> Result<(), Error> {
        let mut depth = 1;
        while let Some(t) = self.next()? {
            match t {
                Tok::Start(..) => depth += 1,
                Tok::End(_) => {
                    depth -= 1;
                    if depth == 0 {
                        return Ok(());
                    }
                }
                Tok::Text(_) => {}
            }
        }
        Ok(())
    }

    /// 读到当前元素结束为止，拼出里面所有文字（刚读到开始标签之后调用）。
    pub(crate) fn text_until_end(&mut self) -> Result<String, Error> {
        let mut out = String::new();
        let mut depth = 1;
        while let Some(t) = self.next()? {
            match t {
                Tok::Start(..) => depth += 1,
                Tok::End(_) => {
                    depth -= 1;
                    if depth == 0 {
                        break;
                    }
                }
                Tok::Text(t) => out.push_str(&t),
            }
        }
        Ok(out)
    }
}

fn bad(what: &str) -> Error {
    Error::Malformed(format!("XML: {what}"))
}

/// 标签结束的 `>`：属性值里的 `>` 不算。
fn tag_end(s: &str) -> Option<usize> {
    let mut quote = None;
    for (i, b) in s.bytes().enumerate().skip(1) {
        match (quote, b) {
            (None, b'"' | b'\'') => quote = Some(b),
            (Some(q), _) if q == b => quote = None,
            (None, b'>') => return Some(i),
            _ => {}
        }
    }
    None
}

/// `<!DOCTYPE …>`（可能带 `[…]` 内部子集）和其他 `<!…>` 声明的长度。
fn skip_declaration(s: &str) -> Result<usize, Error> {
    let mut bracket = 0usize;
    let mut quote = None;
    for (i, b) in s.bytes().enumerate().skip(2) {
        match (quote, b) {
            (None, b'"' | b'\'') => quote = Some(b),
            (Some(q), _) if q == b => quote = None,
            (None, b'[') => bracket += 1,
            (None, b']') => bracket = bracket.saturating_sub(1),
            (None, b'>') if bracket == 0 => return Ok(i + 1),
            _ => {}
        }
    }
    Err(bad("unterminated declaration"))
}

/// 解码预定义实体和数字字符引用。其他实体（DTD 里声明的、HTML 的 `&nbsp;`）原样保留。
pub(crate) fn unescape(s: &str) -> Cow<'_, str> {
    if !s.contains('&') {
        return Cow::Borrowed(s);
    }
    let mut out = String::with_capacity(s.len());
    let mut rest = s;
    while let Some(i) = rest.find('&') {
        out.push_str(&rest[..i]);
        rest = &rest[i..];
        let window = &rest.as_bytes()[..rest.len().min(12)];
        let Some(semi) = window.iter().position(|&b| b == b';') else {
            out.push('&');
            rest = &rest[1..];
            continue;
        };
        let name = &rest[1..semi];
        let ch = match name {
            "lt" => Some('<'),
            "gt" => Some('>'),
            "amp" => Some('&'),
            "quot" => Some('"'),
            "apos" => Some('\''),
            n if n.starts_with("#x") || n.starts_with("#X") => u32::from_str_radix(&n[2..], 16)
                .ok()
                .and_then(char::from_u32),
            n if n.starts_with('#') => n[1..].parse().ok().and_then(char::from_u32),
            _ => None,
        };
        match ch {
            Some(c) => {
                out.push(c);
                rest = &rest[semi + 1..];
            }
            None => {
                out.push('&');
                rest = &rest[1..];
            }
        }
    }
    out.push_str(rest);
    Cow::Owned(out)
}

/// XML 部件的字节转成文本：UTF-16 看 BOM，其余按 UTF-8（坏字节替换），去掉 UTF-8 BOM。
pub(crate) fn decode(bytes: Vec<u8>) -> String {
    match bytes.as_slice() {
        [0xFF, 0xFE, rest @ ..] => utf16(rest, u16::from_le_bytes),
        [0xFE, 0xFF, rest @ ..] => utf16(rest, u16::from_be_bytes),
        [0xEF, 0xBB, 0xBF, ..] => String::from_utf8_lossy(&bytes[3..]).into_owned(),
        _ => String::from_utf8(bytes)
            .unwrap_or_else(|e| String::from_utf8_lossy(e.as_bytes()).into_owned()),
    }
}

fn utf16(b: &[u8], f: fn([u8; 2]) -> u16) -> String {
    char::decode_utf16(b.as_chunks::<2>().0.iter().map(|c| f([c[0], c[1]])))
        .map(|r| r.unwrap_or('\u{FFFD}'))
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn all(s: &str) -> Vec<Tok<'_>> {
        let mut r = Reader::new(s);
        let mut v = Vec::new();
        while let Some(t) = r.next().unwrap() {
            v.push(t);
        }
        v
    }

    #[test]
    fn reads_elements_attributes_text_and_empty_tags() {
        let toks = all(
            r#"<?xml version="1.0"?><!-- c --><w:p a="1 &amp; 2" b='x>y'><w:t>A&lt;B&#x4E2D;&#25991;</w:t><w:br/></w:p>"#,
        );
        let Tok::Start(name, attrs) = &toks[0] else {
            panic!()
        };
        assert_eq!((*name, local_name(name)), ("w:p", "p"));
        assert_eq!(attrs.get("a").as_deref(), Some("1 & 2"));
        assert_eq!(attrs.get("b").as_deref(), Some("x>y"));
        assert_eq!(toks[2], Tok::Text("A<B中文".into()));
        assert_eq!(toks[4], Tok::Start("w:br", Attrs("")));
        assert_eq!(toks[5], Tok::End("w:br"));
        assert_eq!(toks[6], Tok::End("w:p"));
    }

    #[test]
    fn external_and_internal_entities_are_never_expanded() {
        // XXE 和“十亿笑声”：DOCTYPE 整段跳过，实体引用原样留在文本里
        let xml = r#"<?xml version="1.0"?>
<!DOCTYPE r [
  <!ENTITY xxe SYSTEM "file:///etc/passwd">
  <!ENTITY a "aaaaaaaaaa"><!ENTITY b "&a;&a;&a;&a;&a;&a;&a;&a;&a;&a;">
]>
<r><t>&xxe;</t><t>&b;</t><t><![CDATA[<raw> & stuff]]></t></r>"#;
        let texts: Vec<String> = all(xml)
            .into_iter()
            .filter_map(|t| match t {
                Tok::Text(s) if !s.trim().is_empty() => Some(s.into_owned()),
                _ => None,
            })
            .collect();
        assert_eq!(texts, ["&xxe;", "&b;", "<raw> & stuff"]);
    }

    #[test]
    fn deep_nesting_and_truncation_are_errors() {
        let deep = "<a>".repeat(MAX_DEPTH + 1);
        let mut r = Reader::new(&deep);
        let err = loop {
            match r.next() {
                Ok(Some(_)) => {}
                Ok(None) => panic!("expected an error"),
                Err(e) => break e,
            }
        };
        assert_eq!(err, Error::TooLarge("XML nesting depth"));
        for broken in [
            "<a b='1",
            "<a><!-- x",
            "<a><![CDATA[x",
            "<!DOCTYPE x [ <!ENTITY",
        ] {
            let mut r = Reader::new(broken);
            assert!((0..10).any(|_| r.next().is_err()), "{broken}");
        }
    }

    #[test]
    fn decodes_utf16_and_boms() {
        let le: Vec<u8> = [0xFF, 0xFE]
            .into_iter()
            .chain("<a>é</a>".encode_utf16().flat_map(u16::to_le_bytes))
            .collect();
        assert_eq!(decode(le), "<a>é</a>");
        assert_eq!(decode(b"\xEF\xBB\xBF<a/>".to_vec()), "<a/>");
        assert_eq!(unescape("a &nbsp; &#xZZ; & b"), "a &nbsp; &#xZZ; & b");
    }
}
