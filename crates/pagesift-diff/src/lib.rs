#![forbid(unsafe_code)]
//! 同一 URL 两份 Markdown 快照的块级差异对比，并区分噪声变化。

mod algo;
mod block;
mod inline;
mod noise;

use std::cell::OnceCell;
use std::collections::{HashMap, VecDeque};

use serde::{Deserialize, Serialize};

pub use block::{Block, BlockKind, normalize, split_blocks};
pub use inline::{InlineOp, InlineSegment, inline_diff};
pub use noise::NoiseCategory;

use algo::Edit;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum ChangeType {
    Added,
    Removed,
    Modified,
    Moved,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum Significance {
    Noise,
    Substantive,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", default)]
pub struct DiffOptions {
    /// 哪些类别的变化算噪声；不在其中的类别（如价格）按实质变化处理。
    pub noise_categories: Vec<NoiseCategory>,
    /// 纯移动是否算噪声。
    pub moved_is_noise: bool,
    /// 判定"同一块被修改"的词级相似度下限，0 到 1。
    pub similarity_threshold: f64,
}

impl Default for DiffOptions {
    fn default() -> Self {
        Self {
            noise_categories: vec![
                NoiseCategory::Date,
                NoiseCategory::RelativeTime,
                NoiseCategory::Price,
                NoiseCategory::Count,
                NoiseCategory::Number,
            ],
            moved_is_noise: false,
            similarity_threshold: 0.5,
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct BlockChange {
    #[serde(rename = "type")]
    pub change: ChangeType,
    pub block_type: BlockKind,
    pub significance: Significance,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub noise_categories: Vec<NoiseCategory>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub old_index: Option<usize>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub new_index: Option<usize>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub old_text: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub new_text: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub old_hash: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub new_hash: Option<String>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub inline: Vec<InlineSegment>,
}

#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct DiffStats {
    pub old_blocks: usize,
    pub new_blocks: usize,
    pub unchanged: usize,
    pub added: usize,
    pub removed: usize,
    pub modified: usize,
    pub moved: usize,
    pub noise: usize,
    pub substantive: usize,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct DiffReport {
    pub changed: bool,
    pub substantive: bool,
    pub blocks: Vec<BlockChange>,
    pub stats: DiffStats,
}

fn hex(h: &[u8; 32]) -> String {
    const D: &[u8; 16] = b"0123456789abcdef";
    let mut s = String::with_capacity(64);
    for b in h {
        s.push(D[(b >> 4) as usize] as char);
        s.push(D[(b & 15) as usize] as char);
    }
    s
}

/// 块的词集合签名：非空白词的哈希，已排序，用于 Dice 相似度。
fn signature(norm: &str) -> Vec<u64> {
    use std::hash::{BuildHasher, BuildHasherDefault, DefaultHasher};
    let hasher = BuildHasherDefault::<DefaultHasher>::default();
    let mut v: Vec<u64> = inline::tokenize(norm)
        .into_iter()
        .filter(|t| !t.trim().is_empty())
        .map(|t| hasher.hash_one(t))
        .collect();
    v.sort_unstable();
    v
}

fn dice(a: &[u64], b: &[u64]) -> f64 {
    if a.len() + b.len() == 0 {
        return 1.0;
    }
    let (mut i, mut j, mut common) = (0, 0, 0usize);
    while i < a.len() && j < b.len() {
        match a[i].cmp(&b[j]) {
            std::cmp::Ordering::Less => i += 1,
            std::cmp::Ordering::Greater => j += 1,
            std::cmp::Ordering::Equal => {
                common += 1;
                i += 1;
                j += 1;
            }
        }
    }
    2.0 * common as f64 / (a.len() + b.len()) as f64
}

const PAIR_WINDOW: usize = 8;

fn intern<'a>(bs: &'a [Block], ids: &mut HashMap<(BlockKind, &'a str), u32>) -> Vec<u32> {
    bs.iter()
        .map(|b| {
            let n = ids.len() as u32;
            *ids.entry((b.kind, b.norm.as_str())).or_insert(n)
        })
        .collect()
}

pub fn diff(old: &str, new: &str) -> DiffReport {
    diff_with(old, new, &DiffOptions::default())
}

pub fn diff_with(old: &str, new: &str, opts: &DiffOptions) -> DiffReport {
    let ob = split_blocks(old);
    let nb = split_blocks(new);
    let mut ids = HashMap::new();
    let oi = intern(&ob, &mut ids);
    let ni = intern(&nb, &mut ids);
    let edits = algo::diff(&oi, &ni);

    let mut stats = DiffStats {
        old_blocks: ob.len(),
        new_blocks: nb.len(),
        ..Default::default()
    };
    let mut events: Vec<(usize, BlockChange)> = Vec::new();

    // 先配对移动：同一哈希在别处被删除又被插入
    let mut consumed = vec![false; edits.len()];
    let mut deleted: HashMap<u32, VecDeque<(usize, usize)>> = HashMap::new();
    for (p, e) in edits.iter().enumerate() {
        if let Edit::Delete(i) = *e {
            deleted.entry(oi[i]).or_default().push_back((p, i));
        }
    }
    for (p, e) in edits.iter().enumerate() {
        if let Edit::Insert(j) = *e
            && let Some((dp, i)) = deleted.get_mut(&ni[j]).and_then(VecDeque::pop_front)
        {
            consumed[dp] = true;
            consumed[p] = true;
            events.push((
                p,
                BlockChange {
                    change: ChangeType::Moved,
                    block_type: nb[j].kind,
                    significance: if opts.moved_is_noise {
                        Significance::Noise
                    } else {
                        Significance::Substantive
                    },
                    noise_categories: vec![],
                    old_index: Some(i),
                    new_index: Some(j),
                    old_text: Some(ob[i].text.clone()),
                    new_text: Some(nb[j].text.clone()),
                    old_hash: Some(hex(&ob[i].hash())),
                    new_hash: Some(hex(&nb[j].hash())),
                    inline: vec![],
                },
            ));
        }
    }

    let added = |p: usize, j: usize| {
        (
            p,
            BlockChange {
                change: ChangeType::Added,
                block_type: nb[j].kind,
                significance: Significance::Substantive,
                noise_categories: vec![],
                old_index: None,
                new_index: Some(j),
                old_text: None,
                new_text: Some(nb[j].text.clone()),
                old_hash: None,
                new_hash: None,
                inline: vec![],
            },
        )
    };
    let removed = |p: usize, i: usize| {
        (
            p,
            BlockChange {
                change: ChangeType::Removed,
                block_type: ob[i].kind,
                significance: Significance::Substantive,
                noise_categories: vec![],
                old_index: Some(i),
                new_index: None,
                old_text: Some(ob[i].text.clone()),
                new_text: None,
                old_hash: None,
                new_hash: None,
                inline: vec![],
            },
        )
    };

    let osig: Vec<OnceCell<Vec<u64>>> = (0..ob.len()).map(|_| OnceCell::new()).collect();
    let nsig: Vec<OnceCell<Vec<u64>>> = (0..nb.len()).map(|_| OnceCell::new()).collect();
    let mut dels: Vec<(usize, usize)> = Vec::new();
    let mut inss: Vec<(usize, usize)> = Vec::new();
    let flush = |dels: &mut Vec<(usize, usize)>,
                 inss: &mut Vec<(usize, usize)>,
                 events: &mut Vec<(usize, BlockChange)>| {
        let mut j = 0;
        for &(dp, i) in dels.iter() {
            let hi = (j + PAIR_WINDOW).min(inss.len());
            // 顺序候选相似度够高就直接采用，省掉窗口内的其余比较
            let mut best: Option<(usize, f64)> = None;
            for k in j..hi {
                if ob[i].kind != nb[inss[k].1].kind {
                    continue;
                }
                let so = osig[i].get_or_init(|| signature(&ob[i].norm));
                let sn = nsig[inss[k].1].get_or_init(|| signature(&nb[inss[k].1].norm));
                let sim = dice(so, sn);
                if sim >= opts.similarity_threshold && best.is_none_or(|(_, s)| sim > s) {
                    best = Some((k, sim));
                    if sim >= 0.8 {
                        break;
                    }
                }
            }
            match best {
                Some((k, _)) => {
                    for &(ip, jj) in &inss[j..k] {
                        events.push(added(ip, jj));
                    }
                    let (ip, jj) = inss[k];
                    j = k + 1;
                    let (o, n) = (&ob[i], &nb[jj]);
                    let (significance, noise_categories) =
                        match noise::classify(&o.norm, &n.norm, &opts.noise_categories) {
                            Some(c) => (Significance::Noise, c),
                            None => (Significance::Substantive, vec![]),
                        };
                    events.push((
                        dp.min(ip),
                        BlockChange {
                            change: ChangeType::Modified,
                            block_type: n.kind,
                            significance,
                            noise_categories,
                            old_index: Some(i),
                            new_index: Some(jj),
                            old_text: Some(o.text.clone()),
                            new_text: Some(n.text.clone()),
                            old_hash: Some(hex(&o.hash())),
                            new_hash: Some(hex(&n.hash())),
                            inline: inline_diff(&o.norm, &n.norm),
                        },
                    ));
                }
                None => events.push(removed(dp, i)),
            }
        }
        for &(ip, jj) in &inss[j..] {
            events.push(added(ip, jj));
        }
        dels.clear();
        inss.clear();
    };

    for (p, e) in edits.iter().enumerate() {
        match *e {
            Edit::Equal(..) => {
                stats.unchanged += 1;
                flush(&mut dels, &mut inss, &mut events);
            }
            _ if consumed[p] => {}
            Edit::Delete(i) => dels.push((p, i)),
            Edit::Insert(j) => inss.push((p, j)),
        }
    }
    flush(&mut dels, &mut inss, &mut events);

    events.sort_by_key(|e| e.0);
    let blocks: Vec<BlockChange> = events.into_iter().map(|e| e.1).collect();
    for b in &blocks {
        match b.change {
            ChangeType::Added => stats.added += 1,
            ChangeType::Removed => stats.removed += 1,
            ChangeType::Modified => stats.modified += 1,
            ChangeType::Moved => stats.moved += 1,
        }
        match b.significance {
            Significance::Noise => stats.noise += 1,
            Significance::Substantive => stats.substantive += 1,
        }
    }
    DiffReport {
        changed: !blocks.is_empty(),
        substantive: stats.substantive > 0,
        blocks,
        stats,
    }
}
