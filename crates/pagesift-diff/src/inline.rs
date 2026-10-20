//! 行内差异：英文按词、中日韩按字。

use std::collections::HashMap;

use serde::{Deserialize, Serialize};

use crate::algo::{self, Edit};

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum InlineOp {
    Equal,
    Delete,
    Insert,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct InlineSegment {
    pub op: InlineOp,
    pub text: String,
}

fn is_cjk(c: char) -> bool {
    matches!(c as u32,
        0x3400..=0x4DBF | 0x4E00..=0x9FFF | 0xF900..=0xFAFF | 0x20000..=0x2FFFF
        | 0x3040..=0x30FF | 0xAC00..=0xD7AF)
}

pub(crate) fn tokenize(s: &str) -> Vec<&str> {
    #[derive(PartialEq, Clone, Copy)]
    enum Class {
        Word,
        Space,
        Single,
    }
    let class = |c: char| {
        if is_cjk(c) {
            Class::Single
        } else if c.is_alphanumeric() {
            Class::Word
        } else if c.is_whitespace() {
            Class::Space
        } else {
            Class::Single
        }
    };
    let mut out = Vec::new();
    let mut start: Option<(usize, Class)> = None;
    for (i, c) in s.char_indices() {
        let cl = class(c);
        match start {
            Some((_, k)) if k == cl && k != Class::Single => {}
            Some((st, _)) => {
                out.push(&s[st..i]);
                start = Some((i, cl));
            }
            None => start = Some((i, cl)),
        }
    }
    if let Some((st, _)) = start {
        out.push(&s[st..]);
    }
    out
}

pub fn inline_diff(old: &str, new: &str) -> Vec<InlineSegment> {
    let ot = tokenize(old);
    let nt = tokenize(new);
    let mut ids: HashMap<&str, u32> = HashMap::new();
    let oi: Vec<u32> = ot
        .iter()
        .map(|s| {
            let n = ids.len() as u32;
            *ids.entry(s).or_insert(n)
        })
        .collect();
    let ni: Vec<u32> = nt
        .iter()
        .map(|s| {
            let n = ids.len() as u32;
            *ids.entry(s).or_insert(n)
        })
        .collect();
    let mut segs: Vec<InlineSegment> = Vec::new();
    let mut push = |op: InlineOp, t: &str| match segs.last_mut() {
        Some(l) if l.op == op => l.text.push_str(t),
        _ => segs.push(InlineSegment {
            op,
            text: t.to_string(),
        }),
    };
    // 同一处修改里先输出删除，再输出插入，便于阅读
    let (mut dels, mut inss): (Vec<&str>, Vec<&str>) = (Vec::new(), Vec::new());
    for e in algo::diff(&oi, &ni) {
        match e {
            Edit::Equal(i, _) => {
                for d in dels.drain(..) {
                    push(InlineOp::Delete, d);
                }
                for s in inss.drain(..) {
                    push(InlineOp::Insert, s);
                }
                push(InlineOp::Equal, ot[i]);
            }
            Edit::Delete(i) => dels.push(ot[i]),
            Edit::Insert(j) => inss.push(nt[j]),
        }
    }
    for d in dels {
        push(InlineOp::Delete, d);
    }
    for s in inss {
        push(InlineOp::Insert, s);
    }
    segs
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn cjk_by_char() {
        let segs = inline_diff("今天天气很好", "今天天气很差");
        let del: Vec<_> = segs.iter().filter(|s| s.op == InlineOp::Delete).collect();
        assert_eq!(del.len(), 1);
        assert_eq!(del[0].text, "好");
    }

    #[test]
    fn words() {
        let segs = inline_diff("the quick fox", "the slow fox");
        assert!(
            segs.iter()
                .any(|s| s.op == InlineOp::Delete && s.text == "quick")
        );
        assert!(
            segs.iter()
                .any(|s| s.op == InlineOp::Insert && s.text == "slow")
        );
    }
}
