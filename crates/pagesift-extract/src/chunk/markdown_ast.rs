//! Markdown AST 结构保持解析与面包屑祖先路径追踪。
//! 完整保留代码围栏（Fenced Code Block）与表格（Table），杜绝腰斩破损。

use std::ops::Range;
use std::sync::OnceLock;
use regex::Regex;

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum BlockKind {
    Text,
    Heading,
    Code,
    Table,
}

#[derive(Clone, Debug)]
pub struct Block {
    pub range: Range<usize>,
    pub kind: BlockKind,
}

#[derive(Clone, Debug)]
pub struct Heading {
    pub start: usize,
    pub level: u8,
    pub path: Vec<String>,
}

#[derive(Debug)]
pub struct Doc<'a> {
    pub text: &'a str,
    pub blocks: Vec<Block>,
    pub headings: Vec<Heading>,
}

impl<'a> Doc<'a> {
    /// 扫描 Markdown 文档。need_blocks 为 true 时解析段落、围栏代码块和表格。
    pub fn scan(text: &'a str, need_blocks: bool) -> Self {
        let mut blocks = Vec::new();
        let mut headings: Vec<Heading> = Vec::new();
        let mut stack: Vec<(u8, String)> = Vec::new();
        let mut cur: Option<Block> = None;
        let mut fence: Option<(u8, usize)> = None;

        let mut lines = Vec::new();
        let mut pos = 0;
        for line in text.split_inclusive('\n') {
            let start = pos;
            pos += line.len();
            lines.push((start..pos, line));
        }

        let mut line_idx = 0;
        while line_idx < lines.len() {
            let (range, line) = &lines[line_idx];
            let body = line.trim_end_matches(['\n', '\r']);
            let trimmed = body.trim_start_matches(' ');
            let first = trimmed.as_bytes().first().copied();

            // 1. 处于代码围栏内部
            if let Some((ch, len)) = fence {
                if let Some(b) = cur.as_mut() {
                    b.range.end = range.end;
                }
                if first == Some(ch)
                    && fence_marker(body).is_some_and(|(c, l)| {
                        c == ch
                            && l >= len
                            && trimmed
                                .trim_end()
                                .trim_start_matches(char::from(ch))
                                .is_empty()
                    })
                {
                    fence = None;
                    if need_blocks {
                        blocks.extend(cur.take());
                    }
                }
                line_idx += 1;
                continue;
            }

            // 2. 检测代码围栏起点
            if matches!(first, Some(b'`' | b'~'))
                && let Some(marker) = fence_marker(body)
            {
                fence = Some(marker);
                if need_blocks {
                    blocks.extend(cur.take());
                    cur = Some(Block {
                        range: range.clone(),
                        kind: BlockKind::Code,
                    });
                }
                line_idx += 1;
                continue;
            }

            // 如果不需要解析块结构，仅维护 ATX 标题路径
            if !need_blocks {
                if first == Some(b'#')
                    && let Some((level, title)) = atx_heading(body)
                {
                    while stack.last().is_some_and(|(l, _)| *l >= level) {
                        stack.pop();
                    }
                    stack.push((level, title));
                    headings.push(Heading {
                        start: range.start,
                        level,
                        path: stack.iter().map(|(_, t)| t.clone()).collect(),
                    });
                }
                line_idx += 1;
                continue;
            }

            // 3. 空行：结束当前普通文本块
            if trimmed.trim().is_empty() {
                blocks.extend(cur.take());
                line_idx += 1;
                continue;
            }

            // 4. ATX 标题
            if first == Some(b'#')
                && let Some((level, title)) = atx_heading(body)
            {
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
                    range: range.clone(),
                    kind: BlockKind::Heading,
                });
                line_idx += 1;
                continue;
            }

            // 5. GFM 表格检测（当前行包含 | 且下一行是表格分隔行如 |---|---:）
            if is_table_row(body) && line_idx + 1 < lines.len() {
                let next_body = lines[line_idx + 1].1.trim_end_matches(['\n', '\r']);
                if is_table_delimiter(next_body) {
                    blocks.extend(cur.take());
                    let table_start = range.start;
                    let mut table_end = lines[line_idx + 1].0.end;
                    line_idx += 2;
                    while line_idx < lines.len() {
                        let cur_body = lines[line_idx].1.trim_end_matches(['\n', '\r']);
                        if cur_body.trim().is_empty() || !is_table_row(cur_body) {
                            break;
                        }
                        table_end = lines[line_idx].0.end;
                        line_idx += 1;
                    }
                    blocks.push(Block {
                        range: table_start..table_end,
                        kind: BlockKind::Table,
                    });
                    continue;
                }
            }

            // 6. 普通正文文本延续或开始
            match cur.as_mut() {
                Some(b) => b.range.end = range.end,
                None => {
                    cur = Some(Block {
                        range: range.clone(),
                        kind: BlockKind::Text,
                    })
                }
            }
            line_idx += 1;
        }

        blocks.extend(cur);
        Doc {
            text,
            blocks,
            headings,
        }
    }

    /// 获取指定字符偏移量所在位置的祖先标题路径（面包屑）
    pub fn path_at(&self, pos: usize) -> Vec<String> {
        let i = self.headings.partition_point(|h| h.start <= pos);
        i.checked_sub(1)
            .map(|i| self.headings[i].path.clone())
            .unwrap_or_default()
    }

    /// 按指定最高级别切分章节
    pub fn sections(&self, max_level: u8) -> Vec<Range<usize>> {
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

    /// 按句子切分正文（代码块、表格和标题保持整块不切）
    pub fn sentences<F>(&self, split_sent_fn: F) -> Vec<Range<usize>>
    where
        F: Fn(&str) -> Vec<Range<usize>>,
    {
        let mut out = Vec::new();
        for b in &self.blocks {
            if b.kind == BlockKind::Text {
                let base = b.range.start;
                out.extend(
                    split_sent_fn(&self.text[b.range.clone()])
                        .into_iter()
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
pub fn fence_marker(line: &str) -> Option<(u8, usize)> {
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

/// ATX 标题解析：识别 `# Title`，返回 (level, 清洗后的纯文本)。
pub fn atx_heading(line: &str) -> Option<(u8, String)> {
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

/// 快速判断是否为 Markdown 表格行。
#[inline]
pub fn is_table_row(line: &str) -> bool {
    let t = line.trim();
    t.contains('|') && !t.starts_with("```")
}

/// 快速判断是否为 Markdown 表格的分隔行（如 `|---|:---:|`）。
pub fn is_table_delimiter(line: &str) -> bool {
    let t = line.trim();
    if !t.contains('|') || !t.contains('-') {
        return false;
    }
    let stripped: String = t.chars().filter(|c| !c.is_whitespace()).collect();
    let parts: Vec<&str> = stripped.split('|').filter(|s| !s.is_empty()).collect();
    if parts.is_empty() {
        return false;
    }
    parts.iter().all(|p| {
        let trimmed = p.trim_matches(':');
        !trimmed.is_empty() && trimmed.chars().all(|c| c == '-')
    })
}

/// 清洗标题中的链接语法和强调符号，只保留纯文字。
pub fn clean_inline(s: &str) -> String {
    static LINK: OnceLock<Regex> = OnceLock::new();
    let link = LINK.get_or_init(|| Regex::new(r"!?\[([^\]]*)\]\([^)]*\)").expect("valid regex"));
    let s = link.replace_all(s, "$1");
    s.replace(['*', '`'], "").replace("__", "")
}
