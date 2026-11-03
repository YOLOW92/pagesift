//! 按大小递归切分：semchunk（MIT，Isaacus）的切分层级移植，加上 Markdown 结构和中日韩标点；
//! 片段怎么合并成块见 [`crate::pack`]。
//!
//! 一段文本超过大小时，用其中"最有结构意义"的分隔符切开：最长的连续换行（段落）、换行、连续制表符、
//! 连续空白、句末标点后的空白、其他空白、标点本身，最后按字符。切出来的片段在同一层里合并成块（不超过
//! 大小）；单个片段还超过大小就对它递归。合并只发生在同一层的相邻片段之间，所以一块不会跨过比它
//! 所在层级更高的边界（段落内部切出来的句子不会和下一段拼在一起）。
//!
//! 和 semchunk 的区别（自研）：
//! - 文档里有 Markdown 标题、表格或围栏代码块时，最顶层按 Markdown 块切（段落、列表、标题、整段代码块、
//!   表格），合并时一块不以标题结尾：标题总是和它后面的内容在一起；
//! - 合并方式默认按块结构（[`Pack::Structure`]）：不超过大小的整块不切；整块之间合并到预算一半和
//!   这一串里最长块中较大的那个；超长块内部切得最少（上限就是预算）；块长均衡。semchunk 的贪心塞满
//!   （[`Pack::Greedy`]）只留给 queryFilter；
//! - 没有空白的中日韩文本按 `。！？`、`；`、`，、` 切，不会退化到按字切；
//! - 只做字节区间运算，不复制字符串。

use std::ops::Range;

use crate::pack::{Pack, Run, group};

/// 有结构意义的非空白分隔符，按优先程度排列（semchunk 的列表，加上中日韩标点）。
const PUNCT: &[&str] = &[
    ".", "。", "?", "？", "!", "！", ";", "；", ",", "，", "、", "(", ")", "[", "]", "“", "”", "‘",
    "’", "'", "\"", "`", "*", ":", "：", "—", "…", "/", "\\", "–", "&", "-",
];

pub(crate) struct Piece {
    pub range: Range<usize>,
    /// Markdown 标题块：不放在一块的结尾
    pub heading: bool,
}

/// 把 `text` 切成每块不超过 `size` 个 token 的区间（`count` 数 token）。`blocks` 是 Markdown 顶层块，
/// 文档没有标题和代码块时传空，走纯文本的规则。`pack` 决定片段怎么合并成块。
pub(crate) fn recursive(
    text: &str,
    blocks: &[Piece],
    size: usize,
    count: &impl Fn(Range<usize>) -> usize,
    pack: Pack,
) -> Vec<Range<usize>> {
    let mut out = Vec::new();
    if blocks.is_empty() {
        chunk_range(text, 0..text.len(), size, count, pack, &mut out);
        out.retain(|r| !text[r.clone()].trim().is_empty());
        return out;
    }
    let tokens: Vec<usize> = blocks.iter().map(|b| count(b.range.clone())).collect();
    let mut i = 0;
    while i < blocks.len() {
        if tokens[i] > size {
            chunk_range(text, blocks[i].range.clone(), size, count, pack, &mut out);
            i += 1;
            continue;
        }
        // 一串连续的、各自放得下的块，在这串里合并
        let end = run_end(&tokens, i, size);
        let heading = |k: usize| blocks[i + k].heading;
        let run = Run {
            tokens: &tokens[i..end],
            sep_t: 0,
            heading: &heading,
            more_after: end < blocks.len(),
            blocks: true,
        };
        for g in group(&run, size, pack) {
            out.push(blocks[i + g.start].range.start..blocks[i + g.end - 1].range.end);
        }
        i = end;
    }
    out
}

/// 从 `i` 起连续不超过 `size` 的片段到哪里为止。
fn run_end(tokens: &[usize], i: usize, size: usize) -> usize {
    tokens[i..]
        .iter()
        .position(|&t| t > size)
        .map_or(tokens.len(), |p| i + p)
}

/// 分隔一段文本：返回片段（不含分隔符）、分隔符是否是空白、分隔符字节长度（非空白时用来把它接到前一块）、
/// 片段是不是按换行切出来的整行或整段（否则是一个超长块内部的句子、词）。
fn split(text: &str, r: Range<usize>) -> (Vec<Range<usize>>, bool, usize, bool) {
    let s = &text[r.clone()];
    let base = r.start;
    let by = |sep: &str| -> Vec<Range<usize>> {
        let mut out = Vec::new();
        let mut start = 0;
        for (i, _) in s.match_indices(sep) {
            out.push(base + start..base + i);
            start = i + sep.len();
        }
        out.push(base + start..base + s.len());
        out
    };
    // 按字符比较：U+00A0 的 UTF-8 是 C2 A0，按字节比会把 "à"（C3 A0）的第二个字节当成不换行空格
    let longest_run = |set: &[char]| -> Option<&str> {
        let (mut best, mut best_len) = (None, 0);
        let mut run: Option<usize> = None;
        for (i, c) in s.char_indices().chain(std::iter::once((s.len(), '\0'))) {
            match (set.contains(&c) && i < s.len(), run) {
                (true, None) => run = Some(i),
                (false, Some(start)) => {
                    if i - start > best_len {
                        best_len = i - start;
                        best = Some(&s[start..i]);
                    }
                    run = None;
                }
                _ => {}
            }
        }
        best
    };
    if s.contains(['\n', '\r']) {
        let sep = longest_run(&['\n', '\r']).unwrap_or("\n");
        return (by(sep), true, sep.len(), true);
    }
    if s.contains('\t') {
        let sep = longest_run(&['\t']).unwrap_or("\t");
        return (by(sep), true, sep.len(), false);
    }
    if let Some(ws) = s.find(char::is_whitespace) {
        let run = longest_run(&[' ', '\u{a0}', '\x0b', '\x0c']);
        if let Some(run) = run.filter(|r| r.len() >= 2) {
            return (by(run), true, run.len(), false);
        }
        // 单个空白：优先在句末等标点后面的空白处切，标点留在前一段
        for p in PUNCT {
            let mut cuts = Vec::new();
            let mut from = 0;
            while let Some(i) = s[from..].find(p) {
                let at = from + i + p.len();
                if let Some(c) = s[at..].chars().next()
                    && c.is_whitespace()
                {
                    cuts.push((at, c.len_utf8()));
                }
                from = from + i + p.len();
            }
            if !cuts.is_empty() {
                let mut out = Vec::new();
                let mut start = 0;
                for (at, len) in cuts {
                    out.push(base + start..base + at);
                    start = at + len;
                }
                out.push(base + start..base + s.len());
                return (out, true, 1, false);
            }
        }
        let c = s[ws..].chars().next().unwrap_or(' ');
        let mut buf = [0u8; 4];
        let sep: &str = c.encode_utf8(&mut buf);
        let sep = sep.to_owned();
        return (by(&sep), true, sep.len(), false);
    }
    for p in PUNCT {
        if s.contains(p) {
            return (by(p), false, p.len(), false);
        }
    }
    // 只剩一个"词"：按字符切
    let pieces = s
        .char_indices()
        .map(|(i, c)| base + i..base + i + c.len_utf8())
        .collect();
    (pieces, false, 0, false)
}

fn chunk_range(
    text: &str,
    r: Range<usize>,
    size: usize,
    count: &impl Fn(Range<usize>) -> usize,
    pack: Pack,
    out: &mut Vec<Range<usize>>,
) {
    if count(r.clone()) <= size {
        out.push(r);
        return;
    }
    let (pieces, sep_ws, sep_len, lines) = split(text, r.clone());
    if pieces.len() <= 1 {
        // 再也切不开了（单个超长的字）：整段作为一块
        out.push(r);
        return;
    }
    let tokens: Vec<usize> = pieces.iter().map(|p| count(p.clone())).collect();
    // 非空白分隔符（标点）本身在中日韩按字计数时也占 token，合并时算进去
    let sep_t = if sep_ws {
        0
    } else {
        count(pieces[0].end..pieces[0].end + sep_len)
    };
    // 非空白的分隔符（标点）接在前一块后面
    let attach_sep = |out: &mut Vec<Range<usize>>, next: usize| {
        if !sep_ws
            && next < pieces.len()
            && let Some(last) = out.last_mut()
            && last.end + sep_len <= r.end
        {
            last.end += sep_len;
        }
    };
    let mut i = 0;
    while i < pieces.len() {
        if tokens[i] > size {
            chunk_range(text, pieces[i].clone(), size, count, pack, out);
            i += 1;
            attach_sep(out, i);
            continue;
        }
        let end = run_end(&tokens, i, size);
        let run = Run {
            tokens: &tokens[i..end],
            sep_t,
            heading: &|_| false,
            more_after: false,
            blocks: lines,
        };
        for g in group(&run, size, pack) {
            out.push(pieces[i + g.start].start..pieces[i + g.end - 1].end);
            attach_sep(out, i + g.end);
        }
        i = end;
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn words(s: &str) -> usize {
        s.split_whitespace().count()
    }

    fn chunks(text: &str, size: usize) -> Vec<&str> {
        chunks_with(text, size, Pack::Structure)
    }

    fn chunks_with(text: &str, size: usize, pack: Pack) -> Vec<&str> {
        recursive(text, &[], size, &|r| words(&text[r]), pack)
            .into_iter()
            .map(|r| &text[r])
            .collect()
    }

    #[test]
    fn whole_paragraphs_are_never_cut() {
        // 每段都不超过大小时，块边界只落在段落之间，每段整块落在某一块里
        let mut seed: u64 = 3;
        let mut next = || {
            seed ^= seed << 13;
            seed ^= seed >> 7;
            seed ^= seed << 17;
            seed
        };
        for _ in 0..300 {
            let size = 4 + (next() % 30) as usize;
            let paras: Vec<String> = (0..1 + next() % 12)
                .map(|p| {
                    let n = 1 + (next() % size as u64) as usize;
                    (0..n)
                        .map(|w| format!("p{p}w{w}."))
                        .collect::<Vec<_>>()
                        .join(" ")
                })
                .collect();
            let text = paras.join("\n\n");
            for pack in [Pack::Greedy, Pack::Structure] {
                for c in chunks_with(&text, size, pack) {
                    let starts_ok = paras.iter().any(|p| c.starts_with(p.as_str()));
                    let ends_ok = paras.iter().any(|p| c.ends_with(p.as_str()));
                    assert!(starts_ok && ends_ok, "{pack:?} cut a paragraph: {c:?}");
                }
            }
        }
    }

    #[test]
    fn structure_leaves_room_for_two_merged_chunks() {
        // 20 段、每段 3 词：贪心塞到 size，按块结构合并的块不超过 size / 2
        let text = (0..20)
            .map(|i| format!("a{i} b{i} c{i}."))
            .collect::<Vec<_>>()
            .join("\n\n");
        let greedy = chunks_with(&text, 30, Pack::Greedy);
        let structure = chunks_with(&text, 30, Pack::Structure);
        assert_eq!(words(greedy[0]), 30);
        assert!(structure.iter().all(|c| words(c) <= 15));
        // 均衡：60 词在上限 15 下最少 4 块，每块 15
        assert_eq!(structure.len(), 4);
        assert!(structure.iter().all(|c| words(c) == 15));
    }

    #[test]
    fn keeps_paragraphs_then_sentences() {
        let text = "one two three. four five six.\n\nseven eight nine ten eleven twelve. thirteen.";
        assert_eq!(
            chunks(text, 6),
            [
                "one two three. four five six.",
                "seven eight nine ten eleven twelve.",
                "thirteen."
            ]
        );
        assert_eq!(chunks(text, 100), [text]);
    }

    #[test]
    fn splits_cjk_by_its_punctuation() {
        let text = "第一句话很长很长。第二句话也很长。第三句。";
        let count = |s: &str| s.chars().filter(|c| !c.is_whitespace()).count();
        let got: Vec<&str> = recursive(text, &[], 10, &|r| count(&text[r]), Pack::Structure)
            .into_iter()
            .map(|r| &text[r])
            .collect();
        assert_eq!(got, ["第一句话很长很长。", "第二句话也很长。", "第三句。"]);
    }

    #[test]
    fn never_exceeds_the_size_unless_a_single_word_does() {
        let mut seed: u64 = 7;
        let mut next = || {
            seed ^= seed << 13;
            seed ^= seed >> 7;
            seed ^= seed << 17;
            seed
        };
        // 含非 ASCII：à 的 UTF-8 第二个字节和不换行空格的第二个字节相同（A0），曾经被切在字符中间
        let parts = [
            "alpha",
            "beta.",
            "gamma,",
            "delta;",
            "\n",
            "\n\n",
            " ",
            "  ",
            "\t",
            "x-y",
            "(z)",
            "voilà",
            "\u{a0}",
            "à\u{a0}à",
            "日本語。",
            "naïve",
            "🙂",
            "Ünïcödé,",
        ];
        for _ in 0..3000 {
            let n = (next() % 60) as usize;
            let text: String = (0..n)
                .map(|_| parts[(next() % parts.len() as u64) as usize])
                .collect::<Vec<_>>()
                .join(" ");
            let size = 1 + (next() % 8) as usize;
            for pack in [Pack::Greedy, Pack::Structure] {
                let got = recursive(&text, &[], size, &|r| words(&text[r]), pack);
                let mut last = 0;
                for r in &got {
                    assert!(r.start >= last, "ordered, non-overlapping: {text:?}");
                    last = r.end;
                    assert!(
                        words(&text[r.clone()]) <= size,
                        "{:?} > {size} in {text:?}",
                        &text[r.clone()]
                    );
                }
                // 覆盖全部非空白内容
                let nonws = |s: &str| s.chars().filter(|c| !c.is_whitespace()).count();
                let covered: usize = got.iter().map(|r| nonws(&text[r.clone()])).sum();
                assert_eq!(covered, nonws(&text), "{text:?}");
            }
        }
    }
}
