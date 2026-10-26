//! `<script>` 内容的结束位置（规范 13.2.5 的 script data 系列状态）。
//!
//! 普通情况下遇到 `</script` 就结束；但脚本里 `<!--` 之后进入“转义”状态，转义状态里再出现
//! `<script` 进入“双重转义”，双重转义里的 `</script>` 只退回转义状态、不结束脚本，`-->` 回到
//! 普通状态。老式页面常见 `<!-- document.write("<script>…</script>") -->`，不按这个规则走，
//! 后半段 JS 会漏成正文。

use super::is_ws;

#[derive(Clone, Copy, PartialEq, Eq)]
enum State {
    Data,
    Escaped,
    DoubleEscaped,
}

/// `b[i..]` 以 `<` 或 `</` 加 `script` 开头，后面跟空白、`/` 或 `>`（结束标签还允许到文件末尾）。
fn script_tag_at(b: &[u8], i: usize, closing: bool) -> bool {
    let s = i + 1 + usize::from(closing);
    if closing && b.get(i + 1) != Some(&b'/') {
        return false;
    }
    b.len() >= s + 6
        && b[s..s + 6].eq_ignore_ascii_case(b"script")
        && b.get(s + 6)
            .is_some_and(|&c| is_ws(c) || c == b'/' || c == b'>')
}

/// 从 `i` 开始的第一个 `</script` 结束标签。
fn next_close(b: &[u8], mut i: usize) -> Option<usize> {
    let finder = memchr::memmem::Finder::new(b"</");
    while let Some(off) = finder.find(&b[i..]) {
        let lt = i + off;
        if script_tag_at(b, lt, true) {
            return Some(lt);
        }
        i = lt + 2;
    }
    None
}

/// 从 `start` 开始找结束脚本的 `</script` 的位置；找不到返回 `None`（剩下的全是脚本）。
pub(super) fn end(b: &[u8], start: usize) -> Option<usize> {
    let mut state = State::Data;
    let mut i = start;
    // 结束标签候选只会往后移：缓存起来，`<!---->` 反复出现时不重复扫描（否则是平方复杂度）
    let mut cached: Option<Option<usize>> = None;
    loop {
        if state == State::Data {
            // 普通状态：先找结束标签的候选（和其他 raw text 元素一样用 memmem 跳着找），
            // 只在这一段里查有没有 `<!--`
            let close = match cached {
                Some(c) if c.is_none_or(|c| c >= i) => c,
                _ => next_close(b, i),
            };
            cached = Some(close);
            let limit = close.unwrap_or(b.len());
            match memchr::memmem::find(&b[i..limit], b"<!--") {
                None => return close,
                Some(off) => {
                    state = State::Escaped;
                    // `<!-->`、`<!--->` 里开头的两个 `-` 和后面的 `>` 也构成 `-->`
                    i += off + 2;
                    continue;
                }
            }
        }
        i += memchr::memchr2(b'<', b'-', &b[i..])?;
        match (state, b[i]) {
            (State::Data, _) => unreachable!(),
            (_, b'-') => {
                if b[i..].starts_with(b"-->") {
                    state = State::Data;
                    i += 3;
                } else {
                    i += 1;
                }
            }
            (State::Escaped, _) => {
                if script_tag_at(b, i, true) {
                    return Some(i);
                }
                if script_tag_at(b, i, false) {
                    state = State::DoubleEscaped;
                    i += 7;
                } else {
                    i += 1;
                }
            }
            (State::DoubleEscaped, _) => {
                if script_tag_at(b, i, true) {
                    state = State::Escaped;
                    i += 8;
                } else {
                    i += 1;
                }
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::end;

    #[test]
    fn script_ends() {
        // (脚本内容加结束标签后面的部分, 脚本内容的长度；None 表示到末尾都是脚本)
        let cases: &[(&str, Option<usize>)] = &[
            ("a()</script>x", Some(3)),
            ("a()</SCRIPT >x", Some(3)),
            ("a()</scripts>x</script>", Some(14)),
            ("<!-- a() --></script>", Some(12)),
            ("<!--<script></script>--></script>", Some(24)),
            ("<!--<script></script></script>", Some(21)),
            ("<!--<script>--></script>x", Some(15)),
            ("<!--<script>-></script></script>x", Some(23)),
            ("<!-- <sCrIpt>'</script>BAR", None),
            ("<!--></script>", Some(5)),
            ("<!--a</script>", Some(5)),
            ("x</script", None),
            ("<!--<script></script>--><!-- --></script>", Some(32)),
        ];
        let bad: Vec<_> = cases
            .iter()
            .filter(|(s, want)| end(s.as_bytes(), 0) != *want)
            .map(|(s, want)| (s, want, end(s.as_bytes(), 0)))
            .collect();
        assert!(bad.is_empty(), "{bad:?}");
    }
}
