//! 保留的页面（`/interact`）：渲染完不关，标签页一直占着池的名额，直到调用方关掉或丢弃。

use std::sync::Arc;
use std::time::Duration;

use crate::conn::Connection;
use crate::error::Result;
use crate::options::{Action, ActionOutputs, RenderOptions, RenderedPage};
use crate::page::{AxNode, LivePage, PageResources};

use super::PoolInner;
use super::resources::mem_available_mb;

/// 关页面的时限，同 [`super::inner::Lease`] 的清理。
/// [待迁移] 0.81 的清理时限 5 s
const CLEANUP: Duration = Duration::from_secs(5);

/// 还能留多少页面的依据：池的内存准入用的同一组数（实测，见 [`super::memory`]）。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct SessionRoom {
    /// 系统现在的可用内存（MB）；读不到或关了内存检查（`min_available_memory_mb = 0`）时为 `None`。
    pub available_mb: Option<u64>,
    /// 开新标签后系统至少要剩的内存（MB）。
    pub reserve_mb: u64,
    /// 一个标签的内存（MB）。
    pub tab_mb: u64,
    /// 池最多能开的标签数（浏览器数上限 × 每个浏览器的标签数）。
    pub max_tabs: usize,
}

impl super::BrowserPool {
    /// 现在的内存和标签余量。读 `/proc/meminfo`，只在回收会话时用，不在热路径上。
    pub fn session_room(&self) -> SessionRoom {
        let cfg = &self.inner.cfg;
        SessionRoom {
            available_mb: (cfg.min_available_memory_mb > 0)
                .then(mem_available_mb)
                .flatten(),
            reserve_mb: self.inner.mem.reserve_mb(),
            tab_mb: self.inner.mem.tab_mb(),
            max_tabs: cfg.max_browsers * cfg.max_tabs_per_browser,
        }
    }
}

/// 一个保留的页面。丢弃时在后台关页面、还名额；调用方要等关完就用 [`KeptPage::close`]。
pub struct KeptPage {
    pool: Arc<PoolInner>,
    slot_id: u64,
    conn: Connection,
    res: PageResources,
    live: LivePage,
    done: bool,
}

impl KeptPage {
    pub(super) fn new(
        pool: Arc<PoolInner>,
        slot_id: u64,
        conn: Connection,
        res: PageResources,
        live: LivePage,
    ) -> Self {
        Self {
            pool,
            slot_id,
            conn,
            res,
            live,
            done: false,
        }
    }

    /// 浏览器和连接还在。
    pub fn is_alive(&self) -> bool {
        !self.conn.is_closed()
    }

    /// 依次执行动作。动作的校验规则和渲染时相同（数量、等待总时长、`executeJavascript` 开关）。
    /// 调用方负责时限：`wait` 一个等不到的选择器时不会自己返回。
    pub async fn run_actions(&self, actions: &[Action]) -> Result<ActionOutputs> {
        RenderOptions {
            actions: actions.to_vec(),
            ..RenderOptions::default()
        }
        .validate(self.pool.cfg.allow_execute_javascript)?;
        self.live.run_actions(&self.conn, actions).await
    }

    /// 当前页面的结果（最终网址、状态码、HTML）。时序和动作产出字段为空。
    pub async fn capture(&self) -> Result<RenderedPage> {
        self.live.capture(&self.conn).await
    }

    /// 页面的无障碍树（`Accessibility.getFullAXTree`），只有主框架。
    pub async fn ax_nodes(&self) -> Result<Vec<AxNode>> {
        self.live.ax_nodes().await
    }

    /// 点击无障碍树里的一个节点。
    pub async fn click_node(&self, backend_id: i64) -> Result<()> {
        self.live.click_node(&self.conn, backend_id).await
    }

    /// 选中节点里已有的内容并输入 `text`（替换原有内容）。
    pub async fn write_node(&self, backend_id: i64, text: &str) -> Result<()> {
        self.live.write_node(&self.conn, backend_id, text).await
    }

    /// 关掉页面，还名额。
    pub async fn close(mut self) {
        self.done = true;
        let _ = tokio::time::timeout(CLEANUP, self.res.cleanup(&self.conn)).await;
        self.pool.unpin(self.slot_id).await;
    }
}

impl Drop for KeptPage {
    fn drop(&mut self) {
        if self.done {
            return;
        }
        let (pool, id, conn) = (self.pool.clone(), self.slot_id, self.conn.clone());
        let mut res = std::mem::take(&mut self.res);
        if let Ok(rt) = tokio::runtime::Handle::try_current() {
            rt.spawn(async move {
                let _ = tokio::time::timeout(CLEANUP, res.cleanup(&conn)).await;
                pool.unpin(id).await;
            });
        }
    }
}
