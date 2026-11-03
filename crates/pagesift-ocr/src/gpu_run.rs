//! GPU 上跑检测和识别：页面线程把裁好的行图交到队列，调度线程拼批后交给 GPU 工人。
//!
//! 三个决定都不写死数字：
//! - **批多大**：一个样本的显存峰值由试算（[`pagesift_gpu::Ctx::dry`]，走真实的前向代码）得出，
//!   批大小 = 可用显存 / 单样本峰值；估多了分配会失败，那时对半切开重来。
//! - **哪些行拼一批**：宽度差得多就浪费算力，分开又多一次固定开销。批耗时按 `a + t·N·W` 在线回归，
//!   某行补边浪费的时间累计超过 `a` 就另起一批。
//! - **等不等下一批**：见 [`should_wait`]，用排队论的影子价格算。

use std::sync::mpsc::{Sender, channel};
use std::sync::{Arc, Condvar, Mutex, MutexGuard};
use std::thread::JoinHandle;
use std::time::{Duration, Instant};

use pagesift_gpu::{Ctx, Owner, SharedWorker, Worker, is_oom};
use pagesift_infer::Tensor;
use pagesift_infer::exec::Exec;
use pagesift_stat::decide::shadow_price;
use pagesift_stat::regress::RegressionSums;

use crate::det::Det;
use crate::rec::Rec;

/// [策略] 利用率和到达率的指数平滑系数：大约记住最近 10 个批次
const EWMA: f64 = 0.1;

/// CTC 摘要：每步的（最大类，其概率）。
pub(crate) type Digest = Vec<(u32, f32)>;

/// 显存和批次的统计（给评测和报告用）。
#[derive(Debug, Clone, Copy, Default, PartialEq)]
pub struct GpuStats {
    /// 占用显存的峰值（缓冲池 + 权重，字节）。
    pub peak_bytes: u64,
    /// 真正需要的显存峰值（在用的张量 + 权重，字节），比 `peak_bytes` 少池里闲着的部分。
    pub needed_bytes: u64,
    /// 跑过的识别批次数和行数，行数 / 批次数就是平均批大小。
    pub batches: u64,
    pub lines: u64,
    /// 显存不够、对半切开重来的次数。
    pub splits: u64,
}

/// 队列里的一行。
struct Item {
    x: Tensor,
    idx: usize,
    reply: Sender<(usize, Result<Digest, String>)>,
}

impl Item {
    fn width(&self) -> usize {
        self.x.shape[2]
    }
}

struct State {
    pending: Vec<Item>,
    stop: bool,
    /// 正在 [`Engine::run`](crate::pipeline::Engine::run) 里的页数，和其中已交完行、在等结果的页数。
    pages: usize,
    pages_waiting: usize,
    /// 到达率（行/秒）的平滑值；还没有估计时为 0。
    arrival_rate: f64,
    last_arrival: Option<Instant>,
    stats: GpuStats,
}

struct Shared {
    state: Mutex<State>,
    wake: Condvar,
}

impl Shared {
    fn lock(&self) -> MutexGuard<'_, State> {
        // 持锁时不做可能 panic 的工作；真被毒化了状态也还一致
        self.state.lock().unwrap_or_else(|e| e.into_inner())
    }
}

/// 现在开跑还是再等一个到达。
///
/// 开跑一批要付一次固定开销 `a`（秒）。再等一行并进来，这行就不用单独分摊那份开销：平均省
/// `a/(n+1)` 的 GPU 时间；省下的 GPU 时间按影子价格 `W/(1−ρ)` 折成所有排队者少等的时间
/// （`pagesift_stat::decide::shadow_price`，M/M/1 的逗留时间）。代价是已在队列里的 `n` 行每行多等
/// 一个到达间隔 `1/λ`。收益大于代价才等。
///
/// `producers` 是还会交新行的页数：都在等结果时等也等不来，立刻开跑。
pub(crate) fn should_wait(
    n: usize,
    n_max: usize,
    producers: usize,
    arrival_rate: f64,
    utilization: f64,
    overhead: f64,
) -> Option<Duration> {
    if n >= n_max || producers == 0 || arrival_rate <= 0.0 || overhead <= 0.0 {
        return None;
    }
    let gap = 1.0 / arrival_rate;
    let hold_cost = n as f64 * gap;
    let saving = shadow_price(utilization, overhead / (n + 1) as f64);
    (hold_cost < saving).then(|| Duration::from_secs_f64(gap))
}

/// 从队列里挑一批：从最宽的开始往窄走，补边浪费的时间累计超过一次固定开销就停，最多 `n_max` 行。
/// `model` 还没拟合好时（返回 None）就只受 `n_max` 限制。
pub(crate) fn batch_len(widths_desc: &[usize], n_max: usize, model: Option<(f64, f64)>) -> usize {
    let limit = widths_desc.len().min(n_max).max(1);
    let Some((overhead, per_col)) = model else {
        return limit;
    };
    let wmax = widths_desc[0];
    let mut waste = 0.0;
    for (i, &w) in widths_desc.iter().enumerate().take(limit).skip(1) {
        waste += (wmax - w) as f64 * per_col;
        if waste > overhead {
            return i;
        }
    }
    limit
}

/// 批耗时模型 `a + t·(N·W)` 的在线回归。
struct BatchModel {
    sums: RegressionSums,
}

impl BatchModel {
    fn new() -> BatchModel {
        BatchModel {
            sums: RegressionSums::new(2),
        }
    }

    fn observe(&mut self, area: usize, seconds: f64) {
        self.sums.push(&[1.0, area as f64], seconds);
    }

    /// (固定开销 a, 每列时间 t)；数据不够、回归不合理（病态或出负值）时为 None。
    fn params(&self) -> Option<(f64, f64)> {
        let f = self.sums.fit()?;
        let (a, t) = (f.beta[0], f.beta[1]);
        (a > 0.0 && t > 0.0).then_some((a, t))
    }
}

/// 把 GPU 工人、模型和调度线程捆在一起。
pub(crate) struct GpuRunner {
    worker: SharedWorker,
    det: Arc<Det>,
    shared: Arc<Shared>,
    thread: Option<JoinHandle<()>>,
}

/// 页面在 `run` 里时持有。
pub(crate) struct PageGuard<'a>(&'a Shared);

impl Drop for PageGuard<'_> {
    fn drop(&mut self) {
        self.0.lock().pages -= 1;
        self.0.wake.notify_all();
    }
}

impl GpuRunner {
    /// 启动工人（打开设备、JIT 内核）和调度线程。打不开时返回原因。
    pub fn start(index: usize, det: Arc<Det>, rec: Arc<Rec>) -> Result<GpuRunner, String> {
        let worker = Arc::new(Worker::spawn(index)?);
        let shared = Arc::new(Shared {
            state: Mutex::new(State {
                pending: Vec::new(),
                stop: false,
                pages: 0,
                pages_waiting: 0,
                arrival_rate: 0.0,
                last_arrival: None,
                stats: GpuStats::default(),
            }),
            wake: Condvar::new(),
        });
        let thread = {
            let (worker, shared) = (worker.clone(), shared.clone());
            std::thread::Builder::new()
                .name("pagesift-ocr-batch".into())
                .spawn(move || scheduler(&worker, &shared, &rec))
                .map_err(|e| format!("failed to start the OCR batch thread: {e}"))?
        };
        Ok(GpuRunner {
            worker,
            det,
            shared,
            thread: Some(thread),
        })
    }

    pub fn worker(&self) -> &SharedWorker {
        &self.worker
    }

    pub fn page(&self) -> PageGuard<'_> {
        self.shared.lock().pages += 1;
        PageGuard(&self.shared)
    }

    pub fn stats(&self) -> GpuStats {
        let mut s = self.shared.lock().stats;
        // 工人线程查不到就保留 0
        (s.peak_bytes, s.needed_bytes) = self
            .worker
            .call(|ctx: &Ctx<'_>| (ctx.peak_bytes(), ctx.needed_bytes()))
            .unwrap_or((0, 0));
        s
    }

    /// 检测网络：`[3, H, W]` → 概率图 (数据, 宽, 高)。
    pub fn prob_map(&self, x: &Tensor) -> Result<(Vec<f32>, usize, usize), String> {
        let det = self.det.clone();
        self.worker.call(move |ctx: &Ctx<'_>| {
            let owner: Owner = det.clone();
            let e = ctx.exec(1, owner);
            let dx = e.upload(x);
            let prob = det.prob_on(&e, &dx)?;
            let shape = e.shape(&prob);
            let data = e.download(&prob)?;
            Ok((data, shape[2], shape[1]))
        })?
    }

    /// 识别网络：一组 `[3, 48, W]`，返回各行的摘要，顺序同输入。出错的行各自带原因。
    pub fn recognize_many(&self, xs: Vec<Tensor>) -> Vec<Result<Digest, String>> {
        let n = xs.len();
        let (tx, rx) = channel();
        {
            let mut st = self.shared.lock();
            let now = Instant::now();
            if let Some(last) = st.last_arrival {
                let dt = now.duration_since(last).as_secs_f64();
                if dt > 0.0 {
                    st.arrival_rate = ewma(st.arrival_rate, n as f64 / dt);
                }
            }
            st.last_arrival = Some(now);
            st.pages_waiting += 1;
            st.pending
                .extend(xs.into_iter().enumerate().map(|(idx, x)| Item {
                    x,
                    idx,
                    reply: tx.clone(),
                }));
        }
        drop(tx);
        self.shared.wake.notify_all();
        let mut out: Vec<Result<Digest, String>> = (0..n)
            .map(|_| Err("the OCR batch thread exited".to_owned()))
            .collect();
        for (i, r) in rx {
            out[i] = r;
        }
        self.shared.lock().pages_waiting -= 1;
        self.shared.wake.notify_all();
        out
    }
}

impl Drop for GpuRunner {
    fn drop(&mut self) {
        self.shared.lock().stop = true;
        self.shared.wake.notify_all();
        if let Some(t) = self.thread.take() {
            // 线程 panic 也不再往外传：Drop 里没有地方报
            let _ = t.join();
        }
    }
}

fn ewma(old: f64, new: f64) -> f64 {
    if old == 0.0 {
        new
    } else {
        old + EWMA * (new - old)
    }
}

/// 宽度为 `w` 的一行能拼多大的批：可用显存 / 单行的工作内存（试算得出）。
fn plan(worker: &Worker, rec: &Arc<Rec>, w: usize) -> Result<usize, String> {
    worker.call(|ctx: &Ctx<'_>| {
        let owner: Owner = rec.clone();
        let e = ctx.dry(1, owner);
        let x = e.placeholder(vec![rec.input_channels(), rec.input_height(), w]);
        let logits = rec.logits_on(&e, &x);
        e.ctc_digest(&logits);
        let (work, weights) = e.dry_usage();
        let avail = ctx.available_bytes()?.saturating_sub(weights);
        Ok(((avail / work.max(1)) as usize).max(1))
    })?
}

/// 真跑一批。
fn run_batch(worker: &Worker, rec: &Arc<Rec>, items: &[Item]) -> Result<Vec<Digest>, String> {
    let xs: Vec<&Tensor> = items.iter().map(|i| &i.x).collect();
    worker.call(|ctx: &Ctx<'_>| {
        let owner: Owner = rec.clone();
        let e = ctx.exec(xs.len(), owner);
        let x = e.upload_batch(&xs);
        let logits = rec.logits_on(&e, &x);
        let rows = e.ctc_digest(&logits);
        e.finish()?;
        Ok(rows)
    })?
}

/// 跑一批并回复；显存不够就对半切开。返回用掉的时间，切开过时不计入模型（返回 None）。
fn run_and_reply(
    worker: &Worker,
    rec: &Arc<Rec>,
    shared: &Shared,
    mut items: Vec<Item>,
) -> Option<f64> {
    let t = Instant::now();
    match run_batch(worker, rec, &items) {
        Ok(rows) => {
            let secs = t.elapsed().as_secs_f64();
            for (item, r) in items.iter().zip(rows) {
                // 回复通道断开说明那一页已经不要了
                let _ = item.reply.send((item.idx, Ok(r)));
            }
            Some(secs)
        }
        Err(e) if is_oom(&e) && items.len() > 1 => {
            shared.lock().stats.splits += 1;
            let tail = items.split_off(items.len() / 2);
            run_and_reply(worker, rec, shared, items);
            run_and_reply(worker, rec, shared, tail);
            None
        }
        Err(e) => {
            for item in &items {
                let _ = item.reply.send((item.idx, Err(e.clone())));
            }
            None
        }
    }
}

/// 调度线程。
fn scheduler(worker: &Worker, shared: &Shared, rec: &Arc<Rec>) {
    let mut model = BatchModel::new();
    let started = Instant::now();
    let mut busy = 0f64;
    loop {
        let mut st = shared.lock();
        while st.pending.is_empty() {
            if st.stop {
                return;
            }
            st = shared.wake.wait(st).unwrap_or_else(|e| e.into_inner());
        }
        st.pending.sort_by_key(|i| std::cmp::Reverse(i.width()));
        let wmax = st.pending[0].width();
        drop(st);
        // 试算出错（工人线程没了）时按 1 行一批走下去，真跑时会把错误报给调用方
        let n_max = plan(worker, rec, wmax).unwrap_or(1);
        let mut st = shared.lock();
        let params = model.params();
        let producers = st.pages.saturating_sub(st.pages_waiting);
        let utilization = (busy / started.elapsed().as_secs_f64().max(f64::MIN_POSITIVE)).min(1.0);
        if !st.stop
            && let Some(dt) = should_wait(
                st.pending.len(),
                n_max,
                producers,
                st.arrival_rate,
                utilization,
                params.map_or(0.0, |p| p.0),
            )
        {
            // 超时或有新行都回到循环开头重新判断
            drop(
                shared
                    .wake
                    .wait_timeout(st, dt)
                    .unwrap_or_else(|e| e.into_inner()),
            );
            continue;
        }
        st.pending.sort_by_key(|i| std::cmp::Reverse(i.width()));
        let widths: Vec<usize> = st.pending.iter().map(Item::width).collect();
        let n = batch_len(&widths, n_max, params);
        let batch: Vec<Item> = st.pending.drain(..n).collect();
        st.stats.batches += 1;
        st.stats.lines += n as u64;
        drop(st);
        let area = n * batch[0].width();
        let t = Instant::now();
        let secs = run_and_reply(worker, rec, shared, batch);
        busy += t.elapsed().as_secs_f64();
        if let Some(s) = secs {
            model.observe(area, s);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn 没有生产者时不等() {
        assert_eq!(should_wait(3, 100, 0, 1e6, 0.5, 1.0), None);
    }

    #[test]
    fn 批满了不等() {
        assert_eq!(should_wait(100, 100, 2, 1e6, 0.5, 1.0), None);
    }

    #[test]
    fn 到达快且开销大时等_到达慢时不等() {
        // 1 行在队列、每毫秒来一行、固定开销 10 ms：多等 1 ms，省 5 ms
        assert!(should_wait(1, 100, 1, 1e3, 0.0, 0.01).is_some());
        // 每秒才来一行：多等 1 s，只省 5 ms
        assert!(should_wait(1, 100, 1, 1.0, 0.0, 0.01).is_none());
        // 忙的时候省下的 GPU 时间更值钱，同样的到达率也值得等
        assert!(should_wait(1, 100, 1, 50.0, 0.0, 0.01).is_none());
        assert!(should_wait(1, 100, 1, 50.0, 0.9, 0.01).is_some());
    }

    #[test]
    fn 按浪费切批() {
        // 没有模型时只受上限限制
        assert_eq!(batch_len(&[320, 320, 320], 2, None), 2);
        assert_eq!(batch_len(&[3200, 320], 8, None), 2);
        // 相同宽度没有浪费
        assert_eq!(batch_len(&[320, 320, 320], 8, Some((1e-3, 1e-6))), 3);
        // 宽差 2880 列 × 1 µs ≈ 2.9 ms，超过 1 ms 的固定开销，分开
        assert_eq!(batch_len(&[3200, 320], 8, Some((1e-3, 1e-6))), 1);
        // 累计浪费：每行差 400 列 0.4 ms，第 3 行起超过 1 ms
        assert_eq!(
            batch_len(&[800, 400, 400, 400, 400], 8, Some((1e-3, 1e-6))),
            3
        );
    }

    #[test]
    fn 回归拟合出固定开销和每列时间() {
        let mut m = BatchModel::new();
        assert_eq!(m.params(), None);
        for area in [320, 640, 3200, 6400] {
            m.observe(area, 0.01 + 1e-6 * area as f64);
        }
        let (a, t) = m.params().unwrap();
        assert!((a - 0.01).abs() < 1e-6, "{a}");
        assert!((t - 1e-6).abs() < 1e-9, "{t}");
    }
}
