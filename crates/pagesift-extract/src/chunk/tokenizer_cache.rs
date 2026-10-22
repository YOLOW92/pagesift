//! 零拷贝 Token 偏移量计算与快速计数。
//! 针对纯 ASCII 拉丁文与中日韩（CJK）混合排版进行了快速向量化扫描优化。

use std::ops::Range;

/// 中日韩文字和全角标点每个字单独算一个 token。
#[inline]
pub fn is_cjk(c: char) -> bool {
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

#[inline(always)]
pub fn is_ascii_ws(b: u8) -> bool {
    matches!(b, b' ' | b'\t' | b'\n' | b'\r' | 0x0b | 0x0c)
}

/// 快速单趟提取所有 Token 的起始和结束字节偏移量，避免中间 Range 包装与二次分配。
pub fn collect_tokens(text: &str) -> (Vec<usize>, Vec<usize>) {
    let b = text.as_bytes();
    let cap = (b.len() / 6).max(16);
    let mut starts = Vec::with_capacity(cap);
    let mut ends = Vec::with_capacity(cap);
    let mut pos = 0;

    while pos < b.len() {
        while pos < b.len() && is_ascii_ws(b[pos]) {
            pos += 1;
        }
        if pos >= b.len() {
            break;
        }
        if b[pos] < 128 {
            let start = pos;
            // 使用 memchr3 向量化 SIMD 扫描单词结尾（空格、换行或制表符）
            if let Some(offset) = memchr::memchr3(b' ', b'\n', b'\t', &b[pos..]) {
                let candidate = pos + offset;
                // 检查这之间是否有非 ASCII 字节或 \r
                let mut p = pos + 1;
                let mut ascii_clean = true;
                while p < candidate {
                    if b[p] >= 128 || b[p] == b'\r' {
                        ascii_clean = false;
                        break;
                    }
                    p += 1;
                }
                if ascii_clean {
                    starts.push(start);
                    ends.push(candidate);
                    pos = candidate;
                    continue;
                }
            }

            pos += 1;
            while pos < b.len() && b[pos] < 128 && !is_ascii_ws(b[pos]) {
                pos += 1;
            }
            if pos < b.len() && b[pos] >= 128 {
                while pos < b.len() {
                    let Some(c) = text[pos..].chars().next() else { break };
                    if c.is_whitespace() || is_cjk(c) {
                        break;
                    }
                    pos += c.len_utf8();
                    while pos < b.len() && b[pos] < 128 && !is_ascii_ws(b[pos]) {
                        pos += 1;
                    }
                }
            }
            starts.push(start);
            ends.push(pos);
            continue;
        }

        let Some(c) = text[pos..].chars().next() else { break };
        let start = pos;
        pos += c.len_utf8();
        if c.is_whitespace() {
            continue;
        }
        if is_cjk(c) {
            starts.push(start);
            ends.push(pos);
            continue;
        }
        while pos < b.len() {
            if b[pos] < 128 {
                if is_ascii_ws(b[pos]) {
                    break;
                }
                pos += 1;
                continue;
            }
            let Some(n) = text[pos..].chars().next() else { break };
            if n.is_whitespace() || is_cjk(n) {
                break;
            }
            pos += n.len_utf8();
        }
        starts.push(start);
        ends.push(pos);
    }

    (starts, ends)
}

/// 快速返回整个字符串的所有 Token 区间迭代器。
pub fn token_spans(text: &str) -> impl Iterator<Item = Range<usize>> + '_ {
    let b = text.as_bytes();
    let mut pos = 0;
    std::iter::from_fn(move || {
        loop {
            // 快速跳过连续 ASCII 空白
            while pos < b.len() && is_ascii_ws(b[pos]) {
                pos += 1;
            }
            if pos >= b.len() {
                return None;
            }
            if b[pos] < 128 {
                let start = pos;
                pos += 1;
                while pos < b.len() && b[pos] < 128 && !is_ascii_ws(b[pos]) {
                    pos += 1;
                }
                while pos < b.len() {
                    let c = text[pos..].chars().next()?;
                    if c.is_whitespace() || is_cjk(c) {
                        break;
                    }
                    pos += c.len_utf8();
                    while pos < b.len() && b[pos] < 128 && !is_ascii_ws(b[pos]) {
                        pos += 1;
                    }
                }
                return Some(start..pos);
            }
            let c = text[pos..].chars().next()?;
            let start = pos;
            pos += c.len_utf8();
            if c.is_whitespace() {
                continue;
            }
            if is_cjk(c) {
                return Some(start..pos);
            }
            while pos < b.len() {
                if b[pos] < 128 {
                    if is_ascii_ws(b[pos]) {
                        break;
                    }
                    pos += 1;
                    continue;
                }
                let n = text[pos..].chars().next()?;
                if n.is_whitespace() || is_cjk(n) {
                    break;
                }
                pos += n.len_utf8();
            }
            return Some(start..pos);
        }
    })
}

/// Token 索引缓存：保存所有 Token 的起始字节位置，支持 O(log N) 的区间 Token 计数。
#[derive(Debug, Clone, Default)]
pub struct TokenIndex {
    pub starts: Vec<usize>,
}

impl TokenIndex {
    pub fn new(text: &str) -> Self {
        let starts: Vec<usize> = token_spans(text).map(|t| t.start).collect();
        Self { starts }
    }

    #[inline]
    pub fn count_range(&self, markdown: &str, r: Range<usize>) -> usize {
        count_range(markdown, &self.starts, r)
    }

    #[inline]
    pub fn len(&self) -> usize {
        self.starts.len()
    }

    #[inline]
    pub fn is_empty(&self) -> bool {
        self.starts.is_empty()
    }
}

/// 计算指定字节区间 [start, end) 内包含的 token 数量。
pub fn count_range(markdown: &str, starts: &[usize], r: Range<usize>) -> usize {
    if r.start >= r.end {
        return 0;
    }
    let n = starts.partition_point(|&s| s < r.end) - starts.partition_point(|&s| s < r.start);
    let b = markdown.as_bytes();
    let mid_word = if r.start > 0 && r.start < b.len() {
        if b[r.start - 1] < 128 && b[r.start] < 128 {
            !is_ascii_ws(b[r.start - 1]) && !is_ascii_ws(b[r.start])
        } else {
            markdown[..r.start]
                .chars()
                .next_back()
                .is_some_and(|c| !c.is_whitespace() && !is_cjk(c))
                && markdown[r.start..]
                    .chars()
                    .next()
                    .is_some_and(|c| !c.is_whitespace() && !is_cjk(c))
        }
    } else {
        false
    };
    n + usize::from(mid_word)
}
