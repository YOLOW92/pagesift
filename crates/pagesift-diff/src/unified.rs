//! git diff 风格的逐行差异（统一格式，带上下文行），给 Firecrawl 的 changeTracking `git-diff` 模式用。
//! 行序列的对齐和块级 diff 共用同一套 patience + Myers 算法。

use std::collections::HashMap;

use serde::{Deserialize, Serialize};

use crate::algo::{self, Edit};

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum LineKind {
    Normal,
    Add,
    Del,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct DiffLine {
    pub kind: LineKind,
    /// 在旧文本里的行号（从 1 开始）；新增行没有。
    pub old_line: Option<usize>,
    /// 在新文本里的行号（从 1 开始）；删除行没有。
    pub new_line: Option<usize>,
    /// 行内容，不带 `+`/`-`/空格前缀。
    pub text: String,
}

impl DiffLine {
    /// 带前缀的一行，和 git diff 输出里的一样。
    pub fn prefixed(&self) -> String {
        let p = match self.kind {
            LineKind::Normal => ' ',
            LineKind::Add => '+',
            LineKind::Del => '-',
        };
        format!("{p}{}", self.text)
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Hunk {
    pub old_start: usize,
    pub old_lines: usize,
    pub new_start: usize,
    pub new_lines: usize,
    pub lines: Vec<DiffLine>,
}

impl Hunk {
    /// `@@ -1,3 +1,4 @@`。行数总是写出来（`,1` 也写），和 Firecrawl 文档里的示例一致。
    pub fn header(&self) -> String {
        format!(
            "@@ -{},{} +{},{} @@",
            self.old_start, self.old_lines, self.new_start, self.new_lines
        )
    }
}

#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct UnifiedDiff {
    pub hunks: Vec<Hunk>,
    pub additions: usize,
    pub deletions: usize,
}

impl UnifiedDiff {
    /// 统一格式的文本：每个 hunk 一个 `@@` 头，后面是带前缀的行；不含文件头。没有变化时是空串。
    pub fn text(&self) -> String {
        let mut out = String::new();
        for h in &self.hunks {
            if !out.is_empty() {
                out.push('\n');
            }
            out.push_str(&h.header());
            for l in &h.lines {
                out.push('\n');
                out.push_str(&l.prefixed());
            }
        }
        out
    }
}

/// 逐行比较，每处变化前后带 `context` 行上下文；相邻变化的上下文重叠时合并成一个 hunk（同 git）。
pub fn unified_diff(old: &str, new: &str, context: usize) -> UnifiedDiff {
    let ol: Vec<&str> = old.lines().collect();
    let nl: Vec<&str> = new.lines().collect();
    let mut ids: HashMap<&str, u32> = HashMap::new();
    let mut ni = Vec::with_capacity(nl.len());
    let mut oi = Vec::with_capacity(ol.len());
    for (ls, out) in [(&ol, &mut oi), (&nl, &mut ni)] {
        for &l in ls.iter() {
            let n = ids.len() as u32;
            out.push(*ids.entry(l).or_insert(n));
        }
    }
    let edits = algo::diff(&oi, &ni);

    let mut out = UnifiedDiff::default();
    let changed: Vec<usize> = edits
        .iter()
        .enumerate()
        .filter(|(_, e)| !matches!(e, Edit::Equal(..)))
        .map(|(p, _)| p)
        .collect();
    let Some(&first) = changed.first() else {
        return out;
    };
    // 每个 hunk 覆盖的编辑下标区间（含两端），中间的相等行不超过 2×context 时合并
    let mut groups: Vec<(usize, usize)> = vec![(first, first)];
    for &p in &changed[1..] {
        let last = groups.last_mut().expect("non-empty");
        if p - last.1 - 1 > 2 * context {
            groups.push((p, p));
        } else {
            last.1 = p;
        }
    }
    // 每个编辑位置之前已经过了几行旧文本、几行新文本
    let (mut before_old, mut before_new) = (Vec::with_capacity(edits.len()), Vec::new());
    let (mut o, mut n) = (0usize, 0usize);
    for e in &edits {
        before_old.push(o);
        before_new.push(n);
        match e {
            Edit::Equal(..) => {
                o += 1;
                n += 1;
            }
            Edit::Delete(_) => o += 1,
            Edit::Insert(_) => n += 1,
        }
    }
    for (start, end) in groups {
        let lo = start.saturating_sub(context);
        let hi = (end + context).min(edits.len() - 1);
        let mut lines = Vec::with_capacity(hi - lo + 1);
        let (mut old_lines, mut new_lines) = (0, 0);
        for e in &edits[lo..=hi] {
            let line = match *e {
                Edit::Equal(i, j) => {
                    old_lines += 1;
                    new_lines += 1;
                    DiffLine {
                        kind: LineKind::Normal,
                        old_line: Some(i + 1),
                        new_line: Some(j + 1),
                        text: ol[i].to_owned(),
                    }
                }
                Edit::Delete(i) => {
                    old_lines += 1;
                    out.deletions += 1;
                    DiffLine {
                        kind: LineKind::Del,
                        old_line: Some(i + 1),
                        new_line: None,
                        text: ol[i].to_owned(),
                    }
                }
                Edit::Insert(j) => {
                    new_lines += 1;
                    out.additions += 1;
                    DiffLine {
                        kind: LineKind::Add,
                        old_line: None,
                        new_line: Some(j + 1),
                        text: nl[j].to_owned(),
                    }
                }
            };
            lines.push(line);
        }
        // 某一边没有行时，起始行号写成它前面那一行（同 git）
        let start_of = |before: usize, count: usize| if count == 0 { before } else { before + 1 };
        out.hunks.push(Hunk {
            old_start: start_of(before_old[lo], old_lines),
            old_lines,
            new_start: start_of(before_new[lo], new_lines),
            new_lines,
            lines,
        });
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn identical_texts_have_no_hunks() {
        let d = unified_diff("a\nb\n", "a\nb", 3);
        assert!(d.hunks.is_empty());
        assert_eq!(d.text(), "");
    }

    #[test]
    fn single_line_change() {
        let d = unified_diff("old content\n", "new content\n", 3);
        assert_eq!(d.text(), "@@ -1,1 +1,1 @@\n-old content\n+new content");
        assert_eq!((d.additions, d.deletions), (1, 1));
        assert_eq!(d.hunks[0].lines[0].old_line, Some(1));
        assert_eq!(d.hunks[0].lines[1].new_line, Some(1));
    }

    #[test]
    fn context_lines_and_line_numbers() {
        let old = "1\n2\n3\n4\n5\n6\n7\n8\n9\n10\n";
        let new = "1\n2\n3\n4\n5\nsix\n7\n8\n9\n10\n";
        let d = unified_diff(old, new, 2);
        assert_eq!(d.text(), "@@ -4,5 +4,5 @@\n 4\n 5\n-6\n+six\n 7\n 8");
    }

    #[test]
    fn distant_changes_split_into_hunks_and_near_ones_merge() {
        let old: String = (1..=20).map(|i| format!("l{i}\n")).collect();
        let new = old.replace("l2\n", "L2\n").replace("l18\n", "L18\n");
        let d = unified_diff(&old, &new, 3);
        assert_eq!(d.hunks.len(), 2);
        assert_eq!(d.hunks[0].header(), "@@ -1,5 +1,5 @@");
        assert_eq!(d.hunks[1].header(), "@@ -15,6 +15,6 @@");
        let near = old.replace("l2\n", "L2\n").replace("l8\n", "L8\n");
        assert_eq!(unified_diff(&old, &near, 3).hunks.len(), 1);
    }

    #[test]
    fn pure_insertions_and_deletions() {
        let d = unified_diff("", "a\nb\n", 3);
        assert_eq!(d.text(), "@@ -0,0 +1,2 @@\n+a\n+b");
        let d = unified_diff("a\nb\n", "", 3);
        assert_eq!(d.text(), "@@ -1,2 +0,0 @@\n-a\n-b");
        let d = unified_diff("a\nb\n", "a\nb\nc\n", 0);
        assert_eq!(d.text(), "@@ -2,0 +3,1 @@\n+c");
    }
}
