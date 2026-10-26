//! 文本转义：Markdown 特殊字符、行首块标记、公式片段。

use super::*;

/// 文本节点：折叠空白后按 CommonMark 规则做最少的转义（自研）。只在字符真会被解析成格式时才加反斜杠：
/// 成对出现、贴着文字的 `*` `_` `~`（词中间的 `_` 不算）、像标签的 `<`、会构成链接的 `[`、
/// 像实体的 `&`、标点前的 `\`。`a_b_c`、`2 * 3`、`5%` 这类保持原样，大模型读起来也干净。
/// 反引号不转义：网页正文里成对的反引号几乎都是作者写的 Markdown 没被渲染（原意就是代码），
/// 单个的凑不成代码，本来就安全。
pub(super) fn push_text(t: &str, out: &mut String, tex: bool) {
    let start = out.len();
    collapse_whitespace_into(t, out);
    if !out.as_bytes()[start..]
        .iter()
        .any(|b| matches!(b, b'\\' | b'*' | b'_' | b'~' | b'<' | b'[' | b'&'))
    {
        return;
    }
    let seg = out.split_off(start);
    let chars: Vec<char> = seg.chars().collect();
    // 可能有公式的页面：正文里未渲染的 TeX（MathJax 在浏览器里才渲染）原样保留，不转义
    let protected = if tex { tex_spans(&chars) } else { Vec::new() };
    let count = |c: char| chars.iter().filter(|&&x| x == c).count();
    let (stars, unders, tildes) = (count('*'), count('_'), count('~'));
    let before = out.chars().next_back();
    let ws = |o: Option<char>| o.is_none_or(char::is_whitespace);
    let alnum = |o: Option<char>| o.is_some_and(char::is_alphanumeric);
    // 每个位置之后第一个 `]` 的下标：大量不配对的 `[` 时逐个往后找是平方复杂度（模糊测试找到的）
    let mut next_close = vec![usize::MAX; chars.len() + 1];
    for i in (0..chars.len()).rev() {
        next_close[i] = if chars[i] == ']' {
            i
        } else {
            next_close[i + 1]
        };
    }
    // 受保护区间按位置排好序、互不重叠，跟着下标往前走
    let mut span = 0;
    for (i, &c) in chars.iter().enumerate() {
        while protected.get(span).is_some_and(|&(_, b)| b <= i) {
            span += 1;
        }
        if protected.get(span).is_some_and(|&(a, _)| a <= i) {
            out.push(c);
            continue;
        }
        let p = if i > 0 { Some(chars[i - 1]) } else { before };
        let n = chars.get(i + 1).copied();
        let esc = match c {
            '\\' => n.is_some_and(|n| n.is_ascii_punctuation()),
            '*' => stars >= 2 && !(ws(p) && ws(n)),
            '~' => tildes >= 2 && !(ws(p) && ws(n)),
            '_' => unders >= 2 && !(ws(p) && ws(n)) && !(alnum(p) && alnum(n)),
            '<' => n.is_some_and(|n| n.is_ascii_alphabetic() || matches!(n, '/' | '!' | '?')),
            '[' => {
                let j = next_close[i + 1];
                j != usize::MAX && matches!(chars.get(j + 1), Some('(' | '['))
            }
            '&' => entity_like(&chars[i + 1..]),
            _ => false,
        };
        if esc {
            out.push('\\');
        }
        out.push(c);
    }
}

/// 文本里未渲染的 TeX 片段：`$$…$$`、`\[…\]`、`\(…\)`、`$…$`（`$` 后不是空格、前一个 `$` 前不是空格，
/// 结尾 `$` 后面不是数字，免得把 "$5 and $10" 当成公式）。返回字符下标区间。
pub(super) fn tex_spans(chars: &[char]) -> Vec<(usize, usize)> {
    let mut spans = Vec::new();
    let find = |from: usize, pat: &[char]| {
        (from..chars.len().saturating_sub(pat.len() - 1)).find(|&k| chars[k..].starts_with(pat))
    };
    // 某种收尾符从某处往后已经找不到时，后面同类的开头都不用再找（否则平方复杂度）
    let mut missing = [false; 3];
    let mut i = 0;
    while i < chars.len() {
        let two = chars.get(i..i + 2);
        let close: Option<(usize, &[char])> = match two {
            Some(['$', '$']) => Some((0, &['$', '$'])),
            Some(['\\', '[']) => Some((1, &['\\', ']'])),
            Some(['\\', '(']) => Some((2, &['\\', ')'])),
            _ => None,
        };
        if let Some((kind, close)) = close
            && !missing[kind]
        {
            match find(i + 2, close) {
                Some(end) => {
                    spans.push((i, end + 2));
                    i = end + 2;
                    continue;
                }
                None => missing[kind] = true,
            }
        }
        if chars[i] == '$'
            && chars.get(i + 1).is_some_and(|c| !c.is_whitespace())
            && let Some(end) = (i + 1..chars.len()).find(|&k| chars[k] == '$')
            && !chars[end - 1].is_whitespace()
            && !chars.get(end + 1).is_some_and(char::is_ascii_digit)
        {
            spans.push((i, end + 1));
            i = end + 1;
            continue;
        }
        i += 1;
    }
    spans
}

/// `&` 后面像 HTML 实体（`&amp;`、`&#123;`、`&#x1F;`）。
pub(super) fn entity_like(rest: &[char]) -> bool {
    let semi = rest.iter().take(34).position(|&c| c == ';');
    let Some(end) = semi else { return false };
    let body = &rest[..end];
    match body {
        ['#', 'x' | 'X', hex @ ..] => {
            (1..=6).contains(&hex.len()) && hex.iter().all(char::is_ascii_hexdigit)
        }
        ['#', dec @ ..] => (1..=7).contains(&dec.len()) && dec.iter().all(char::is_ascii_digit),
        [first, more @ ..] => {
            first.is_ascii_alphabetic()
                && !more.is_empty()
                && more.iter().all(char::is_ascii_alphanumeric)
        }
        [] => false,
    }
}

/// 段落里一行的开头像块级标记时转义（行从 `start` 开始，已经折叠成单行）。
pub(super) fn escape_block_start(text: &mut String, start: usize) {
    let line = &text[start..];
    let b = line.as_bytes();
    let only = |set: &[u8]| b.iter().all(|c| set.contains(c) || *c == b' ');
    let thematic = |c: u8| only(&[c]) && b.iter().filter(|&&x| x == c).count() >= 3;
    let at = match b.first() {
        Some(b'#') => {
            let n = b.iter().take_while(|&&c| c == b'#').count();
            (n <= 6 && matches!(b.get(n), None | Some(b' '))).then_some(0)
        }
        Some(b'-' | b'+' | b'*') => {
            (matches!(b.get(1), None | Some(b' ')) || thematic(b[0]) || only(b"-")).then_some(0)
        }
        Some(b'_') => thematic(b'_').then_some(0),
        Some(b'=') => only(b"=").then_some(0),
        Some(b'>') => Some(0),
        Some(b'~') => line.starts_with("~~~").then_some(0),
        Some(b'|') => None,
        Some(c) if c.is_ascii_digit() => {
            let n = b.iter().take_while(|c| c.is_ascii_digit()).count();
            (n <= 9
                && matches!(b.get(n), Some(b'.' | b')'))
                && matches!(b.get(n + 1), None | Some(b' ')))
            .then_some(n)
        }
        _ => None,
    };
    if let Some(i) = at {
        text.insert(start + i, '\\');
    }
}

/// 表格单元格里不会出现列表、标题、引用，段落开头为此加的转义去掉。
pub(super) fn unescape_block_start(cell: &str) -> String {
    let b = cell.as_bytes();
    if b.first() == Some(&b'\\')
        && matches!(
            b.get(1),
            Some(b'#' | b'-' | b'+' | b'*' | b'>' | b'=' | b'_' | b'~')
        )
    {
        return cell[1..].to_string();
    }
    let n = b.iter().take_while(|c| c.is_ascii_digit()).count();
    if n > 0 && b.get(n) == Some(&b'\\') && matches!(b.get(n + 1), Some(b'.' | b')')) {
        return format!("{}{}", &cell[..n], &cell[n + 1..]);
    }
    cell.to_string()
}
