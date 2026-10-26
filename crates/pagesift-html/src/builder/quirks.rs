//! 从 DOCTYPE 判断 quirks 模式（规范 13.2.6.4.1 “initial” 插入模式）。
//! 树构建里受 quirks 影响的只有一处：quirks 模式下 `<table>` 不关掉打开着的 `<p>`。
//! 没有 DOCTYPE 的文档也是 quirks 模式。

/// `content` 是 `<!doctype` 之后、`>` 之前的部分；`eof` 表示到文件末尾也没遇到 `>`。
pub(super) fn quirks(content: &[u8], eof: bool) -> bool {
    let Some(d) = parse(content) else {
        return true;
    };
    if eof || d.name != b"html" {
        return true;
    }
    let public = d.public.map(|p| p.to_ascii_lowercase());
    let system = d.system.map(|s| s.to_ascii_lowercase());
    if let Some(p) = &public {
        if [
            &b"-//w3o//dtd w3 html strict 3.0//en//"[..],
            b"-/w3c/dtd html 4.0 transitional/en",
            b"html",
        ]
        .contains(&p.as_slice())
            || PUBLIC_PREFIXES
                .iter()
                .any(|pre| p.starts_with(pre.as_bytes()))
        {
            return true;
        }
        if system.is_none()
            && (p.starts_with(b"-//w3c//dtd html 4.01 frameset//")
                || p.starts_with(b"-//w3c//dtd html 4.01 transitional//"))
        {
            return true;
        }
    }
    system.as_deref() == Some(b"http://www.ibm.com/data/dtd/v11/ibmxhtml1-transitional.dtd")
}

struct Doctype<'a> {
    name: Vec<u8>,
    public: Option<&'a [u8]>,
    system: Option<&'a [u8]>,
}

fn is_ws(c: u8) -> bool {
    matches!(c, b'\t' | b'\n' | b'\x0c' | b' ' | b'\r')
}

/// 按分词器的 DOCTYPE 状态读名字和两个标识符；规范里会设 force-quirks 的写法返回 `None`。
fn parse(c: &[u8]) -> Option<Doctype<'_>> {
    let mut i = 0;
    let skip_ws = |i: &mut usize| {
        while *i < c.len() && is_ws(c[*i]) {
            *i += 1;
        }
    };
    skip_ws(&mut i);
    let start = i;
    while i < c.len() && !is_ws(c[i]) {
        i += 1;
    }
    if start == i {
        return None;
    }
    let name = c[start..i].to_ascii_lowercase();
    skip_ws(&mut i);
    let mut d = Doctype {
        name,
        public: None,
        system: None,
    };
    if i == c.len() {
        return Some(d);
    }
    let keyword = |i: usize, k: &[u8]| c.len() >= i + 6 && c[i..i + 6].eq_ignore_ascii_case(k);
    let quoted = |i: &mut usize| -> Option<&[u8]> {
        let q = *c.get(*i).filter(|&&q| q == b'"' || q == b'\'')?;
        let s = *i + 1;
        let e = s + memchr::memchr(q, &c[s..])?;
        *i = e + 1;
        Some(&c[s..e])
    };
    if keyword(i, b"public") {
        i += 6;
        skip_ws(&mut i);
        d.public = Some(quoted(&mut i)?);
        skip_ws(&mut i);
        if i < c.len() {
            d.system = Some(quoted(&mut i)?);
        }
    } else if keyword(i, b"system") {
        i += 6;
        skip_ws(&mut i);
        d.system = Some(quoted(&mut i)?);
    } else {
        return None;
    }
    // 标识符之后的多余内容是 bogus DOCTYPE，但不设 force-quirks
    Some(d)
}

/// 规范里公共标识符以这些开头（不分大小写）的是 quirks 模式。
const PUBLIC_PREFIXES: &[&str] = &[
    "+//silmaril//dtd html pro v0r11 19970101//",
    "-//as//dtd html 3.0 aswedit + extensions//",
    "-//advasoft ltd//dtd html 3.0 aswedit + extensions//",
    "-//ietf//dtd html 2.0 level 1//",
    "-//ietf//dtd html 2.0 level 2//",
    "-//ietf//dtd html 2.0 strict level 1//",
    "-//ietf//dtd html 2.0 strict level 2//",
    "-//ietf//dtd html 2.0 strict//",
    "-//ietf//dtd html 2.0//",
    "-//ietf//dtd html 2.1e//",
    "-//ietf//dtd html 3.0//",
    "-//ietf//dtd html 3.2 final//",
    "-//ietf//dtd html 3.2//",
    "-//ietf//dtd html 3//",
    "-//ietf//dtd html level 0//",
    "-//ietf//dtd html level 1//",
    "-//ietf//dtd html level 2//",
    "-//ietf//dtd html level 3//",
    "-//ietf//dtd html strict level 0//",
    "-//ietf//dtd html strict level 1//",
    "-//ietf//dtd html strict level 2//",
    "-//ietf//dtd html strict level 3//",
    "-//ietf//dtd html strict//",
    "-//ietf//dtd html//",
    "-//metrius//dtd metrius presentational//",
    "-//microsoft//dtd internet explorer 2.0 html strict//",
    "-//microsoft//dtd internet explorer 2.0 html//",
    "-//microsoft//dtd internet explorer 2.0 tables//",
    "-//microsoft//dtd internet explorer 3.0 html strict//",
    "-//microsoft//dtd internet explorer 3.0 html//",
    "-//microsoft//dtd internet explorer 3.0 tables//",
    "-//netscape comm. corp.//dtd html//",
    "-//netscape comm. corp.//dtd strict html//",
    "-//o'reilly and associates//dtd html 2.0//",
    "-//o'reilly and associates//dtd html extended 1.0//",
    "-//o'reilly and associates//dtd html extended relaxed 1.0//",
    "-//sq//dtd html 2.0 hotmetal + extensions//",
    "-//softquad software//dtd hotmetal pro 6.0::19990601::extensions to html 4.0//",
    "-//softquad//dtd hotmetal pro 4.0::19971010::extensions to html 4.0//",
    "-//spyglass//dtd html 2.0 extended//",
    "-//sun microsystems corp.//dtd hotjava html//",
    "-//sun microsystems corp.//dtd hotjava strict html//",
    "-//w3c//dtd html 3 1995-03-24//",
    "-//w3c//dtd html 3.2 draft//",
    "-//w3c//dtd html 3.2 final//",
    "-//w3c//dtd html 3.2//",
    "-//w3c//dtd html 3.2s draft//",
    "-//w3c//dtd html 4.0 frameset//",
    "-//w3c//dtd html 4.0 transitional//",
    "-//w3c//dtd html experimental 19960712//",
    "-//w3c//dtd html experimental 970421//",
    "-//w3c//dtd w3 html//",
    "-//w3o//dtd w3 html 3.0//",
    "-//webtechs//dtd mozilla html 2.0//",
    "-//webtechs//dtd mozilla html//",
];

#[cfg(test)]
mod tests {
    use super::quirks;

    #[test]
    fn doctypes() {
        let cases: &[(&str, bool)] = &[
            (" html", false),
            (" HTML", false),
            (
                " html PUBLIC \"-//W3C//DTD XHTML 1.0 Strict//EN\" \"http://www.w3.org/TR/xhtml1/DTD/xhtml1-strict.dtd\"",
                false,
            ),
            (
                " html PUBLIC \"-//W3C//DTD HTML 4.01//EN\" \"http://www.w3.org/TR/html4/strict.dtd\"",
                false,
            ),
            (
                " html PUBLIC \"-//W3C//DTD HTML 4.01 Transitional//EN\"",
                true,
            ),
            (
                " html PUBLIC \"-//W3C//DTD HTML 4.01 Transitional//EN\" \"http://www.w3.org/TR/html4/loose.dtd\"",
                false,
            ),
            (" HTML PUBLIC \"-//W3C//DTD HTML 3.2//EN\"", true),
            (" html PUBLIC \"html\"", true),
            (
                " html SYSTEM \"http://www.ibm.com/data/dtd/v11/ibmxhtml1-transitional.dtd\"",
                true,
            ),
            (" html SYSTEM \"about:legacy-compat\"", false),
            ("", true),
            (" foo", true),
            (" html bogus", true),
            (" html PUBLIC \"unterminated", true),
        ];
        let bad: Vec<_> = cases
            .iter()
            .filter(|(c, want)| quirks(c.as_bytes(), false) != *want)
            .collect();
        assert!(bad.is_empty(), "{bad:?}");
        assert!(quirks(b" html", true), "没结束的 DOCTYPE 是 quirks");
    }
}
