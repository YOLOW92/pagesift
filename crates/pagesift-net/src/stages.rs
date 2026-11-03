//! 公网抓取各阶段的遥测：在途数、完成数、按结果分类的计数和耗时分位数。
//!
//! 0.84 起各阶段不再排队（并发由礼貌限速、全局并发和 CPU 工人数决定，即统一准入），
//! 0.86 删掉了只为排队服务的 AIMD 上限、观测窗口和固定样本数，只留遥测。
//!
//! 并发（192 vCPU 实测经验）：每个请求都会进出几个阶段，计数不能落在一把全局锁或同一个原子量上。
//! 计数按线程分片（分片数随核数），读快照时汇总；耗时分位数用 DDSketch（可合并，内存有界）。

use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use pagesift_stat::Stat;
use pagesift_stat::sketch::Sketch;

/// [推导] 阶段数，等于 [`NetworkStage`] 的变体数
const STAGE_COUNT: usize = 4;
/// [推导] 传输阶段数，等于 [`TransportPhase`] 的变体数
const PHASE_COUNT: usize = 5;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
#[repr(usize)]
pub enum NetworkStage {
    Dns,
    Handshake,
    Http,
    Parse,
}

impl NetworkStage {
    pub const ALL: [Self; STAGE_COUNT] = [Self::Dns, Self::Handshake, Self::Http, Self::Parse];
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum StageOutcome {
    Success,
    /// DNS/network/timeout failures that may improve after reducing pressure.
    TransientFailure,
    /// HTTP 429 or another direct remote-overload signal.
    Overload,
    /// Certificate, protocol, malformed-input and other repeatable failures.
    DeterministicFailure,
    /// robots, SSRF and access-policy rejection.
    PolicyRejected,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
#[repr(usize)]
pub enum TransportPhase {
    Dns,
    Tcp,
    Tls,
    Ttfb,
    Body,
}

impl TransportPhase {
    pub const ALL: [Self; PHASE_COUNT] = [Self::Dns, Self::Tcp, Self::Tls, Self::Ttfb, Self::Body];
}

#[derive(Debug, Clone, PartialEq)]
pub struct StageSnapshot {
    pub stage: NetworkStage,
    /// 已经没有准入上限；保留这个字段给旧的报表，值等于 `active`。
    pub limit: usize,
    pub active: usize,
    pub completed: u64,
    pub transient_failures: u64,
    pub overloads: u64,
    pub deterministic_failures: u64,
    pub policy_rejections: u64,
    pub cancelled: u64,
    pub p50: Duration,
    pub p95: Duration,
    pub p99: Duration,
}

#[derive(Debug, Clone, PartialEq)]
pub struct PhaseSnapshot {
    pub phase: TransportPhase,
    pub samples: u64,
    pub transient_failures: u64,
    pub overloads: u64,
    pub deterministic_failures: u64,
    pub policy_rejections: u64,
    pub p50: Duration,
    pub p95: Duration,
    pub p99: Duration,
}

/// 一类计数（阶段或传输阶段）。
#[derive(Debug, Clone, Default)]
struct Cell {
    /// 进入减离开；任务可能在别的线程上结束，所以单个分片可以为负，汇总后才有意义。
    active: i64,
    completed: u64,
    transient_failures: u64,
    overloads: u64,
    deterministic_failures: u64,
    policy_rejections: u64,
    cancelled: u64,
    latency: Sketch,
}

impl Cell {
    fn record(&mut self, elapsed: Duration, outcome: StageOutcome) {
        self.completed += 1;
        match outcome {
            StageOutcome::Success => {}
            StageOutcome::TransientFailure => self.transient_failures += 1,
            StageOutcome::Overload => self.overloads += 1,
            StageOutcome::DeterministicFailure => self.deterministic_failures += 1,
            StageOutcome::PolicyRejected => self.policy_rejections += 1,
        }
        self.latency.push(elapsed.as_secs_f64());
    }

    fn add(&mut self, o: &Self) {
        self.active += o.active;
        self.completed += o.completed;
        self.transient_failures += o.transient_failures;
        self.overloads += o.overloads;
        self.deterministic_failures += o.deterministic_failures;
        self.policy_rejections += o.policy_rejections;
        self.cancelled += o.cancelled;
        self.latency.merge(&o.latency);
    }

    /// 遥测报的固定三个分位点（中位、p95、p99），是展示口径，不参与决策。
    fn quantiles(&self) -> (Duration, Duration, Duration) {
        if self.latency.count() == 0 {
            return (Duration::ZERO, Duration::ZERO, Duration::ZERO);
        }
        let q = |p: f64| Duration::from_secs_f64(self.latency.quantile(p).max(0.0));
        // [策略] 报表口径：p50 / p95 / p99（与 0.81 的输出字段一致）
        (q(0.5), q(0.95), q(0.99))
    }
}

#[derive(Debug, Default)]
struct Shard {
    stages: [Cell; STAGE_COUNT],
    phases: [Cell; PHASE_COUNT],
}

/// 独占缓存行（[协议] 128 字节：相邻缓存行成对预取，按两行对齐）。
#[repr(align(128))]
#[derive(Debug, Default)]
struct Padded(Mutex<Shard>);

#[derive(Debug, Clone)]
pub struct StageController {
    shards: Arc<[Padded]>,
}

#[derive(Debug)]
pub struct StagePermit {
    controller: StageController,
    stage: NetworkStage,
    started: Instant,
    finished: bool,
}

/// 线程到分片的映射：每个线程第一次用时按顺序领一个号。
fn shard_index(len: usize) -> usize {
    static NEXT: AtomicUsize = AtomicUsize::new(0);
    thread_local! {
        static SLOT: usize = NEXT.fetch_add(1, Ordering::Relaxed);
    }
    SLOT.with(|s| *s) % len
}

impl StageController {
    pub fn new() -> Self {
        // [推导] 分片数随核数：取不小于逻辑核数的 2 的幂，每个线程基本独占一个分片
        let n = std::thread::available_parallelism()
            .map_or(1, |n| n.get())
            .next_power_of_two();
        Self {
            shards: (0..n).map(|_| Padded::default()).collect(),
        }
    }

    fn with<R>(&self, f: impl FnOnce(&mut Shard) -> R) -> R {
        let shard = &self.shards[shard_index(self.shards.len())].0;
        let mut s = shard.lock().unwrap_or_else(|e| e.into_inner());
        f(&mut s)
    }

    fn total(&self) -> Shard {
        let mut all = Shard::default();
        for s in self.shards.iter() {
            let s = s.0.lock().unwrap_or_else(|e| e.into_inner());
            for (a, b) in all.stages.iter_mut().zip(s.stages.iter()) {
                a.add(b);
            }
            for (a, b) in all.phases.iter_mut().zip(s.phases.iter()) {
                a.add(b);
            }
        }
        all
    }

    /// 进入一个阶段（只记遥测，不排队）。
    pub async fn acquire(&self, stage: NetworkStage) -> StagePermit {
        self.with(|s| s.stages[stage as usize].active += 1);
        StagePermit {
            controller: self.clone(),
            stage,
            started: Instant::now(),
            finished: false,
        }
    }

    pub fn snapshots(&self) -> Vec<StageSnapshot> {
        let all = self.total();
        NetworkStage::ALL
            .into_iter()
            .map(|stage| snapshot(stage, &all.stages[stage as usize]))
            .collect()
    }

    pub fn snapshot(&self, stage: NetworkStage) -> StageSnapshot {
        snapshot(stage, &self.total().stages[stage as usize])
    }

    pub fn phase_snapshots(&self) -> Vec<PhaseSnapshot> {
        let all = self.total();
        TransportPhase::ALL
            .into_iter()
            .map(|phase| {
                let c = &all.phases[phase as usize];
                let (p50, p95, p99) = c.quantiles();
                PhaseSnapshot {
                    phase,
                    samples: c.completed,
                    transient_failures: c.transient_failures,
                    overloads: c.overloads,
                    deterministic_failures: c.deterministic_failures,
                    policy_rejections: c.policy_rejections,
                    p50,
                    p95,
                    p99,
                }
            })
            .collect()
    }

    pub fn record_phase(&self, phase: TransportPhase, elapsed: Duration, outcome: StageOutcome) {
        self.with(|s| s.phases[phase as usize].record(elapsed, outcome));
    }

    fn finish(
        &self,
        stage: NetworkStage,
        elapsed: Duration,
        outcome: StageOutcome,
        cancelled: bool,
    ) {
        self.with(|s| {
            if stage == NetworkStage::Dns {
                s.phases[TransportPhase::Dns as usize].record(elapsed, outcome);
            }
            let c = &mut s.stages[stage as usize];
            c.active -= 1;
            c.cancelled += u64::from(cancelled);
            c.record(elapsed, outcome);
        });
    }
}

fn snapshot(stage: NetworkStage, c: &Cell) -> StageSnapshot {
    let (p50, p95, p99) = c.quantiles();
    let active = c.active.max(0) as usize;
    StageSnapshot {
        stage,
        limit: active,
        active,
        completed: c.completed,
        transient_failures: c.transient_failures,
        overloads: c.overloads,
        deterministic_failures: c.deterministic_failures,
        policy_rejections: c.policy_rejections,
        cancelled: c.cancelled,
        p50,
        p95,
        p99,
    }
}

impl Default for StageController {
    fn default() -> Self {
        Self::new()
    }
}

impl StagePermit {
    pub fn finish(mut self, outcome: StageOutcome) {
        self.finished = true;
        self.controller
            .finish(self.stage, self.started.elapsed(), outcome, false);
    }
}

impl Drop for StagePermit {
    fn drop(&mut self) {
        if !self.finished {
            self.finished = true;
            self.controller.finish(
                self.stage,
                self.started.elapsed(),
                StageOutcome::TransientFailure,
                true,
            );
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn counts_outcomes_per_stage_across_threads() {
        let controller = StageController::new();
        let mut handles = Vec::new();
        for i in 0..8 {
            let c = controller.clone();
            handles.push(std::thread::spawn(move || {
                let rt = tokio::runtime::Builder::new_current_thread()
                    .build()
                    .unwrap();
                rt.block_on(async {
                    for _ in 0..100 {
                        let outcome = if i % 2 == 0 {
                            StageOutcome::Success
                        } else {
                            StageOutcome::PolicyRejected
                        };
                        c.acquire(NetworkStage::Http).await.finish(outcome);
                    }
                });
            }));
        }
        let held = controller.acquire(NetworkStage::Dns).await;
        for h in handles {
            h.join().unwrap();
        }
        let http = controller.snapshot(NetworkStage::Http);
        assert_eq!(http.completed, 800);
        assert_eq!(http.policy_rejections, 400);
        assert_eq!(http.active, 0);
        assert_eq!(controller.snapshot(NetworkStage::Dns).active, 1);
        drop(held);
    }

    #[tokio::test]
    async fn cancellation_is_counted_and_is_not_fake_success() {
        let controller = StageController::new();
        let permit = controller.acquire(NetworkStage::Parse).await;
        drop(permit);
        let snapshot = controller.snapshot(NetworkStage::Parse);
        assert_eq!(snapshot.active, 0);
        assert_eq!(snapshot.completed, 1);
        assert_eq!(snapshot.cancelled, 1);
        assert_eq!(snapshot.transient_failures, 1);
    }

    #[test]
    fn phase_quantiles_come_from_the_sketch() {
        let controller = StageController::new();
        for ms in 1..=100 {
            controller.record_phase(
                TransportPhase::Ttfb,
                Duration::from_millis(ms),
                StageOutcome::Success,
            );
        }
        let p = controller
            .phase_snapshots()
            .into_iter()
            .find(|p| p.phase == TransportPhase::Ttfb)
            .unwrap();
        assert_eq!(p.samples, 100);
        assert!(p.p50 >= Duration::from_millis(48) && p.p50 <= Duration::from_millis(52));
        assert!(p.p99 >= Duration::from_millis(97));
    }
}
