use memchr::memmem;

use crate::Tag;
use crate::dom::{Data, Span};
use crate::entity::decode_into;

use super::{Builder, Mode, Ns, table_context};

// ---------------------------------------------------------------- 分词

/// 开始标签之后分词器要切换到的状态。
#[derive(Clone, Copy, PartialEq, Eq)]
pub(super) enum Next {
    Data,
    /// `<style>`、`<script>` 等：内容原样作为文本，直到对应的结束标签。
    RawText,
    /// `<title>`、`<textarea>`：同上，但解码字符引用。
    RcData,
    /// `<plaintext>`：剩下的全部是文本。
    PlainText,
}

pub(super) fn is_ws(c: u8) -> bool {
    matches!(c, b'\t' | b'\n' | b'\x0c' | b' ' | b'\r')
}

/// 分词器找到的属性：名字和值在源字符串里的范围。
pub(super) type RawAttr = ((usize, usize), (usize, usize));

struct TagToken {
    name: (usize, usize),
    self_closing: bool,
    end: usize,
}

pub(super) fn tokenize(src: &str, tb: &mut Builder<'_>) {
    let b = src.as_bytes();
    let mut pos = 0;
    let mut text_start = 0;
    while let Some(off) = memchr::memchr(b'<', &b[pos..]) {
        let lt = pos + off;
        match b.get(lt + 1) {
            Some(c) if c.is_ascii_alphabetic() => {
                tb.text_range(text_start, lt, true);
                let Some(t) = scan_tag(b, lt + 1, &mut tb.scratch) else {
                    // 到文件末尾还没结束的标签按规范丢弃，文档照常收尾
                    tb.finish();
                    return;
                };
                let next = tb.start_tag(t.name, t.self_closing);
                pos = t.end;
                if next != Next::Data {
                    let name = src[t.name.0..t.name.1].to_ascii_lowercase();
                    pos = raw_text(src, pos, &name, next, tb);
                }
                text_start = pos;
            }
            Some(b'/') => match b.get(lt + 2) {
                Some(c) if c.is_ascii_alphabetic() => {
                    tb.text_range(text_start, lt, true);
                    let Some(t) = scan_tag(b, lt + 2, &mut tb.scratch) else {
                        // 到文件末尾还没结束的标签按规范丢弃，文档照常收尾
                        tb.finish();
                        return;
                    };
                    tb.end_tag(t.name);
                    pos = t.end;
                    text_start = pos;
                }
                Some(b'>') => {
                    tb.text_range(text_start, lt, true);
                    pos = lt + 3;
                    text_start = pos;
                }
                None => break,
                Some(_) => {
                    tb.text_range(text_start, lt, true);
                    pos = bogus_comment(b, lt + 2, tb);
                    text_start = pos;
                }
            },
            Some(b'!') => {
                tb.text_range(text_start, lt, true);
                pos = markup_declaration(b, lt + 2, tb);
                text_start = pos;
            }
            Some(b'?') => {
                tb.text_range(text_start, lt, true);
                pos = bogus_comment(b, lt + 1, tb);
                text_start = pos;
            }
            // 不是标签的 `<`（比如 "a < b"）留在文本里
            _ => pos = lt + 1,
        }
    }
    tb.text_range(text_start, b.len(), true);
    tb.finish();
}

/// 从标签名的第一个字符开始扫描一个标签，属性放进 `attrs`。到文件末尾还没结束的标签按规范丢弃。
fn scan_tag(b: &[u8], mut i: usize, attrs: &mut Vec<RawAttr>) -> Option<TagToken> {
    let name_start = i;
    while i < b.len() && !is_ws(b[i]) && b[i] != b'/' && b[i] != b'>' {
        i += 1;
    }
    let name = (name_start, i);
    attrs.clear();
    loop {
        while i < b.len() && is_ws(b[i]) {
            i += 1;
        }
        match b.get(i)? {
            b'>' => {
                return Some(TagToken {
                    name,
                    self_closing: false,
                    end: i + 1,
                });
            }
            b'/' => {
                i += 1;
                if b.get(i) == Some(&b'>') {
                    return Some(TagToken {
                        name,
                        self_closing: true,
                        end: i + 1,
                    });
                }
                continue;
            }
            _ => {}
        }
        // 属性名的第一个字符可以是 `=`
        let an_start = i;
        i += 1;
        while i < b.len() && !is_ws(b[i]) && !matches!(b[i], b'/' | b'>' | b'=') {
            i += 1;
        }
        let an = (an_start, i);
        while i < b.len() && is_ws(b[i]) {
            i += 1;
        }
        let mut value = (i, i);
        if b.get(i) == Some(&b'=') {
            i += 1;
            while i < b.len() && is_ws(b[i]) {
                i += 1;
            }
            match *b.get(i)? {
                q @ (b'"' | b'\'') => {
                    let vs = i + 1;
                    let ve = vs + memchr::memchr(q, &b[vs..])?;
                    value = (vs, ve);
                    i = ve + 1;
                }
                b'>' => value = (i, i),
                _ => {
                    let vs = i;
                    while i < b.len() && !is_ws(b[i]) && b[i] != b'>' {
                        i += 1;
                    }
                    value = (vs, i);
                }
            }
        }
        attrs.push((an, value));
    }
}

/// raw text / RCDATA 元素的内容：找到对应的结束标签为止。返回结束标签之后的位置。
fn raw_text(src: &str, start: usize, name: &str, next: Next, tb: &mut Builder<'_>) -> usize {
    let b = src.as_bytes();
    if next == Next::PlainText {
        // plaintext 的内容仍按 "in body" 处理字符：先重建活动格式元素
        if start < b.len() {
            tb.reconstruct();
        }
        tb.raw_text(start, b.len(), false);
        return b.len();
    }
    if name == "script" {
        let Some(lt) = super::script::end(b, start) else {
            tb.raw_text(start, b.len(), false);
            return b.len();
        };
        tb.raw_text(start, lt, false);
        let Some(t) = scan_tag(b, lt + 2, &mut tb.scratch) else {
            return b.len();
        };
        tb.end_tag(t.name);
        return t.end;
    }
    let finder = memmem::Finder::new(b"</");
    let mut i = start;
    while let Some(off) = finder.find(&b[i..]) {
        let lt = i + off;
        let ns = lt + 2;
        let ne = ns + name.len();
        if ne < b.len()
            && b[ns..ne].eq_ignore_ascii_case(name.as_bytes())
            && (is_ws(b[ne]) || b[ne] == b'/' || b[ne] == b'>')
        {
            tb.raw_text(start, lt, next == Next::RcData);
            let Some(t) = scan_tag(b, ns, &mut tb.scratch) else {
                return b.len();
            };
            tb.end_tag(t.name);
            return t.end;
        }
        i = lt + 2;
    }
    tb.raw_text(start, b.len(), next == Next::RcData);
    b.len()
}

/// `<!` 之后：注释、DOCTYPE、CDATA（只在 SVG/MathML 里）或者伪注释。
fn markup_declaration(b: &[u8], i: usize, tb: &mut Builder<'_>) -> usize {
    let rest = &b[i..];
    if rest.starts_with(b"--") {
        let start = i + 2;
        if b.get(start) == Some(&b'>') {
            tb.comment(start, start);
            return start + 1;
        }
        if b[start..].starts_with(b"->") {
            tb.comment(start, start);
            return start + 2;
        }
        return match comment_end(&b[start..]) {
            Some((off, len)) => {
                tb.comment(start, start + off);
                start + off + len
            }
            None => {
                tb.comment(start, b.len());
                b.len()
            }
        };
    }
    if rest.len() >= 7 && rest[..7].eq_ignore_ascii_case(b"doctype") {
        let gt = memchr::memchr(b'>', rest);
        let content = &rest[7..gt.unwrap_or(rest.len())];
        tb.doctype(content, gt.is_none());
        return gt.map_or(b.len(), |o| i + o + 1);
    }
    // CDATA 只在当前节点不是 HTML 元素时有效（集成点 mtext、foreignObject 等本身也是外部元素）
    if rest.starts_with(b"[CDATA[") && tb.stack.last().is_some_and(|o| o.ns != Ns::Html) {
        let start = i + 7;
        return match memmem::find(&b[start..], b"]]>") {
            Some(off) => {
                tb.raw_text(start, start + off, false);
                start + off + 3
            }
            None => {
                tb.raw_text(start, b.len(), false);
                b.len()
            }
        };
    }
    bogus_comment(b, i, tb)
}

/// 注释的结束：`-->`，或者规范里同样结束注释的 `--!>`。返回位置和结束符长度。
/// `--!>` 很少见：先用 memmem 找 `-->`，只在它前面找 `!`（注释在页面里很多，逐个找 `--` 会让
/// 整体解析慢 1.5%）。
#[inline(never)]
fn comment_end(b: &[u8]) -> Option<(usize, usize)> {
    let close = memmem::find(b, b"-->");
    let limit = close.unwrap_or(b.len());
    let mut from = 0;
    while let Some(off) = memchr::memchr(b'!', &b[from..limit]) {
        let i = from + off;
        if i >= 2 && &b[i - 2..i] == b"--" && b.get(i + 1) == Some(&b'>') {
            return Some((i - 2, 4));
        }
        from = i + 1;
    }
    close.map(|p| (p, 3))
}

fn bogus_comment(b: &[u8], start: usize, tb: &mut Builder<'_>) -> usize {
    match memchr::memchr(b'>', &b[start..]) {
        Some(off) => {
            tb.comment(start, start + off);
            start + off + 1
        }
        None => {
            tb.comment(start, b.len());
            b.len()
        }
    }
}

impl<'a> Builder<'a> {
    // ---- token

    pub(super) fn doctype(&mut self, content: &[u8], eof: bool) {
        if self.mode == Mode::Initial {
            self.quirks = super::quirks::quirks(content, eof);
            let id = self.new_node(Data::Doctype);
            self.append(0, id);
        }
    }

    pub(super) fn comment(&mut self, s: usize, e: usize) {
        self.skip_lf = false;
        let id = self.new_node(Data::Comment(Span::source(s, e)));
        if matches!(self.mode, Mode::Initial | Mode::AfterAfterFrameset) || self.stack.is_empty() {
            self.append(0, id);
        } else {
            self.place(id, false);
        }
    }

    /// 普通文本（data 状态）：解码字符引用，按插入模式放置。
    pub(super) fn text_range(&mut self, s: usize, e: usize, decode: bool) {
        if s >= e {
            return;
        }
        if self.in_frameset_modes() {
            self.frameset_text(s, e);
            return;
        }
        // HTML 内容里规范要求忽略 NUL（外部内容里保留为 U+FFFD）：按 NUL 切开分别处理
        if self.has_nul(s, e) && !self.in_foreign() {
            self.text_without_nul(s, e, decode);
            return;
        }
        let span = self.text_span(s, e, decode);
        let Some(span) = self.drop_leading_lf(span) else {
            return;
        };
        let mut span = span;
        if self.mode < Mode::InBody && !self.in_template() {
            // body 之前：开头的空白不算内容，其余的文字隐式打开 body
            let lead = self.str(span).bytes().take_while(|&c| is_ws(c)).count();
            if lead == span.end() - span.offset() {
                return;
            }
            span = span.skip(lead);
            self.ensure_body();
        }
        if self.frameset_ok && !self.str(span).bytes().all(is_ws) && self.non_ws_besides_nul(s, e) {
            self.frameset_ok = false;
        }
        let cur = self.current().filter(|o| o.ns == Ns::Html).map(|o| o.tag);
        let mut table_ws = cur.is_some_and(|t| table_context(t) || t == Tag::Colgroup)
            && self.str(span).bytes().all(is_ws);
        // 列组里只能有空白：有别的文字就先关掉列组（文字随后按表格规则挪到表格前面）
        if cur == Some(Tag::Colgroup) && !table_ws {
            self.pop();
            table_ws = false;
        }
        if !self.in_foreign() && !table_ws {
            self.reconstruct();
        }
        self.add_text(span, true);
    }

    /// `[s, e)` 里有没有由 NUL 换来的 U+FFFD。
    fn has_nul(&self, s: usize, e: usize) -> bool {
        if self.nuls.is_empty() {
            return false;
        }
        let i = self.nuls.partition_point(|&p| (p as usize) < s);
        self.nuls.get(i).is_some_and(|&p| (p as usize) < e)
    }

    /// 除了由 NUL 换来的 U+FFFD 以外还有非空白字符（规范：外部内容里的 NUL 不影响 frameset-ok）。
    fn non_ws_besides_nul(&self, s: usize, e: usize) -> bool {
        if !self.has_nul(s, e) {
            return true;
        }
        let mut i = s;
        for &p in &self.nuls[self.nuls.partition_point(|&p| (p as usize) < s)..] {
            let p = p as usize;
            if p >= e {
                break;
            }
            if !self.src.as_bytes()[i..p].iter().all(|&c| is_ws(c)) {
                return true;
            }
            i = p + '\u{fffd}'.len_utf8();
        }
        !self.src.as_bytes()[i..e].iter().all(|&c| is_ws(c))
    }

    /// 去掉由 NUL 换来的 U+FFFD，剩下的几段分别按普通文本处理。
    fn text_without_nul(&mut self, s: usize, e: usize, decode: bool) {
        // 规范：head 之前的 NUL 和其他字符一样隐式打开 body，然后被忽略
        if self.mode < Mode::InBody && !self.in_template() {
            self.ensure_body();
        }
        let first = self.nuls.partition_point(|&p| (p as usize) < s);
        let mut i = s;
        let mut k = first;
        while let Some(&p) = self.nuls.get(k) {
            let p = p as usize;
            if p >= e {
                break;
            }
            self.text_range(i, p, decode);
            i = p + '\u{fffd}'.len_utf8();
            k += 1;
        }
        self.text_range(i, e, decode);
    }

    /// raw text / RCDATA 元素的内容，直接放进当前元素。
    pub(super) fn raw_text(&mut self, s: usize, e: usize, decode: bool) {
        if s >= e {
            self.skip_lf = false;
            return;
        }
        let span = self.text_span(s, e, decode);
        let Some(span) = self.drop_leading_lf(span) else {
            return;
        };
        self.add_text(span, false);
    }

    pub(super) fn text_span(&mut self, s: usize, e: usize, decode: bool) -> Span {
        let raw = &self.src[s..e];
        if decode && memchr::memchr(b'&', raw.as_bytes()).is_some() {
            let start = self.buf.len();
            decode_into(&mut self.buf, raw, false);
            Span::owned(start, self.buf.len())
        } else {
            Span::source(s, e)
        }
    }

    pub(super) fn drop_leading_lf(&mut self, span: Span) -> Option<Span> {
        let span = if std::mem::take(&mut self.skip_lf) && self.str(span).starts_with('\n') {
            span.skip(1)
        } else {
            span
        };
        (!span.is_empty()).then_some(span)
    }

    pub(super) fn tag_of(&mut self, (s, e): (usize, usize)) -> Tag {
        let raw = &self.src.as_bytes()[s..e];
        if raw.len() > 16 {
            return Tag::Other;
        }
        if raw.iter().any(u8::is_ascii_uppercase) {
            self.lower.clear();
            self.lower
                .extend(raw.iter().map(|c| c.to_ascii_lowercase() as char));
            Tag::from_name(&self.lower)
        } else {
            Tag::from_name(&self.src[s..e])
        }
    }
}
