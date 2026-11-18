//! 进程级的 CPU 工人池（`pagesift_flow::Pool`）：正文提取、PDF/Office 解析、解压转码、缓存编解码
//! 都在这里执行，不占异步线程。同时在跑的任务数等于工人数。

use super::*;

static CPU_CAPACITY: std::sync::OnceLock<usize> = std::sync::OnceLock::new();
static CPU_GROUPS: std::sync::OnceLock<Vec<pagesift_flow::CpuSet>> = std::sync::OnceLock::new();

/// 进程可用的 CPU 数，由建运行时的一方在**绑核之前**设置。
///
/// `available_parallelism()` 在 Linux 上返回调用线程的亲和集。工人池第一次被用到时
/// 已经在 worker 线程里，而 CLI 的 worker 会被绑到单个 CPU：0.70–0.72 因此只有 1 个许可，
/// 所有正文提取串行。没有设置时（Python/Node 绑定等不绑核的宿主）仍按调用线程计算。
pub fn set_cpu_capacity(cpus: usize) {
    let _ = CPU_CAPACITY.set(cpus.max(1));
}

/// CPU 工人的分组：每个 NUMA 节点允许使用的逻辑 CPU。设置后工人按组分配、绑到所在组的 CPU 集，
/// 偷任务先在组内偷（0.70 在 192 vCPU 上的实测，见 `pagesift_flow::cpu`）。
/// 不设置时（Python、Node 宿主）工人不绑核。
pub fn set_cpu_groups(groups: Vec<pagesift_flow::CpuSet>) {
    let _ = CPU_GROUPS.set(groups);
}

thread_local! {
    /// 每个工人一份提取工作区（DOM 和索引的缓冲）：工人常驻，缓冲一直是热的，
    /// 也不会在别的线程的 malloc arena 上释放。
    pub(super) static CONTENT_WORKSPACE: std::cell::RefCell<pagesift_content::ProcessWorkspace> =
        std::cell::RefCell::new(pagesift_content::ProcessWorkspace::default());
    /// 单次辅助 DOM 查询（CSS 自适应抽取、标签筛选、iframe 扫描、find_similar）复用的解析缓冲。
    pub(super) static DOM_SCRATCH: std::cell::RefCell<pagesift_html::ParseScratch> =
        std::cell::RefCell::new(pagesift_html::ParseScratch::default());
}

/// 在当前线程复用 [`DOM_SCRATCH`] 解析 `html` 并执行 `f`，结束后立即回收节点与属性缓冲。
pub(super) fn with_dom<R>(html: &str, f: impl FnOnce(&pagesift_html::Html<'_>) -> R) -> R {
    DOM_SCRATCH.with(|cell| match cell.try_borrow_mut() {
        Ok(mut scratch) => pagesift_content::with_parsed_dom(html, &mut scratch, f),
        Err(_) => {
            let dom = pagesift_html::Html::parse(html);
            f(&dom)
        }
    })
}

pub fn pool() -> &'static pagesift_flow::Pool {
    static POOL: std::sync::LazyLock<pagesift_flow::Pool> = std::sync::LazyLock::new(|| {
        let n = CPU_CAPACITY
            .get()
            .copied()
            .unwrap_or_else(|| std::thread::available_parallelism().map_or(1, |n| n.get()));
        let groups = CPU_GROUPS.get().map_or(&[][..], Vec::as_slice);
        // 只有一个节点时绑与不绑相同，省掉系统调用
        let pin = (groups.len() > 1).then_some(
            pagesift_policy::topology::pin_current_thread_to_set as fn(&[usize]) -> bool,
        );
        pagesift_flow::Pool::with_groups(n, groups, pin).expect("starting CPU workers")
    });
    &POOL
}

/// 起工人池，并把网络层的解压接到工人上。可重复调用。
pub(super) fn install() {
    static HOOK: std::sync::Once = std::sync::Once::new();
    HOOK.call_once(|| {
        pool();
        pagesift_net::set_cpu_executor(|job| pool().spawn(job), || pool().round_trip());
    });
}

pub(super) async fn cpu_task<T: Send + 'static>(
    f: impl FnOnce() -> T + Send + 'static,
) -> Result<T, ApiError> {
    pool()
        .run(None, f)
        .await
        .map_err(|e| ApiError::new(ErrorCode::Internal, e.to_string()))
}

pub(crate) async fn cpu_task_keyed<T: Send + 'static>(
    key: u64,
    f: impl FnOnce() -> T + Send + 'static,
) -> Result<T, ApiError> {
    pool()
        .run(Some(key), f)
        .await
        .map_err(|e| ApiError::new(ErrorCode::Internal, e.to_string()))
}
