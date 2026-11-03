//! 渲染完没有关掉的页面（`/interact`）：继续执行动作、取无障碍树、按节点点击和输入。
//!
//! 事件监听和子请求拦截在保留期间一直在跑，所以点击触发的导航和子请求照样过网络检查。

use std::collections::BTreeMap;
use std::sync::Arc;

use serde_json::{Value, json};

use super::actions::{ActionError, run_action};
use super::monitor::check_interception;
use super::{Metrics, PageCtx, Shared};
use crate::conn::{Connection, Session};
use crate::error::{Error, Result};
use crate::options::*;

/// 无障碍树的一个节点：给模型看的页面快照的原料。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AxNode {
    /// `DOM.BackendNodeId`，点击和输入用它定位。
    pub backend_id: i64,
    pub role: String,
    pub name: String,
    pub value: Option<String>,
}

pub(crate) struct LivePage {
    session: Session,
    shared: Arc<Shared>,
    metrics: Metrics,
    virtual_items: BTreeMap<String, Vec<String>>,
    opts: RenderOptions,
}

/// CDP 的 `AXValue`：`{type, value}`，取字符串形式。
fn ax_text(v: Option<&Value>) -> Option<String> {
    match v?.get("value")? {
        Value::String(s) => Some(s.clone()),
        Value::Null => None,
        other => Some(other.to_string()),
    }
}

/// 解析 `Accessibility.getFullAXTree` 的结果：去掉被忽略的节点和没有对应 DOM 节点的节点。
fn parse_ax_nodes(tree: &Value) -> Vec<AxNode> {
    tree.get("nodes")
        .and_then(Value::as_array)
        .into_iter()
        .flatten()
        .filter(|n| n.get("ignored").and_then(Value::as_bool) != Some(true))
        .filter_map(|n| {
            Some(AxNode {
                backend_id: n.get("backendDOMNodeId")?.as_i64()?,
                role: ax_text(n.get("role")).unwrap_or_default(),
                name: ax_text(n.get("name")).unwrap_or_default(),
                value: ax_text(n.get("value")).filter(|v| !v.is_empty()),
            })
        })
        .collect()
}

/// 在节点上执行的函数里，先取到元素本身：无障碍树里的文字节点没有 `scrollIntoView`。
const ELEMENT: &str = "const e = this.nodeType === 1 ? this : this.parentElement;";

impl LivePage {
    pub(super) fn new(
        session: Session,
        shared: Arc<Shared>,
        metrics: Metrics,
        virtual_items: BTreeMap<String, Vec<String>>,
        opts: RenderOptions,
    ) -> Self {
        Self {
            session,
            shared,
            metrics,
            virtual_items,
            opts,
        }
    }

    fn ctx<'a>(&'a self, conn: &'a Connection) -> PageCtx<'a> {
        PageCtx {
            conn,
            session: &self.session,
            shared: &self.shared,
            metrics: self.metrics,
            opts: &self.opts,
            virtual_items: self.virtual_items.clone(),
        }
    }

    pub(crate) async fn run_actions(
        &self,
        conn: &Connection,
        actions: &[Action],
    ) -> Result<ActionOutputs> {
        let ctx = self.ctx(conn);
        let mut out = ActionOutputs::default();
        for (index, action) in actions.iter().enumerate() {
            run_action(&ctx, action, &mut out)
                .await
                .map_err(|e| match e {
                    ActionError::Cdp(e) => e,
                    ActionError::Failed(message) => Error::Action { index, message },
                })?;
            check_interception(&self.shared)?;
        }
        Ok(out)
    }

    pub(crate) async fn capture(&self, conn: &Connection) -> Result<RenderedPage> {
        let captured = self.ctx(conn).html().await?;
        // 和渲染时一样：停在浏览器自己的错误页时不把它当正文
        if captured.url.starts_with("chrome-error:") {
            return Err(Error::Navigation(
                "the page navigated to an address that could not be loaded".into(),
            ));
        }
        check_interception(&self.shared)?;
        Ok(RenderedPage {
            final_url: captured.url,
            status: *self.shared.status.lock().unwrap(),
            content_type: self.shared.content_type.lock().unwrap().clone(),
            html: captured.html,
            screenshot: None,
            actions: ActionOutputs::default(),
            blocked_requests: self
                .shared
                .blocked
                .lock()
                .unwrap()
                .drain(..)
                .map(|(_, b)| b)
                .collect(),
            iframes_inlined: captured.inlined,
            iframes_skipped: captured.skipped,
            virtual_scroll_items: None,
            early_stopped: false,
            first_content: None,
            timings: Timings::default(),
        })
    }

    pub(crate) async fn ax_nodes(&self) -> Result<Vec<AxNode>> {
        self.session.call("Accessibility.enable", json!({})).await?;
        let tree = self
            .session
            .call("Accessibility.getFullAXTree", json!({}))
            .await?;
        Ok(parse_ax_nodes(&tree))
    }

    /// 在节点上执行一个函数，返回它的值。
    async fn on_node(&self, backend_id: i64, body: &str) -> Result<Value> {
        let node = self
            .session
            .call("DOM.resolveNode", json!({ "backendNodeId": backend_id }))
            .await?;
        let object = node
            .pointer("/object/objectId")
            .and_then(Value::as_str)
            .ok_or_else(|| Error::UnexpectedResponse("DOM.resolveNode: no objectId".into()))?;
        let r = self
            .session
            .call(
                "Runtime.callFunctionOn",
                json!({
                    "objectId": object,
                    "functionDeclaration": format!("function() {{ {ELEMENT} {body} }}"),
                    "returnByValue": true,
                    "userGesture": true,
                }),
            )
            .await?;
        if let Some(ex) = r.get("exceptionDetails") {
            return Err(Error::Action {
                index: 0,
                message: ex
                    .pointer("/exception/description")
                    .or_else(|| ex.get("text"))
                    .and_then(Value::as_str)
                    .unwrap_or("JavaScript exception")
                    .to_owned(),
            });
        }
        Ok(r.pointer("/result/value").cloned().unwrap_or(Value::Null))
    }

    pub(crate) async fn click_node(&self, conn: &Connection, backend_id: i64) -> Result<()> {
        let at = self
            .on_node(
                backend_id,
                "e.scrollIntoView({block: 'center', inline: 'center'}); \
                 const r = e.getBoundingClientRect(); \
                 if (r.width === 0 && r.height === 0) { e.click(); return []; } \
                 return [r.x + r.width / 2, r.y + r.height / 2];",
            )
            .await?;
        if let (Some(x), Some(y)) = (
            at.get(0).and_then(Value::as_f64),
            at.get(1).and_then(Value::as_f64),
        ) {
            for ty in ["mouseMoved", "mousePressed", "mouseReleased"] {
                self.session
                    .call(
                        "Input.dispatchMouseEvent",
                        json!({ "type": ty, "x": x, "y": y, "button": "left", "clickCount": 1 }),
                    )
                    .await?;
            }
        }
        self.ctx(conn).settle().await;
        check_interception(&self.shared)
    }

    pub(crate) async fn write_node(
        &self,
        conn: &Connection,
        backend_id: i64,
        text: &str,
    ) -> Result<()> {
        self.on_node(
            backend_id,
            "e.focus(); if (typeof e.select === 'function') e.select();",
        )
        .await?;
        self.session
            .call("Input.insertText", json!({ "text": text }))
            .await?;
        self.ctx(conn).settle().await;
        check_interception(&self.shared)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn keeps_named_nodes_with_a_dom_node() {
        let tree = json!({"nodes": [
            {"nodeId": "1", "ignored": true, "backendDOMNodeId": 1, "role": {"value": "none"}},
            {"nodeId": "2", "ignored": false, "backendDOMNodeId": 7,
             "role": {"type": "role", "value": "button"}, "name": {"type": "computedString", "value": "Load more"}},
            {"nodeId": "3", "ignored": false, "backendDOMNodeId": 8,
             "role": {"value": "textbox"}, "name": {"value": "Email"}, "value": {"type": "string", "value": "a@b"}},
            {"nodeId": "4", "ignored": false, "role": {"value": "generic"}},
        ]});
        let nodes = parse_ax_nodes(&tree);
        assert_eq!(nodes.len(), 2);
        assert_eq!(nodes[0].role, "button");
        assert_eq!(nodes[0].name, "Load more");
        assert_eq!(nodes[1].value.as_deref(), Some("a@b"));
    }
}
