//! 元素定位：一套 `locate` 取代 0.91 的三套重定位（拓扑指纹、`Anchor`、`FuzzyAnchor`）。
//!
//! 顺序固定，每一步的结果都如实返回：
//! 1. **选择器精确命中**：在作用域内恰好命中一个元素（配了身份属性时，身份也要对上）→ [`Located::Exact`]；
//! 2. **身份属性**：配了业务身份属性（如 `data-sku`）时用 [`crate::Anchor`] 的规则全文查找：
//!    恰好一个 → [`Located::Anchored`]，没有 → [`Located::Missing`]，重复 → [`Located::Ambiguous`]。
//!    身份对不上就报 Missing，**不退回到相似度去猜**；
//! 3. **结构指纹**：作用域内每个元素和保存的 [`Fingerprint`] 逐个特征比相似度，按“文档里有多少元素
//!    同样像”换算成似然比并合并（见 `posterior.rs`）。最高者是目标的后验概率 ≥ 1 − δ 才接受为
//!    [`Located::Relocated`]；否则 Ambiguous（没有候选时 Missing）。
//!
//! 重定位成功后用 [`robust_selector`] 生成新选择器，调用方写回存储（见 `pagesift-store` 的
//! `Selectors`），结果里报 `relocated` 警告。

mod fingerprint;
mod posterior;
mod robust;
mod similar;

use pagesift_html::{NodeRef, Selector};
use serde::{Deserialize, Serialize};

pub use fingerprint::{Fingerprint, is_hash_like};
pub use posterior::{DELTA, chi2_even_tail};
pub use robust::robust_selector;
pub use similar::{Similar, find_similar};

/// 要找的元素：当前选择器、可选的业务身份、上次命中时记下的指纹。可序列化，持久化后跨页面版本使用。
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Target {
    pub selector: String,
    /// 业务身份属性名和值（例如 `data-sku` = `A1`）。
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub identity: Option<(String, String)>,
    pub fingerprint: Fingerprint,
}

impl Target {
    /// 在作用域内用 `selector` 取到恰好一个元素，记下它的指纹（可选身份属性）。
    pub fn capture(
        scope: NodeRef<'_>,
        selector: &str,
        identity_attribute: Option<&str>,
    ) -> Result<Self, String> {
        let sel = Selector::parse(selector).map_err(|e| e.to_string())?;
        let mut hits = scope.select(&sel);
        let node = hits.next().ok_or("selector matches nothing")?;
        if hits.next().is_some() {
            return Err("selector must match exactly one element".into());
        }
        Self::from_node(node, selector, identity_attribute)
    }

    pub fn from_node(
        node: NodeRef<'_>,
        selector: &str,
        identity_attribute: Option<&str>,
    ) -> Result<Self, String> {
        let el = node.as_element().ok_or("not an element")?;
        let identity = match identity_attribute {
            Some(a) => Some((
                a.to_string(),
                el.attr(a)
                    .filter(|v| !v.trim().is_empty())
                    .ok_or("stable business identity is required")?
                    .to_string(),
            )),
            None => None,
        };
        Ok(Self {
            selector: selector.to_string(),
            identity,
            fingerprint: Fingerprint::of(node).ok_or("not an element")?,
        })
    }
}

/// [`locate`] 的结果。
#[derive(Debug, Clone, PartialEq)]
pub enum Located<'a> {
    /// 选择器照旧命中。
    Exact(NodeRef<'a>),
    /// 选择器失效，按身份属性找到。
    Anchored(NodeRef<'a>),
    /// 选择器失效，按结构指纹找到。`score` 是等权平均相似度（展示用），`posterior` 是它是目标的
    /// 后验概率（≥ 1 − δ），`p` 是“只是背景里最像的一个”的 p 值（诊断用）。
    Relocated {
        node: NodeRef<'a>,
        score: f64,
        posterior: f64,
        p: f64,
    },
    Missing,
    /// 证据不够或有多个说得过去的候选，拒绝猜：`best` 是最高者的等权相似度，`posterior` 是它的后验，
    /// `p` 是背景检验的 p 值，`candidate` 是最高者（只供诊断，不是结果）。
    Ambiguous {
        best: f64,
        posterior: f64,
        p: f64,
        candidate: Option<NodeRef<'a>>,
    },
}

impl<'a> Located<'a> {
    pub fn node(&self) -> Option<NodeRef<'a>> {
        match self {
            Located::Exact(n) | Located::Anchored(n) | Located::Relocated { node: n, .. } => {
                Some(*n)
            }
            _ => None,
        }
    }

    /// 走到了哪一步：`exact`、`anchor`、`fingerprint`、`missing`、`ambiguous`。
    pub fn step(&self) -> &'static str {
        match self {
            Located::Exact(_) => "exact",
            Located::Anchored(_) => "anchor",
            Located::Relocated { .. } => "fingerprint",
            Located::Missing => "missing",
            Located::Ambiguous { .. } => "ambiguous",
        }
    }
}

/// 在 `scope`（文档根或某个元素）内找 `target`。
pub fn locate<'a>(scope: NodeRef<'a>, target: &Target) -> Located<'a> {
    exact_or_anchor(scope, target).unwrap_or_else(|| {
        let c = candidates(scope, &target.fingerprint);
        judge(scope, &c, &c.scores, false)
    })
}

/// 找列表项：和 [`locate`] 相同，只是最高分的结构相同的兄弟（同一列表的其他项）不算竞争者——
/// 找回列表时取哪一项都一样，之后由 [`find_similar`] 拼出整组的选择器。
pub fn locate_list<'a>(scope: NodeRef<'a>, target: &Target) -> Located<'a> {
    exact_or_anchor(scope, target).unwrap_or_else(|| {
        let c = candidates(scope, &target.fingerprint);
        judge(scope, &c, &c.scores, true)
    })
}

/// 在每个列表项里各找一次字段。列表项可交换，零分布在各项之间合并估计（一个列表项里只有几个
/// 元素，单独估计太粗）；每一项的后验仍在本项的候选之间算。
pub fn locate_each<'a>(scopes: &[NodeRef<'a>], target: &Target) -> Vec<Located<'a>> {
    let early: Vec<Option<Located<'a>>> =
        scopes.iter().map(|s| exact_or_anchor(*s, target)).collect();
    let cands: Vec<Candidates<'a>> = scopes
        .iter()
        .map(|s| candidates(*s, &target.fingerprint))
        .collect();
    let pool: Vec<posterior::Scores> = cands.iter().flat_map(|c| c.scores.clone()).collect();
    early
        .into_iter()
        .zip(scopes.iter().zip(&cands))
        .map(|(e, (s, c))| e.unwrap_or_else(|| judge(*s, c, &pool, false)))
        .collect()
}

/// 第①②步；返回 `None` 表示要走第③步。
fn exact_or_anchor<'a>(scope: NodeRef<'a>, target: &Target) -> Option<Located<'a>> {
    // ① 选择器精确命中
    if let Ok(sel) = Selector::parse(&target.selector) {
        let mut hits = scope.select(&sel);
        if let (Some(n), None) = (hits.next(), hits.next()) {
            let identity_ok = target.identity.as_ref().is_none_or(|(k, v)| {
                n.as_element()
                    .is_some_and(|e| e.attr(k) == Some(v.as_str()))
            });
            if identity_ok {
                return Some(Located::Exact(n));
            }
        }
    }
    // ② 身份属性：对不上就是 Missing，不猜
    let (k, v) = target.identity.as_ref()?;
    let mut found = scope.descendants().filter(|n| {
        n.as_element()
            .is_some_and(|e| e.attr(k) == Some(v.as_str()))
    });
    Some(match (found.next(), found.next()) {
        (None, _) => Located::Missing,
        (Some(n), None) => Located::Anchored(n),
        (Some(_), Some(_)) => Located::Ambiguous {
            best: 1.0,
            posterior: 0.5,
            p: 1.0,
            candidate: None,
        },
    })
}

struct Candidates<'a> {
    nodes: Vec<NodeRef<'a>>,
    scores: Vec<posterior::Scores>,
    mean: Vec<f64>,
}

fn candidates<'a>(scope: NodeRef<'a>, fp: &Fingerprint) -> Candidates<'a> {
    let mut c = Candidates {
        nodes: Vec::new(),
        scores: Vec::new(),
        mean: Vec::new(),
    };
    for n in scope
        .descendants()
        .filter(|n| n.is_element() && *n != scope)
    {
        if let Some(f) = Fingerprint::of(n) {
            c.nodes.push(n);
            c.scores.push(fp.features(&f));
            c.mean.push(fp.similarity(&f));
        }
    }
    c
}

/// 按 `posterior.rs` 的两个条件判决；`pool` 是估计零分布的候选（本作用域或合并的各列表项）。
fn judge<'a>(
    scope: NodeRef<'a>,
    c: &Candidates<'a>,
    pool: &[posterior::Scores],
    group: bool,
) -> Located<'a> {
    let lr = posterior::log_lr(&c.scores, pool);
    let Some(bi) = (0..lr.len()).max_by(|a, b| lr[*a].0.total_cmp(&lr[*b].0)) else {
        return Located::Missing;
    };
    let best = c.nodes[bi];
    let mut peers = vec![best];
    if group && let Some(sim) = find_similar(best, scope).filter(|s| s.depth == 0) {
        peers = sim.items;
    }
    // 最高者的祖先和后代（包装层、内层文字）是同一个东西，不算竞争者
    let others: Vec<f64> = (0..lr.len())
        .filter(|i| {
            let n = c.nodes[*i];
            !peers.iter().any(|p| n == *p || related(n, *p))
        })
        .map(|i| lr[i].0)
        .collect();
    let (ok, post, p) = posterior::decide(lr[bi], &others, pool.len());
    if ok {
        Located::Relocated {
            node: best,
            score: c.mean[bi],
            posterior: post,
            p,
        }
    } else {
        Located::Ambiguous {
            best: c.mean[bi],
            posterior: post,
            p,
            candidate: Some(best),
        }
    }
}

fn related(a: NodeRef<'_>, b: NodeRef<'_>) -> bool {
    a.ancestors().any(|x| x == b) || b.ancestors().any(|x| x == a)
}

#[cfg(test)]
mod tests;
