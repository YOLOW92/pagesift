//! Markdown 分块。每块带上所属的标题路径，方便 RAG 检索时还原上下文。

pub mod fast;
pub mod markdown_ast;
pub mod semantic;
pub mod tokenizer_cache;

use std::ops::Range;
use regex::Regex;
use serde::{Deserialize, Serialize};

use crate::Error;
use self::markdown_ast::{BlockKind, Doc};
pub(crate) use self::markdown_ast::atx_heading;
pub use self::tokenizer_cache::{TokenIndex, count_range, is_cjk, token_spans};

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
    /// 按空行切分；围栏代码块、表格和标题各自成整块。
    Paragraphs,
    /// 按句子切分，识别中日韩的 `。！？`；不跨段落、代码块或表格。
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
    /// 每块不超过 `size` 个 token，在最有结构意义的位置切开（段落、句子、标点…），
    /// 不跨更高层级的边界；Markdown 标题、表格和代码块按整块保护处理。RAG 推荐用这个。
    Recursive {
        #[serde(alias = "chunk_size")]
        size: usize,
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
    let need_blocks = matches!(
        strategy,
        ChunkStrategy::Paragraphs | ChunkStrategy::Sentences
    ) || (matches!(strategy, ChunkStrategy::Recursive { .. })
        && markdown
            .as_bytes()
            .iter()
            .any(|b| matches!(b, b'#' | b'`' | b'~' | b'|')));
    let need_headings = need_blocks || markdown.as_bytes().contains(&b'#');
    let doc = if need_blocks {
        Doc::scan(markdown, true)
    } else if need_headings {
        Doc::scan(markdown, false)
    } else {
        Doc {
            text: markdown,
            blocks: Vec::new(),
            headings: Vec::new(),
        }
    };
    let mut pre_starts: Option<Vec<usize>> = None;
    let ranges = match strategy {
        ChunkStrategy::Headings { max_level } => {
            if !(1..=6).contains(max_level) {
                return Err(Error::Options("max_level must be between 1 and 6".into()));
            }
            doc.sections(*max_level)
        }
        ChunkStrategy::Paragraphs => doc.blocks.iter().map(|b| b.range.clone()).collect(),
        ChunkStrategy::Sentences => doc.sentences(semantic::split_sentences),
        ChunkStrategy::FixedTokens { size } => {
            positive(*size, "size")?;
            let (r, s) = fast::fixed_tokens(markdown, *size);
            pre_starts = Some(s);
            r
        }
        ChunkStrategy::SlidingWindow { window, step } => {
            positive(*window, "window")?;
            positive(*step, "step")?;
            let (r, s) = fast::sliding_window(markdown, *window, *step);
            pre_starts = Some(s);
            r
        }
        ChunkStrategy::Overlapping { window, overlap } => {
            positive(*window, "window")?;
            if *overlap >= *window {
                return Err(Error::Options("overlap must be smaller than window".into()));
            }
            let (r, s) = fast::overlapping_window(markdown, *window, *overlap);
            pre_starts = Some(s);
            r
        }
        ChunkStrategy::Recursive { size } => {
            positive(*size, "size")?;
            // 有标题、表格或代码块时按 Markdown 块结构处理，否则当纯文本
            let structured = doc.blocks.iter().any(|b| b.kind != BlockKind::Text);
            let pieces: Vec<crate::recursive::Piece> = if structured {
                doc.blocks
                    .iter()
                    .map(|b| crate::recursive::Piece {
                        range: b.range.clone(),
                        heading: b.kind == BlockKind::Heading,
                    })
                    .collect()
            } else {
                Vec::new()
            };
            let starts: Vec<usize> = token_spans(markdown).map(|t| t.start).collect();
            let count = |r: Range<usize>| count_range(markdown, &starts, r);
            let out = crate::recursive::recursive(markdown, &pieces, *size, &count);
            pre_starts = Some(starts);
            out
        }
        ChunkStrategy::Regex { patterns } => regex_split(markdown, patterns)?,
    };
    let starts = pre_starts.unwrap_or_else(|| token_spans(markdown).map(|t| t.start).collect());
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
                tokens: count_range(markdown, &starts, r),
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

    const MD: &str = "前言。\n\n# 安装\n\n先下载。然后解压！\n\n## Linux\n\nRun `make`. Then install, e.g. with Mr. Smith's help.\n\n```sh\n# not a heading\n\nmake\n```\n\n| Item | Qty |\n| --- | :---: |\n| Apple | 10 |\n| Orange | 5 |\n\n## [macOS](https://apple.com) ##\n\nUse brew.\n\n# 用法\n\nDone.\n";

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
        assert_eq!(c[0], ("前言。".into(), vec![]));
        assert_eq!(c[1].1, path(&["安装"]));
        assert_eq!(c[2].1, path(&["安装", "Linux"]));
        assert!(c[2].0.contains("# not a heading"));
        assert!(c[2].0.contains("| Apple | 10 |"));
    }

    #[test]
    fn paragraphs_keep_code_blocks_and_tables_whole() {
        let c = texts(&ChunkStrategy::Paragraphs);
        // 代码块保持整块
        assert!(c.iter().any(|(t, p)| t.starts_with("```sh")
            && t.ends_with("```")
            && *p == path(&["安装", "Linux"])));
        // 表格保持整块
        assert!(c.iter().any(|(t, p)| t.contains("| Item | Qty |")
            && t.contains("| Orange | 5 |")
            && *p == path(&["安装", "Linux"])));
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
    }
}
