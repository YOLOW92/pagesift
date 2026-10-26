//! SmartArt（DrawingML Diagrams，ECMA-376 第 1 部分 21.4）：读数据部件 `dgm:dataModel`，
//! 按节点之间的父子关系输出成嵌套列表。
//!
//! 数据模型里 `dgm:pt` 是点（`type` 缺省为 `node`；`doc` 是根，`asst` 是助理节点，`parTrans`、
//! `sibTrans`、`pres` 是连接线和版式用的点，没有内容），`dgm:cxn` 是连接（`type` 缺省为 `parOf`：
//! `srcId` 是父、`destId` 是子，`srcOrd` 是子节点的次序）。版式（layout）只管怎么画，不影响内容，
//! 所以不读；也不读缓存的绘图部件（`dsp:drawing`），它的文字和数据模型一样，但没有层级。

use std::collections::{HashMap, HashSet};

use super::shapes::text_body;
use crate::Error;
use crate::md::Block;
use crate::xml::{Reader, Tok, local_name};

/// 列表最深嵌套（和 Markdown 渲染一致）。
const MAX_LEVEL: usize = 8;

struct Point {
    kind: String,
    paras: Vec<String>,
}

/// 把一个 SmartArt 数据部件变成列表项追加到 `blocks`。
pub(super) fn read(xml: &str, blocks: &mut Vec<Block>) -> Result<(), Error> {
    let mut points: HashMap<String, Point> = HashMap::new();
    let mut order: Vec<String> = Vec::new();
    // 父 → [(srcOrd, 子)]
    let mut children: HashMap<String, Vec<(u32, String)>> = HashMap::new();
    let mut has_parent: HashSet<String> = HashSet::new();
    let mut r = Reader::new(xml);
    let mut current: Option<String> = None;
    while let Some(t) = r.next()? {
        match t {
            Tok::Start(name, a) => match local_name(name) {
                "pt" => {
                    let id = a.get("modelId").map(|v| v.into_owned()).unwrap_or_default();
                    let kind = a.get("type").map_or("node".to_string(), |v| v.into_owned());
                    order.push(id.clone());
                    points.insert(
                        id.clone(),
                        Point {
                            kind,
                            paras: Vec::new(),
                        },
                    );
                    current = Some(id);
                }
                "t" => {
                    let paras = text_body(&mut r)?;
                    if let Some(p) = current.as_ref().and_then(|id| points.get_mut(id)) {
                        p.paras = paras
                            .into_iter()
                            .map(|p| p.text.replace('\n', " "))
                            .filter(|t| !t.trim().is_empty())
                            .collect();
                    }
                }
                "cxn" => {
                    let parent_of = a.get("type").is_none_or(|t| t == "parOf");
                    if let (true, Some(src), Some(dest)) =
                        (parent_of, a.get("srcId"), a.get("destId"))
                    {
                        let ord = a.get("srcOrd").and_then(|v| v.parse().ok()).unwrap_or(0);
                        has_parent.insert(dest.to_string());
                        children
                            .entry(src.into_owned())
                            .or_default()
                            .push((ord, dest.into_owned()));
                    }
                }
                "prSet" | "spPr" | "extLst" | "bg" | "whole" => r.skip()?,
                _ => {}
            },
            Tok::End(name) if local_name(name) == "pt" => current = None,
            _ => {}
        }
    }
    for list in children.values_mut() {
        list.sort_by_key(|(ord, _)| *ord);
    }
    // 根：doc 点；没有 doc 时，没有父节点的内容点按出现顺序当根
    let roots: Vec<String> = match order.iter().find(|id| points[*id].kind == "doc") {
        Some(doc) => children
            .get(doc)
            .map(|c| c.iter().map(|(_, id)| id.clone()).collect())
            .unwrap_or_default(),
        None => order
            .iter()
            .filter(|id| content_point(&points[*id]) && !has_parent.contains(*id))
            .cloned()
            .collect(),
    };
    let mut seen = HashSet::new();
    for id in &roots {
        walk(id, 0, &points, &children, &mut seen, blocks);
    }
    Ok(())
}

fn content_point(p: &Point) -> bool {
    matches!(p.kind.as_str(), "node" | "asst")
}

/// 深度优先输出：节点的第一段是列表项，其余段落是它下一级的项；没有文字的节点不占层级。
fn walk(
    id: &str,
    level: usize,
    points: &HashMap<String, Point>,
    children: &HashMap<String, Vec<(u32, String)>>,
    seen: &mut HashSet<String>,
    blocks: &mut Vec<Block>,
) {
    // 连接成环或重复引用时只输出一次
    if !seen.insert(id.to_string()) {
        return;
    }
    let Some(p) = points.get(id).filter(|p| content_point(p)) else {
        return;
    };
    let mut next = level;
    if let Some((first, rest)) = p.paras.split_first() {
        push(blocks, level, first);
        for extra in rest {
            push(blocks, level + 1, extra);
        }
        next = level + 1;
    }
    for (_, child) in children.get(id).into_iter().flatten() {
        walk(child, next, points, children, seen, blocks);
    }
}

fn push(blocks: &mut Vec<Block>, level: usize, text: &str) {
    blocks.push(Block::Item {
        level: level.min(MAX_LEVEL) as u8,
        ordered: false,
        number: 0,
        text: text.to_string(),
    });
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::md::render;

    #[test]
    fn nodes_become_nested_list_in_child_order() {
        // 子节点故意倒着写，次序看 srcOrd；sibTrans/pres 点和 presOf 连接都不算内容
        let xml = r#"<dgm:dataModel><dgm:ptLst>
<dgm:pt modelId="d" type="doc"><dgm:t><a:p/></dgm:t></dgm:pt>
<dgm:pt modelId="b"><dgm:t><a:p><a:r><a:t>Second</a:t></a:r></a:p></dgm:t></dgm:pt>
<dgm:pt modelId="a"><dgm:prSet/><dgm:t><a:p><a:r><a:t>First</a:t></a:r></a:p><a:p><a:r><a:t>more</a:t></a:r></a:p></dgm:t></dgm:pt>
<dgm:pt modelId="a1"><dgm:t><a:p><a:r><a:t>Child *one*</a:t></a:r></a:p></dgm:t></dgm:pt>
<dgm:pt modelId="s" type="sibTrans"><dgm:t><a:p><a:r><a:t>x</a:t></a:r></a:p></dgm:t></dgm:pt>
<dgm:pt modelId="p" type="pres"/>
</dgm:ptLst><dgm:cxnLst>
<dgm:cxn modelId="1" srcId="d" destId="b" srcOrd="1"/>
<dgm:cxn modelId="2" srcId="d" destId="a" srcOrd="0"/>
<dgm:cxn modelId="3" type="parOf" srcId="a" destId="a1" srcOrd="0"/>
<dgm:cxn modelId="4" type="presOf" srcId="a" destId="p"/>
<dgm:cxn modelId="5" srcId="a1" destId="d" srcOrd="0"/>
</dgm:cxnLst></dgm:dataModel>"#;
        let mut blocks = Vec::new();
        read(xml, &mut blocks).unwrap();
        assert_eq!(
            render(&blocks),
            "- First\n    - more\n    - Child \\*one\\*\n- Second\n"
        );
    }

    #[test]
    fn without_doc_point_roots_are_parentless_nodes() {
        let xml = r#"<dgm:dataModel><dgm:ptLst>
<dgm:pt modelId="a"><dgm:t><a:p><a:r><a:t>A</a:t></a:r></a:p></dgm:t></dgm:pt>
<dgm:pt modelId="b"><dgm:t><a:p><a:r><a:t>B</a:t></a:r></a:p></dgm:t></dgm:pt>
</dgm:ptLst><dgm:cxnLst><dgm:cxn modelId="1" srcId="a" destId="b"/></dgm:cxnLst></dgm:dataModel>"#;
        let mut blocks = Vec::new();
        read(xml, &mut blocks).unwrap();
        assert_eq!(render(&blocks), "- A\n    - B\n");
    }
}
