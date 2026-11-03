//! act 的计划缓存（0.97 的“解析一次 → 缓存 → 自愈 → LLM 兜底”）：模型定下的步骤和它们的目标元素。
//!
//! 键是“注册域 + URL 形状 + 指令哈希”（[`super::super::autoschema::keyed`]），存在 `pagesift-store`。
//! 目标元素记成 0.93 的 [`Target`]（稳健选择器 + 结构指纹），下次按这个顺序找：
//! 1. 选择器恰好命中一个元素，且文字和记下的一样；
//! 2. 否则按指纹 `locate`，找到后写回新选择器；
//! 3. 有一个找不到（`locate` 拒绝猜也算）就整个计划作废，由调用方重新问模型。
//!
//! 第 1、2 步都查文字：选择器还能命中、指纹也像，不等于还是同一个东西，“购买”的位置换成了“删除”时不能照点。
//! 整个计划要么全部找到才执行，要么一步不做：不会做到一半才发现页面变了。

use pagesift_extract::locate::{Fingerprint, Located, Target, locate, robust_selector};
use pagesift_html::{Html, NodeRef, Selector};
use serde::{Deserialize, Serialize};
use serde_json::Value;

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub(super) struct PlanStep {
    /// 模型给的步骤（去掉了 `id`）。要输入的文字里只留 `{{secret:NAME}}` 占位符，不是秘密的值。
    pub step: Value,
    /// 针对元素的步骤的目标；按键、滚动、等待没有。
    pub target: Option<Target>,
    pub role: String,
    pub name: String,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub(super) struct Plan {
    pub steps: Vec<PlanStep>,
    /// 这个任务要不要模型作答（只是做动作时不用）。
    pub answer: bool,
}

/// 按 CSS 路径在取回的 HTML 上找到元素，记下稳健选择器和指纹。第二个值：它是不是密码框。
/// 路径没有恰好命中一个元素时（比如元素在被内联的 iframe 里）返回 `None`，这个计划就不缓存。
pub(super) fn capture_target(html: &str, path: &str) -> Option<(Target, bool)> {
    let doc = Html::parse(html);
    let selector = Selector::parse(path).ok()?;
    let mut hits = doc.root().select(&selector);
    let node = hits.next()?;
    if hits.next().is_some() {
        return None;
    }
    let robust = robust_selector(node, doc.root()).unwrap_or_else(|| path.to_string());
    let target = Target::from_node(node, &robust, None).ok()?;
    let password = node
        .as_element()
        .and_then(|e| e.attr("type"))
        .is_some_and(|t| t.eq_ignore_ascii_case("password"));
    Some((target, password))
}

fn same_text(node: NodeRef<'_>, target: &Target) -> bool {
    Fingerprint::of(node).is_some_and(|f| f.text == target.fingerprint.text)
}

/// 指纹找回来的元素，文字要和记下的有重叠（都没有文字也算，比如输入框）。指纹看的是属性和位置，
/// 同一个位置上“购买”换成“删除”它也会接受；抽取可以容忍文字变（文字本来就是要抽的内容），
/// 点击不行。这里只判“完全不重叠”，部分改动（“加载更多 (3)”）照常接受。
fn text_overlaps(node: NodeRef<'_>, target: &Target) -> bool {
    let want = &target.fingerprint.text;
    Fingerprint::of(node)
        .is_some_and(|f| (f.text.is_empty() && want.is_empty()) || !f.text.is_disjoint(want))
}

/// 找一个目标。第二个值：是不是走了指纹（选择器要写回）。
fn find<'a>(root: NodeRef<'a>, target: &Target) -> Option<(NodeRef<'a>, bool)> {
    let node = match locate(root, target) {
        Located::Exact(n) if same_text(n, target) => return Some((n, false)),
        Located::Exact(_) => {
            // 选择器命中了文字不同的元素：当作选择器失效，只凭指纹找（空选择器解析不出来，不会精确命中）
            let mut blind = target.clone();
            blind.selector.clear();
            locate(root, &blind).node()?
        }
        other => other.node()?,
    };
    text_overlaps(node, target).then_some((node, true))
}

/// 在当前页面上找计划的全部目标。全部找到时返回走了指纹的步骤下标（它们的选择器和指纹已更新，
/// 调用方写回缓存）；有一个找不到时返回 `None`，计划作废。
pub(super) fn resolve(html: &str, plan: &mut Plan) -> Option<Vec<usize>> {
    let doc = Html::parse(html);
    let root = doc.root();
    let mut healed = Vec::new();
    for (i, s) in plan.steps.iter_mut().enumerate() {
        let Some(target) = &mut s.target else {
            continue;
        };
        let (node, relocated) = find(root, target)?;
        if relocated {
            target.selector = robust_selector(node, root)?;
            target.fingerprint = Fingerprint::of(node)?;
            healed.push(i);
        }
    }
    Some(healed)
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn page(body: &str) -> String {
        let nav: String = (0..12)
            .map(|i| format!("<a href='/c{i}'>Category number {i}</a>"))
            .collect();
        format!(
            "<html><body><nav>{nav}</nav><main><h1>Shop</h1><p>Some words about the shop, long enough to count.</p>{body}</main></body></html>"
        )
    }

    fn plan_for(html: &str, path: &str) -> Plan {
        let (target, _) = capture_target(html, path).unwrap();
        Plan {
            steps: vec![
                PlanStep {
                    step: json!({"action": "click"}),
                    target: Some(target),
                    role: "button".into(),
                    name: "Buy now".into(),
                },
                // 按键没有目标，不影响查找
                PlanStep {
                    step: json!({"action": "press", "key": "Enter"}),
                    target: None,
                    role: String::new(),
                    name: String::new(),
                },
            ],
            answer: false,
        }
    }

    const PATH: &str = "html > body > main > button:nth-of-type(1)";

    #[test]
    fn captures_a_stable_selector_and_notices_password_fields() {
        let html = page("<button id='buy'>Buy now</button>");
        let (t, password) = capture_target(&html, PATH).unwrap();
        assert!(t.selector.contains("buy"), "{}", t.selector);
        assert!(!password);
        let html = page("<input name='pw' type='Password'>");
        let path = "html > body > main > input:nth-of-type(1)";
        assert!(capture_target(&html, path).unwrap().1);
        assert!(capture_target(&html, "section").is_none());
    }

    #[test]
    fn an_unchanged_page_needs_no_healing() {
        let html = page("<button id='buy'>Buy now</button>");
        let mut plan = plan_for(&html, PATH);
        let before = plan.clone();
        assert_eq!(resolve(&html, &mut plan), Some(vec![]));
        assert_eq!(plan, before);
    }

    #[test]
    fn a_renamed_element_is_found_by_its_fingerprint() {
        let old = page(
            "<button id='buy' class='cta primary'>Buy now</button><button id='wish'>Add to wishlist</button>",
        );
        let new = page(
            "<div><button id='purchase-1' class='cta primary'>Buy now</button></div><button id='wish'>Add to wishlist</button>",
        );
        let mut plan = plan_for(&old, PATH);
        assert_eq!(resolve(&new, &mut plan), Some(vec![0]));
        let t = plan.steps[0].target.as_ref().unwrap();
        assert_ne!(t.selector, "#buy");
        // 写回之后再查就是精确命中
        assert_eq!(resolve(&new, &mut plan), Some(vec![]));
    }

    #[test]
    fn a_selector_that_now_means_something_else_does_not_count() {
        let old = page("<button id='act'>Buy now</button>");
        let new = page("<button id='act'>Delete my account</button>");
        let mut plan = plan_for(&old, PATH);
        // 选择器还能命中，属性和位置也都一样，文字完全变了：绝不能当成原来那个按钮
        assert_eq!(resolve(&new, &mut plan), None);
        // 只改了一部分文字：还是那个按钮，走指纹写回
        let reworded = page("<button id='act'>Buy it now</button>");
        let mut plan = plan_for(&old, PATH);
        assert_eq!(resolve(&reworded, &mut plan), Some(vec![0]));
    }

    #[test]
    fn a_missing_element_voids_the_plan() {
        let old = page("<button id='buy' class='cta'>Buy now</button>");
        let new = page("<p>Sold out, nothing to do here today.</p>");
        let mut plan = plan_for(&old, PATH);
        assert_eq!(resolve(&new, &mut plan), None);
    }
}
