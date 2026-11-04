//! HTML 外部扫描器，移植自 tree-sitter-html 的 `src/scanner.c` 与 `src/tag.h`
//! （MIT，Copyright (c) 2014 Max Brunsfeld）。标签编号与官方枚举一致，序列化格式相同。

use super::{MAX_SERIALIZED, ScanLexer, Scanner, is_space};

const START_TAG_NAME: usize = 0;
const SCRIPT_START_TAG_NAME: usize = 1;
const STYLE_START_TAG_NAME: usize = 2;
const END_TAG_NAME: usize = 3;
const ERRONEOUS_END_TAG_NAME: usize = 4;
const SELF_CLOSING_TAG_DELIMITER: usize = 5;
const IMPLICIT_END_TAG: usize = 6;
const RAW_TEXT: usize = 7;
const COMMENT: usize = 8;

/// 空元素（官方枚举里排在 `END_OF_VOID_TAGS` 之前）。
const VOID_NAMES: [&str; 23] = [
    "AREA", "BASE", "BASEFONT", "BGSOUND", "BR", "COL", "COMMAND", "EMBED", "FRAME", "HR", "IMAGE",
    "IMG", "INPUT", "ISINDEX", "KEYGEN", "LINK", "MENUITEM", "META", "NEXTID", "PARAM", "SOURCE",
    "TRACK", "WBR",
];
const END_OF_VOID_TAGS: u8 = 23;
/// 其余元素，编号从 `END_OF_VOID_TAGS + 1` 起，顺序与官方枚举一致。
const OTHER_NAMES: [&str; 102] = [
    "A",
    "ABBR",
    "ADDRESS",
    "ARTICLE",
    "ASIDE",
    "AUDIO",
    "B",
    "BDI",
    "BDO",
    "BLOCKQUOTE",
    "BODY",
    "BUTTON",
    "CANVAS",
    "CAPTION",
    "CITE",
    "CODE",
    "COLGROUP",
    "DATA",
    "DATALIST",
    "DD",
    "DEL",
    "DETAILS",
    "DFN",
    "DIALOG",
    "DIV",
    "DL",
    "DT",
    "EM",
    "FIELDSET",
    "FIGCAPTION",
    "FIGURE",
    "FOOTER",
    "FORM",
    "H1",
    "H2",
    "H3",
    "H4",
    "H5",
    "H6",
    "HEAD",
    "HEADER",
    "HGROUP",
    "HTML",
    "I",
    "IFRAME",
    "INS",
    "KBD",
    "LABEL",
    "LEGEND",
    "LI",
    "MAIN",
    "MAP",
    "MARK",
    "MATH",
    "MENU",
    "METER",
    "NAV",
    "NOSCRIPT",
    "OBJECT",
    "OL",
    "OPTGROUP",
    "OPTION",
    "OUTPUT",
    "P",
    "PICTURE",
    "PRE",
    "PROGRESS",
    "Q",
    "RB",
    "RP",
    "RT",
    "RTC",
    "RUBY",
    "S",
    "SAMP",
    "SCRIPT",
    "SECTION",
    "SELECT",
    "SLOT",
    "SMALL",
    "SPAN",
    "STRONG",
    "STYLE",
    "SUB",
    "SUMMARY",
    "SUP",
    "SVG",
    "TABLE",
    "TBODY",
    "TD",
    "TEMPLATE",
    "TEXTAREA",
    "TFOOT",
    "TH",
    "THEAD",
    "TIME",
    "TITLE",
    "TR",
    "U",
    "UL",
    "VAR",
    "VIDEO",
];
const CUSTOM: u8 = 126;
const END_: u8 = 127;

const NOT_IN_PARAGRAPHS: [&str; 26] = [
    "ADDRESS",
    "ARTICLE",
    "ASIDE",
    "BLOCKQUOTE",
    "DETAILS",
    "DIV",
    "DL",
    "FIELDSET",
    "FIGCAPTION",
    "FIGURE",
    "FOOTER",
    "FORM",
    "H1",
    "H2",
    "H3",
    "H4",
    "H5",
    "H6",
    "HEADER",
    "HR",
    "MAIN",
    "NAV",
    "OL",
    "P",
    "PRE",
    "SECTION",
];

#[derive(Clone, PartialEq, Eq)]
struct Tag {
    kind: u8,
    custom: Vec<u8>,
}

/// `lower`：Svelte 的标签名保留大小写，表里的名字按小写比较（大写开头的是组件，算自定义）。
fn type_for_name(name: &[u8], lower: bool) -> u8 {
    let eq = |n: &str| {
        if lower {
            n.to_ascii_lowercase().as_bytes() == name
        } else {
            n.as_bytes() == name
        }
    };
    if let Some(i) = VOID_NAMES.iter().position(|n| eq(n)) {
        return i as u8;
    }
    if let Some(i) = OTHER_NAMES.iter().position(|n| eq(n)) {
        return END_OF_VOID_TAGS + 1 + i as u8;
    }
    CUSTOM
}

fn type_is(kind: u8, name: &str) -> bool {
    kind != CUSTOM && type_for_name(name.as_bytes(), false) == kind
}

impl Tag {
    fn for_name(name: Vec<u8>, lower: bool) -> Tag {
        let kind = type_for_name(&name, lower);
        Tag {
            kind,
            custom: if kind == CUSTOM { name } else { Vec::new() },
        }
    }

    fn is_void(&self) -> bool {
        self.kind < END_OF_VOID_TAGS
    }

    fn can_contain(&self, other: &Tag) -> bool {
        let c = other.kind;
        let is = |n: &str| type_is(c, n);
        let me = |n: &str| type_is(self.kind, n);
        if me("LI") {
            !is("LI")
        } else if me("DT") || me("DD") {
            !is("DT") && !is("DD")
        } else if me("P") {
            !NOT_IN_PARAGRAPHS.iter().any(|n| is(n))
        } else if me("COLGROUP") {
            is("COL")
        } else if me("RB") || me("RT") || me("RP") {
            !is("RB") && !is("RT") && !is("RP")
        } else if me("OPTGROUP") {
            !is("OPTGROUP")
        } else if me("TR") {
            !is("TR")
        } else if me("TD") || me("TH") {
            !is("TD") && !is("TH") && !is("TR")
        } else {
            true
        }
    }
}

#[derive(Default)]
pub struct Html {
    tags: Vec<Tag>,
    /// Svelte 变体：标签名保留大小写、允许 `.`，并多出花括号相关的记号。
    svelte: bool,
}

impl Html {
    pub fn svelte() -> Html {
        Html {
            tags: Vec::new(),
            svelte: true,
        }
    }
}

fn upper(c: char) -> u8 {
    c.to_uppercase().next().unwrap_or(c) as u32 as u8
}

fn scan_tag_name(lx: &mut ScanLexer, svelte: bool) -> Vec<u8> {
    let mut name = Vec::new();
    while lx.lookahead().is_alphanumeric()
        || matches!(lx.lookahead(), '-' | ':')
        || (svelte && lx.lookahead() == '.')
    {
        name.push(if svelte {
            lx.lookahead() as u32 as u8
        } else {
            upper(lx.lookahead())
        });
        lx.next();
    }
    name
}

fn scan_comment(lx: &mut ScanLexer) -> bool {
    if lx.lookahead() != '-' {
        return false;
    }
    lx.next();
    if lx.lookahead() != '-' {
        return false;
    }
    lx.next();
    let mut dashes = 0u32;
    while lx.lookahead() != '\0' {
        match lx.lookahead() {
            '-' => dashes += 1,
            '>' if dashes >= 2 => {
                lx.set_result(COMMENT as u16);
                lx.next();
                lx.mark_end();
                return true;
            }
            _ => dashes = 0,
        }
        lx.next();
    }
    false
}

impl Html {
    fn scan_raw_text(&mut self, lx: &mut ScanLexer) -> bool {
        let Some(top) = self.tags.last() else {
            return false;
        };
        lx.mark_end();
        let delim: &[u8] = if type_is(top.kind, "SCRIPT") {
            b"</SCRIPT"
        } else {
            b"</STYLE"
        };
        let mut idx = 0;
        while lx.lookahead() != '\0' {
            if upper(lx.lookahead()) == delim[idx] {
                idx += 1;
                if idx == delim.len() {
                    break;
                }
                lx.next();
            } else {
                idx = 0;
                lx.next();
                lx.mark_end();
            }
        }
        lx.set_result(RAW_TEXT as u16);
        true
    }

    fn scan_implicit_end_tag(&mut self, lx: &mut ScanLexer) -> bool {
        let mut closing = false;
        if lx.lookahead() == '/' {
            closing = true;
            lx.next();
        } else if self.tags.last().is_some_and(Tag::is_void) {
            self.tags.pop();
            lx.set_result(IMPLICIT_END_TAG as u16);
            return true;
        }
        let name = scan_tag_name(lx, self.svelte);
        if name.is_empty() && !lx.eof() {
            return false;
        }
        let next = Tag::for_name(name, self.svelte);
        if closing {
            if self.tags.last() == Some(&next) {
                return false;
            }
            if self.tags.iter().any(|t| t.kind == next.kind) {
                self.tags.pop();
                lx.set_result(IMPLICIT_END_TAG as u16);
                return true;
            }
        } else if let Some(parent) = self.tags.last()
            && (!parent.can_contain(&next)
                || ((type_is(parent.kind, "HTML")
                    || type_is(parent.kind, "HEAD")
                    || type_is(parent.kind, "BODY"))
                    && lx.eof()))
        {
            self.tags.pop();
            lx.set_result(IMPLICIT_END_TAG as u16);
            return true;
        }
        false
    }

    fn scan_start_tag_name(&mut self, lx: &mut ScanLexer) -> bool {
        let name = scan_tag_name(lx, self.svelte);
        if name.is_empty() {
            return false;
        }
        let tag = Tag::for_name(name, self.svelte);
        let sym = if type_is(tag.kind, "SCRIPT") {
            SCRIPT_START_TAG_NAME
        } else if type_is(tag.kind, "STYLE") {
            STYLE_START_TAG_NAME
        } else {
            START_TAG_NAME
        };
        self.tags.push(tag);
        lx.set_result(sym as u16);
        true
    }

    fn scan_end_tag_name(&mut self, lx: &mut ScanLexer) -> bool {
        let name = scan_tag_name(lx, self.svelte);
        if name.is_empty() {
            return false;
        }
        let tag = Tag::for_name(name, self.svelte);
        if self.tags.last() == Some(&tag) {
            self.tags.pop();
            lx.set_result(END_TAG_NAME as u16);
        } else {
            lx.set_result(ERRONEOUS_END_TAG_NAME as u16);
        }
        true
    }
}

impl Scanner for Html {
    fn scan(&mut self, lx: &mut ScanLexer, v: &[bool]) -> bool {
        if v[RAW_TEXT] && !v[START_TAG_NAME] && !v[END_TAG_NAME] {
            return self.scan_raw_text(lx);
        }
        if self.svelte
            && let Some(r) = super::svelte::pre_scan(lx, v)
        {
            return r;
        }
        while is_space(lx.lookahead()) {
            lx.skip();
        }
        match lx.lookahead() {
            '<' => {
                lx.mark_end();
                lx.next();
                if lx.lookahead() == '!' {
                    lx.next();
                    return scan_comment(lx);
                }
                if v[IMPLICIT_END_TAG] {
                    return self.scan_implicit_end_tag(lx);
                }
            }
            c @ ('\0' | '{') if c == '\0' || self.svelte => {
                if v[IMPLICIT_END_TAG] {
                    return self.scan_implicit_end_tag(lx);
                }
            }
            '/' => {
                if v[SELF_CLOSING_TAG_DELIMITER] {
                    lx.next();
                    if lx.lookahead() == '>' {
                        lx.next();
                        if !self.tags.is_empty() {
                            self.tags.pop();
                            lx.set_result(SELF_CLOSING_TAG_DELIMITER as u16);
                        }
                        return true;
                    }
                    return false;
                }
            }
            _ => {
                if (v[START_TAG_NAME] || v[END_TAG_NAME]) && !v[RAW_TEXT] {
                    return if v[START_TAG_NAME] {
                        self.scan_start_tag_name(lx)
                    } else {
                        self.scan_end_tag_name(lx)
                    };
                }
            }
        }
        false
    }

    fn serialize(&self, out: &mut Vec<u8>) {
        let count = self.tags.len().min(u16::MAX as usize) as u16;
        let base = out.len();
        out.extend_from_slice(&[0, 0]);
        out.extend_from_slice(&count.to_le_bytes());
        let mut done = 0u16;
        while done < count {
            let t = &self.tags[done as usize];
            if t.kind == CUSTOM {
                let n = t.custom.len().min(255);
                if out.len() - base + 2 + n >= MAX_SERIALIZED {
                    break;
                }
                out.push(t.kind);
                out.push(n as u8);
                out.extend_from_slice(&t.custom[..n]);
            } else {
                if out.len() - base + 1 >= MAX_SERIALIZED {
                    break;
                }
                out.push(t.kind);
            }
            done += 1;
        }
        out[base..base + 2].copy_from_slice(&done.to_le_bytes());
    }

    fn deserialize(&mut self, d: &[u8]) {
        self.tags.clear();
        if d.len() < 4 {
            return;
        }
        let serialized = u16::from_le_bytes([d[0], d[1]]) as usize;
        let count = u16::from_le_bytes([d[2], d[3]]) as usize;
        let mut p = 4;
        let mut i = 0;
        while i < serialized && p < d.len() {
            let kind = d[p];
            p += 1;
            let mut custom = Vec::new();
            if kind == CUSTOM {
                if p >= d.len() {
                    break;
                }
                let n = (d[p] as usize).min(d.len() - p - 1);
                p += 1;
                custom = d[p..p + n].to_vec();
                p += n;
            }
            self.tags.push(Tag { kind, custom });
            i += 1;
        }
        while self.tags.len() < count {
            self.tags.push(Tag {
                kind: END_,
                custom: Vec::new(),
            });
        }
    }
}
