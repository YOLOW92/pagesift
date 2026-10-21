//! Markdown 分块。每块带上所属的标题路径，方便 RAG 检索时还原上下文。

use std::ops::Range;
use std::sync::OnceLock;

use regex::Regex;
use serde::{Deserialize, Serialize};

use crate::Error;

/// 分块策略。JSON 形式是 `{"type": "sliding_window", "window": 100, "step": 50}`，
/// 参数名也接受 Crawl4AI 的写法（`window_size`、`chunk_size`）。
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum ChunkStrategy {
    /// 在级别不超过 `max_level` 的 ATX 标题处切开，每节一块。
    Headings {
        #[serde(default = "default_level")]
        max_level: u8,
    },
    /// 按空行切分；围栏代码块和标题各自成块。
    Paragraphs,
    /// 按句子切分，识别中日韩的 `。！？`；不跨段落。
    Sentences,
    /// 每 `size` 个 token 一块（Crawl4AI 的 `FixedLengthWordChunking`）。
    FixedTokens {
        #[serde(alias = "chunk_size")]
        size: usize,
    },
    /// 窗口每次前进 `step` 个 token（Crawl4AI 的 `SlidingWindowChunking`）。
    SlidingWindow {
        #[serde(alias = "window_size")]
        window: usize,
        step: usize,
    },
    /// 相邻窗口重叠 `overlap` 个 token（Crawl4AI 的 `OverlappingWindowChunking`）。
    Overlapping {
        #[serde(alias = "window_size")]
        window: usize,
        overlap: usize,
    },
    /// 依次用每个正则切分（Crawl4AI 的 `RegexChunking`，默认 `\n\n`）。
    Regex {
        #[serde(default = "default_patterns")]
        patterns: Vec<String>,
    },
}

fn default_level() -> u8 {
    6
}

fn default_patterns() -> Vec<String> {
    vec![r"\n\n".to_owned()]
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct Chunk {
    pub index: usize,
    pub text: String,
    /// 从最外层到最内层的标题，例如 `["安装", "Linux"]`；取的是块起点所在的位置。
    pub heading_path: Vec<String>,
    /// 在输入里的 UTF-8 字节范围。
    pub start: usize,
    pub end: usize,
    /// token 数：拉丁文按空白分词，中日韩每个字算一个 token。
    pub tokens: usize,
}

/// 把 Markdown 切成块。参数不合法（窗口为 0、重叠不小于窗口、正则写错）时返回错误。
pub fn chunk(markdown: &str, strategy: &ChunkStrategy) -> Result<Vec<Chunk>, Error> {
    let doc = Doc::scan(markdown);
    let ranges = match strategy {
        ChunkStrategy::Headings { max_level } => {
            if !(1..=6).contains(max_level) {
                return Err(Error::Options("max_level must be between 1 and 6".into()));
            }
            doc.sections(*max_level)
        }
        ChunkStrategy::Paragraphs => doc.blocks.iter().map(|b| b.range.clone()).collect(),
        ChunkStrategy::Sentences => doc.sentences(),
        ChunkStrategy::FixedTokens { size } => {
            positive(*size, "size")?;
            windows(markdown, *size, *size, false)
        }
        ChunkStrategy::SlidingWindow { window, step } => {
            positive(*window, "window")?;
            positive(*step, "step")?;
            windows(markdown, *window, *step, true)
        }
        ChunkStrategy::Overlapping { window, overlap } => {
            positive(*window, "window")?;
            if overlap >= window {
                return Err(Error::Options("overlap must be smaller than window".into()));
            }
            windows(markdown, *window, window - overlap, false)
        }
        ChunkStrategy::Regex { patterns } => regex_split(markdown, patterns)?,
    };
    Ok(ranges
        .into_iter()
        .filter_map(|r| trim(markdown, r))
        .enumerate()
        .map(|(index, r)| {
            let text = &markdown[r.clone()];
            Chunk {
                index,
                text: text.to_owned(),
                heading_path: doc.path_at(r.start),
                start: r.start,
                end: r.end,
                tokens: token_spans(text).count(),
            }
        })
        .collect())
}

fn positive(v: usize, name: &str) -> Result<(), Error> {
    if v == 0 {
        return Err(Error::Options(format!("{name} must be greater than 0")));
    }
    Ok(())
}

fn trim(text: &str, r: Range<usize>) -> Option<Range<usize>> {
    let s = &text[r.clone()];
    let lead = s.len() - s.trim_start().len();
    let t = s.trim();
    (!t.is_empty()).then(|| r.start + lead..r.start + lead + t.len())
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum BlockKind {
    Text,
    Heading,
    Code,
}

struct Block {
    range: Range<usize>,
    kind: BlockKind,
}

struct Heading {
    start: usize,
    level: u8,
    path: Vec<String>,
}

struct Doc<'a> {
    text: &'a str,
    blocks: Vec<Block>,
    headings: Vec<Heading>,
}

impl<'a> Doc<'a> {
    fn scan(text: &'a str) -> Self {
        let mut blocks = Vec::new();
        let mut headings: Vec<Heading> = Vec::new();
        let mut stack: Vec<(u8, String)> = Vec::new();
        let mut cur: Option<Block> = None;
        let mut fence: Option<(u8, usize)> = None;
        let mut pos = 0;
        for line in text.split_inclusive('\n') {
            let range = pos..pos + line.len();
            pos = range.end;
            let body = line.trim_end_matches(['\n', '\r']);
            if let Some((ch, len)) = fence {
                if let Some(b) = cur.as_mut() {
                    b.range.end = range.end;
                }
                if fence_marker(body).is_some_and(|(c, l)| {
                    c == ch && l >= len && body.trim().trim_start_matches(char::from(ch)).is_empty()
                }) {
                    fence = None;
                    blocks.extend(cur.take());
                }
                continue;
            }
            if let Some(marker) = fence_marker(body) {
                blocks.extend(cur.take());
                fence = Some(marker);
                cur = Some(Block {
                    range,
                    kind: BlockKind::Code,
                });
                continue;
            }
            if body.trim().is_empty() {
                blocks.extend(cur.take());
                continue;
            }
            if let Some((level, title)) = atx_heading(body) {
                blocks.extend(cur.take());
                while stack.last().is_some_and(|(l, _)| *l >= level) {
                    stack.pop();
                }
                stack.push((level, title));
                headings.push(Heading {
                    start: range.start,
                    level,
                    path: stack.iter().map(|(_, t)| t.clone()).collect(),
                });
                blocks.push(Block {
                    range,
                    kind: BlockKind::Heading,
                });
                continue;
            }
            match cur.as_mut() {
                Some(b) => b.range.end = range.end,
                None => {
                    cur = Some(Block {
                        range,
                        kind: BlockKind::Text,
                    })
                }
            }
        }
        blocks.extend(cur);
        Doc {
            text,
            blocks,
            headings,
        }
    }

    fn path_at(&self, pos: usize) -> Vec<String> {
        let i = self.headings.partition_point(|h| h.start <= pos);
        i.checked_sub(1)
            .map(|i| self.headings[i].path.clone())
            .unwrap_or_default()
    }

    fn sections(&self, max_level: u8) -> Vec<Range<usize>> {
        let mut cuts: Vec<usize> = self
            .headings
            .iter()
            .filter(|h| h.level <= max_level)
            .map(|h| h.start)
            .collect();
        cuts.insert(0, 0);
        cuts.push(self.text.len());
        cuts.dedup();
        cuts.windows(2).map(|w| w[0]..w[1]).collect()
    }

    fn sentences(&self) -> Vec<Range<usize>> {
        let mut out = Vec::new();
        for b in &self.blocks {
            if b.kind == BlockKind::Text {
                let base = b.range.start;
                out.extend(
                    split_sentences(&self.text[b.range.clone()])
                        .map(|r| base + r.start..base + r.end),
                );
            } else {
                out.push(b.range.clone());
            }
        }
        out
    }
}

/// 围栏代码块的开头：最多 3 个空格缩进，至少 3 个 ` 或 ~。
fn fence_marker(line: &str) -> Option<(u8, usize)> {
    let t = line.trim_start_matches(' ');
    if line.len() - t.len() > 3 {
        return None;
    }
    let ch = *t.as_bytes().first()?;
    if ch != b'`' && ch != b'~' {
        return None;
    }
    let n = t.bytes().take_while(|&b| b == ch).count();
    (n >= 3).then_some((ch, n))
}

pub(crate) fn atx_heading(line: &str) -> Option<(u8, String)> {
    let t = line.trim_start_matches(' ');
    if line.len() - t.len() > 3 {
        return None;
    }
    let level = t.bytes().take_while(|&b| b == b'#').count();
    if !(1..=6).contains(&level) {
        return None;
    }
    let rest = &t[level..];
    if !rest.is_empty() && !rest.starts_with([' ', '\t']) {
        return None;
    }
    let mut title = rest.trim();
    // 结尾的 "##" 闭合序列
    let stripped = title.trim_end_matches('#');
    if stripped.len() < title.len() && (stripped.is_empty() || stripped.ends_with([' ', '\t'])) {
        title = stripped.trim_end();
    }
    Some((level as u8, clean_inline(title)))
}

/// 去掉标题里的链接语法和强调符号，只留文字。
fn clean_inline(s: &str) -> String {
    static LINK: OnceLock<Regex> = OnceLock::new();
    let link = LINK.get_or_init(|| Regex::new(r"!?\[([^\]]*)\]\([^)]*\)").expect("valid regex"));
    let s = link.replace_all(s, "$1");
    s.replace(['*', '`'], "").replace("__", "")
}

const LATIN_END: &[char] = &['.', '!', '?', '…'];
const CJK_END: &[char] = &['。', '！', '？', '｡'];
const CLOSERS: &[char] = &[
    '"', '\'', ')', ']', '”', '’', '」', '』', '）', '》', '】', '〉',
];
const ABBREVIATIONS: &[&str] = &[
    "co", "dr", "e.g", "etc", "fig", "i.e", "inc", "jr", "ltd", "mr", "mrs", "ms", "no", "prof",
    "sr", "st", "vs",
];

/// 句子边界：拉丁标点后面跟空白，或者中日韩句末标点（后面不需要空白）；换行也算边界。
fn split_sentences(text: &str) -> impl Iterator<Item = Range<usize>> + '_ {
    let mut cuts = Vec::new();
    let mut it = text.char_indices().peekable();
    while let Some((i, c)) = it.next() {
        if c == '\n' {
            cuts.push(i + 1);
            continue;
        }
        let cjk = CJK_END.contains(&c);
        if !cjk && !LATIN_END.contains(&c) {
            continue;
        }
        let mut end = i + c.len_utf8();
        while let Some(&(j, n)) = it.peek() {
            if CLOSERS.contains(&n) || LATIN_END.contains(&n) || CJK_END.contains(&n) {
                end = j + n.len_utf8();
                it.next();
            } else {
                break;
            }
        }
        let at_space = text[end..].chars().next().is_none_or(char::is_whitespace);
        if cjk || (at_space && !(c == '.' && is_abbreviation(&text[..i]))) {
            cuts.push(end);
        }
    }
    cuts.push(text.len());
    let mut start = 0;
    cuts.into_iter().filter_map(move |end| {
        let r = start..end;
        start = end;
        (!r.is_empty()).then_some(r)
    })
}

fn is_abbreviation(before: &str) -> bool {
    let word = before.rsplit(char::is_whitespace).next().unwrap_or("");
    let word = word.trim_start_matches(['(', '"', '\'']);
    let mut chars = word.chars();
    if matches!((chars.next(), chars.next()), (Some(c), None) if c.is_alphabetic() && c.is_uppercase())
    {
        return true;
    }
    ABBREVIATIONS.iter().any(|a| word.eq_ignore_ascii_case(a))
}

/// 中日韩文字和全角标点每个字单独算一个 token。
pub(crate) fn is_cjk(c: char) -> bool {
    matches!(c,
        '\u{1100}'..='\u{11FF}'
        | '\u{2E80}'..='\u{2FDF}'
        | '\u{3000}'..='\u{303F}'
        | '\u{3040}'..='\u{30FF}'
        | '\u{3130}'..='\u{318F}'
        | '\u{31F0}'..='\u{31FF}'
        | '\u{3400}'..='\u{4DBF}'
        | '\u{4E00}'..='\u{9FFF}'
        | '\u{AC00}'..='\u{D7AF}'
        | '\u{F900}'..='\u{FAFF}'
        | '\u{FF00}'..='\u{FFEF}'
        | '\u{20000}'..='\u{2FA1F}')
}

fn token_spans(text: &str) -> impl Iterator<Item = Range<usize>> + '_ {
    let mut it = text.char_indices().peekable();
    std::iter::from_fn(move || {
        loop {
            let (i, c) = it.next()?;
            if c.is_whitespace() {
                continue;
            }
            if is_cjk(c) {
                return Some(i..i + c.len_utf8());
            }
            let mut end = i + c.len_utf8();
            while let Some(&(j, n)) = it.peek() {
                if n.is_whitespace() || is_cjk(n) {
                    break;
                }
                end = j + n.len_utf8();
                it.next();
            }
            return Some(i..end);
        }
    })
}

/// token 窗口。`tail` 对应 Crawl4AI 滑动窗口的行为：最后不对齐时补一个以结尾为止的整窗。
fn windows(text: &str, window: usize, step: usize, tail: bool) -> Vec<Range<usize>> {
    let tokens: Vec<Range<usize>> = token_spans(text).collect();
    let n = tokens.len();
    if n == 0 {
        return Vec::new();
    }
    if n <= window {
        return std::iter::once(0..text.len()).collect();
    }
    let span = |a: usize, b: usize| tokens[a].start..tokens[b - 1].end;
    let mut out = Vec::new();
    let mut i = 0;
    if tail {
        while i + window <= n {
            out.push(span(i, i + window));
            i += step;
        }
        if i - step + window < n {
            out.push(span(n - window, n));
        }
    } else {
        loop {
            let end = (i + window).min(n);
            out.push(span(i, end));
            if end >= n {
                break;
            }
            i += step;
        }
    }
    out
}

fn regex_split(text: &str, patterns: &[String]) -> Result<Vec<Range<usize>>, Error> {
    let mut pieces: Vec<Range<usize>> = std::iter::once(0..text.len()).collect();
    for (i, p) in patterns.iter().enumerate() {
        let re = Regex::new(p).map_err(|e| Error::Regex {
            path: format!("patterns[{i}]"),
            message: e.to_string(),
        })?;
        let mut next = Vec::new();
        for r in pieces {
            let mut start = r.start;
            for m in re.find_iter(&text[r.clone()]) {
                if m.is_empty() {
                    continue;
                }
                next.push(start..r.start + m.start());
                start = r.start + m.end();
            }
            next.push(start..r.end);
        }
        pieces = next;
    }
    Ok(pieces)
}

#[cfg(test)]
mod tests {
    use super::*;

    const MD: &str = "前言。\n\n# 安装\n\n先下载。然后解压！\n\n## Linux\n\nRun `make`. Then install, e.g. with Mr. Smith's help.\n\n```sh\n# not a heading\n\nmake\n```\n\n## [macOS](https://apple.com) ##\n\nUse brew.\n\n# 用法\n\nDone.\n";

    fn texts(s: &ChunkStrategy) -> Vec<(String, Vec<String>)> {
        chunk(MD, s)
            .unwrap()
            .into_iter()
            .map(|c| (c.text, c.heading_path))
            .collect()
    }

    fn path(p: &[&str]) -> Vec<String> {
        p.iter().map(|s| s.to_string()).collect()
    }

    #[test]
    fn headings_carry_paths() {
        let c = texts(&ChunkStrategy::Headings { max_level: 6 });
        assert_eq!(c.len(), 5);
        assert_eq!(c[0], ("前言。".into(), vec![]));
        assert_eq!(c[1].1, path(&["安装"]));
        assert_eq!(c[2].1, path(&["安装", "Linux"]));
        assert!(c[2].0.contains("# not a heading"));
        assert_eq!(c[3].1, path(&["安装", "macOS"]));
        assert_eq!(c[4], ("# 用法\n\nDone.".into(), path(&["用法"])));
        let top = texts(&ChunkStrategy::Headings { max_level: 1 });
        assert_eq!(top.len(), 3);
    }

    #[test]
    fn paragraphs_keep_code_blocks_whole() {
        let c = texts(&ChunkStrategy::Paragraphs);
        assert!(c.iter().any(|(t, p)| t.starts_with("```sh")
            && t.ends_with("```")
            && *p == path(&["安装", "Linux"])));
        assert!(
            c.iter()
                .any(|(t, _)| t == "## [macOS](https://apple.com) ##")
        );
    }

    #[test]
    fn sentences_split_cjk_and_latin() {
        let c: Vec<String> = texts(&ChunkStrategy::Sentences)
            .into_iter()
            .map(|c| c.0)
            .collect();
        assert!(c.contains(&"先下载。".to_owned()));
        assert!(c.contains(&"然后解压！".to_owned()));
        assert!(c.contains(&"Run `make`.".to_owned()));
        assert!(c.contains(&"Then install, e.g. with Mr. Smith's help.".to_owned()));
        let s: Vec<_> = split_sentences("他说：“好。”然后走了。Pi is 3.14 ok? Yes!")
            .map(|r| r.len())
            .collect();
        assert_eq!(s.len(), 4);
    }

    #[test]
    fn token_windows() {
        let text = "a b c d e f g 中文";
        let fixed = chunk(text, &ChunkStrategy::FixedTokens { size: 4 }).unwrap();
        assert_eq!(
            fixed.iter().map(|c| c.text.as_str()).collect::<Vec<_>>(),
            ["a b c d", "e f g 中"]
                .into_iter()
                .chain(["文"])
                .collect::<Vec<_>>()
        );
        let slide = chunk(text, &ChunkStrategy::SlidingWindow { window: 4, step: 3 }).unwrap();
        assert_eq!(
            slide.iter().map(|c| c.text.as_str()).collect::<Vec<_>>(),
            ["a b c d", "d e f g", "f g 中文"]
        );
        let over = chunk(
            text,
            &ChunkStrategy::Overlapping {
                window: 5,
                overlap: 2,
            },
        )
        .unwrap();
        assert_eq!(
            over.iter().map(|c| c.text.as_str()).collect::<Vec<_>>(),
            ["a b c d e", "d e f g 中", "g 中文"]
        );
        assert!(over.iter().all(|c| c.tokens <= 5));
        assert_eq!(
            chunk("x y", &ChunkStrategy::SlidingWindow { window: 4, step: 1 })
                .unwrap()
                .len(),
            1
        );
    }

    #[test]
    fn regex_and_options() {
        let c = chunk(
            "a\n\nb---c",
            &ChunkStrategy::Regex {
                patterns: vec![r"\n\n".into(), "-+".into()],
            },
        )
        .unwrap();
        assert_eq!(
            c.iter().map(|c| c.text.as_str()).collect::<Vec<_>>(),
            ["a", "b", "c"]
        );
        assert_eq!(c[2].start, 7);
        assert!(chunk("x", &ChunkStrategy::FixedTokens { size: 0 }).is_err());
        assert!(
            chunk(
                "x",
                &ChunkStrategy::Overlapping {
                    window: 2,
                    overlap: 2
                }
            )
            .is_err()
        );
        assert!(chunk("x", &ChunkStrategy::Headings { max_level: 7 }).is_err());
        assert!(
            chunk(
                "x",
                &ChunkStrategy::Regex {
                    patterns: vec!["(".into()]
                }
            )
            .is_err()
        );
        let s: ChunkStrategy =
            serde_json::from_str(r#"{"type": "sliding_window", "window_size": 10, "step": 5}"#)
                .unwrap();
        assert_eq!(
            s,
            ChunkStrategy::SlidingWindow {
                window: 10,
                step: 5
            }
        );
        let s: ChunkStrategy = serde_json::from_str(r#"{"type": "regex"}"#).unwrap();
        assert_eq!(
            s,
            ChunkStrategy::Regex {
                patterns: vec![r"\n\n".into()]
            }
        );
    }
}
