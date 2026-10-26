use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use tokio::sync::Notify;

const WINDOW: u32 = 16;
const EWMA_ALPHA: f64 = 0.20;
/// 估计“慢”用最近这么多次完成的服务时间。
const SAMPLES: usize = 32;
/// 攒够这么多样本才按源站自己的服务时间判断“慢”，之前用调用方给的上限（尾部阈值）。
const MIN_SAMPLES: usize = 8;
/// 比源站服务时间中位数长这么多倍算慢（中位数不被少数慢请求带偏，慢请求占多数时阈值才跟着变大）。
const SLOW_FACTOR: f64 = 4.0;
/// “慢”的阈值下限：更短时让出名额换来的并发不值得，本机回环的样本也主要是调度噪声。
const SLOW_FLOOR: Duration = Duration::from_millis(250);

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum HostOutcome {
    Success,
    Failure,
    Overload,
    Neutral,
}

/// Runtime state of one origin's self-tuning connection controller.
#[derive(Debug, Clone, PartialEq)]
pub struct HostConnectionStats {
    pub origin: String,
    pub hot_target: usize,
    pub active: usize,
    pub max_hot: usize,
    /// 等得太久、已让出热连接名额的请求数（见 [`HostPermit::park`]）。
    pub parked: usize,
    pub completed: u64,
    pub failed: u64,
    pub overloaded: u64,
    pub cancelled: u64,
    pub reused: u64,
    pub connected: u64,
    pub service_ewma: Duration,
    pub connect_ewma: Duration,
}

#[derive(Debug)]
struct State {
    adaptive: bool,
    target: usize,
    max: usize,
    active: usize,
    parked: usize,
    completed: u64,
    failed: u64,
    overloaded: u64,
    cancelled: u64,
    reused: u64,
    connected: u64,
    service_ewma_us: f64,
    connect_ewma_us: f64,
    best_service_us: f64,
    window_samples: u32,
    window_failures: u32,
    window_saturated: bool,
    cooldown: u32,
    /// 最近完成的服务时间（微秒），环形缓冲。
    samples: Vec<u64>,
    next_sample: usize,
    /// 按 `samples` 算出的“慢”阈值（微秒）；样本不够时为 0，用调用方的上限。
    slow_after_us: u64,
    /// 上一次有请求及时结束（没超过“慢”阈值）的时刻：排队的请求只有在这么久没有任何进展时
    /// 才算“前面都是慢请求”。
    last_progress: tokio::time::Instant,
    /// 延迟随负载上升（被压垮）：停放名额收紧到热连接目标，免得停放的请求让并发继续升到上限。
    /// 只看延迟相对这个源站自己基线的变化，不看绝对值：一直很慢但和并发无关的源站不收紧。
    slow_limited: bool,
}

impl State {
    /// 这个源站上多久算慢：最近服务时间中位数的 [`SLOW_FACTOR`] 倍，夹在 [`SLOW_FLOOR`] 和 `cap` 之间。
    fn slow_after(&self, cap: Duration) -> Duration {
        if self.slow_after_us == 0 {
            return cap;
        }
        Duration::from_micros(self.slow_after_us).clamp(SLOW_FLOOR.min(cap), cap)
    }

    /// 还能停放几个：平时只受安全上限约束；源站持续慢时不超过热连接目标。
    fn may_park(&self) -> bool {
        let below_max = self.active + self.parked < self.max;
        below_max && (!self.slow_limited || self.parked < self.target)
    }

    fn record_sample(&mut self, elapsed_us: f64) {
        let us = elapsed_us.max(0.0) as u64;
        if self.samples.len() < SAMPLES {
            self.samples.push(us);
        } else {
            self.samples[self.next_sample] = us;
        }
        self.next_sample = (self.next_sample + 1) % SAMPLES;
        if self.samples.len() >= MIN_SAMPLES {
            let mut sorted = self.samples.clone();
            let mid = sorted.len() / 2;
            let (_, median, _) = sorted.select_nth_unstable(mid);
            self.slow_after_us = ((*median as f64) * SLOW_FACTOR).max(1.0) as u64;
        }
    }
}

/// A cancellation-safe, per-origin AIMD controller. It deliberately has no
/// background scanner: only completed requests change the watermark.
pub(crate) struct HostConnectionControl {
    state: Mutex<State>,
    changed: Notify,
}

pub(crate) struct HostPermit {
    control: Arc<HostConnectionControl>,
    started: Instant,
    /// 拿到名额时这个源站的“慢”阈值：请求从拿到名额起超过这么久就停放（见 [`HostPermit::park`]）。
    slow_after: Duration,
    reused: bool,
    connect_time: Option<Duration>,
    finished: bool,
    parked: bool,
}

impl HostConnectionControl {
    pub(crate) fn new(adaptive: bool, max: usize) -> Arc<Self> {
        let max = max.max(1);
        Arc::new(Self {
            state: Mutex::new(State {
                adaptive,
                target: if adaptive { max.min(2) } else { max },
                max,
                active: 0,
                parked: 0,
                completed: 0,
                failed: 0,
                overloaded: 0,
                cancelled: 0,
                reused: 0,
                connected: 0,
                service_ewma_us: 0.0,
                connect_ewma_us: 0.0,
                best_service_us: 0.0,
                window_samples: 0,
                window_failures: 0,
                window_saturated: false,
                cooldown: 0,
                samples: Vec::with_capacity(SAMPLES),
                next_sample: 0,
                slow_after_us: 0,
                last_progress: tokio::time::Instant::now(),
                slow_limited: false,
            }),
            changed: Notify::new(),
        })
    }

    #[cfg(test)]
    pub(crate) async fn acquire(self: &Arc<Self>) -> HostPermit {
        self.acquire_within(Duration::from_secs(3600)).await
    }

    /// 拿一个名额。`cap` 是“慢”阈值的上限（尾部阈值），实际阈值随这个源站的服务时间自适应
    /// （[`State::slow_after`]）。热连接目标满了就排队；排队期间这个源站超过一个“慢”阈值没有
    /// 任何请求及时结束，说明占着名额的都是慢请求，这时以停放身份放行（受停放名额约束）：否则
    /// 慢请求一代一代地释放名额，排在 N 个慢请求后面的快请求要等 N/目标 个阈值。有请求陆续及时
    /// 结束的长队列（源站健康、只是排得长）不会因此放行，并发仍由热连接目标决定。
    pub(crate) async fn acquire_within(self: &Arc<Self>, cap: Duration) -> HostPermit {
        let queued_at = tokio::time::Instant::now();
        loop {
            let notified = self.changed.notified();
            let impatient_at = {
                let mut state = self.state.lock().expect("host connection lock poisoned");
                let now = tokio::time::Instant::now();
                let slow_after = state.slow_after(cap);
                let impatient_at = queued_at.max(state.last_progress).checked_add(slow_after);
                let impatient = impatient_at.is_some_and(|t| now >= t);
                let hot = state.active < state.target && state.active + state.parked < state.max;
                // 停放的请求不占热连接名额，但仍算在安全上限里
                if hot || impatient && state.may_park() {
                    if hot {
                        state.active += 1;
                        if state.active == state.target {
                            state.window_saturated = true;
                        }
                    } else {
                        state.parked += 1;
                    }
                    return HostPermit {
                        control: self.clone(),
                        started: Instant::now(),
                        slow_after,
                        reused: false,
                        connect_time: None,
                        finished: false,
                        parked: !hot,
                    };
                }
                impatient_at
            };
            match impatient_at {
                Some(t) if tokio::time::Instant::now() < t => {
                    tokio::select! {
                        _ = notified => {}
                        _ = tokio::time::sleep_until(t) => {}
                    }
                }
                _ => notified.await,
            }
        }
    }

    pub(crate) fn target(&self) -> usize {
        self.state
            .lock()
            .expect("host connection lock poisoned")
            .target
    }

    pub(crate) fn stats(&self, origin: String) -> HostConnectionStats {
        let state = self.state.lock().expect("host connection lock poisoned");
        HostConnectionStats {
            origin,
            hot_target: state.target,
            active: state.active,
            max_hot: state.max,
            parked: state.parked,
            completed: state.completed,
            failed: state.failed,
            overloaded: state.overloaded,
            cancelled: state.cancelled,
            reused: state.reused,
            connected: state.connected,
            service_ewma: micros(state.service_ewma_us),
            connect_ewma: micros(state.connect_ewma_us),
        }
    }

    /// 把一个占着热连接名额的请求转为停放，空出的名额立刻给排队的请求。源站持续慢、停放名额
    /// 用完时不停放（返回 false），请求继续占着热连接名额。
    fn park(&self) -> bool {
        let mut state = self.state.lock().expect("host connection lock poisoned");
        // 自己从热连接转出来，安全上限的占用不变
        state.active = state.active.saturating_sub(1);
        if !state.may_park() {
            state.active += 1;
            return false;
        }
        state.parked += 1;
        drop(state);
        self.changed.notify_waiters();
        true
    }

    #[allow(clippy::too_many_arguments)]
    fn complete(
        &self,
        outcome: HostOutcome,
        elapsed: Duration,
        reused: bool,
        connect_time: Option<Duration>,
        cancelled: bool,
        parked: bool,
        slow: bool,
    ) {
        let mut state = self.state.lock().expect("host connection lock poisoned");
        if parked {
            state.parked = state.parked.saturating_sub(1);
        } else {
            state.active = state.active.saturating_sub(1);
        }
        // 没超过“慢”阈值就结束的请求才算进展：慢请求结束不能让排队者重新计时，
        // 否则一直很慢的源站上排队的请求迟迟放不进来
        if !slow {
            state.last_progress = tokio::time::Instant::now();
        }
        if outcome == HostOutcome::Neutral {
            drop(state);
            self.changed.notify_waiters();
            return;
        }

        state.completed += 1;
        state.window_samples += 1;
        if reused {
            state.reused += 1;
        } else {
            state.connected += 1;
        }
        if cancelled {
            state.cancelled += 1;
        }
        let elapsed_us = elapsed.as_secs_f64() * 1_000_000.0;
        state.service_ewma_us = ewma(state.service_ewma_us, elapsed_us);
        if !cancelled {
            state.record_sample(elapsed_us);
        }
        if let Some(duration) = connect_time {
            state.connect_ewma_us =
                ewma(state.connect_ewma_us, duration.as_secs_f64() * 1_000_000.0);
        }

        match outcome {
            HostOutcome::Success => {}
            HostOutcome::Failure => {
                state.failed += 1;
                state.window_failures += 1;
            }
            HostOutcome::Overload => {
                state.failed += 1;
                state.overloaded += 1;
                state.window_failures += 1;
                if state.adaptive && state.cooldown == 0 {
                    // 429/503 are direct overload signals and should back off now,
                    // without waiting for a whole observation window. Multiple
                    // responses from the same overshoot episode must not stack.
                    state.target = state.target.saturating_sub(1).max(1);
                    state.cooldown = WINDOW;
                    reset_window(&mut state);
                }
            }
            HostOutcome::Neutral => unreachable!(),
        }

        if state.adaptive && state.window_samples >= WINDOW {
            if state.cooldown > 0 {
                state.cooldown = state.cooldown.saturating_sub(WINDOW);
                reset_window(&mut state);
                drop(state);
                self.changed.notify_waiters();
                return;
            }
            let error_rate = state.window_failures as f64 / state.window_samples as f64;
            // Sub-millisecond loopback samples are dominated by scheduler and
            // clock noise. Use service-time regression only once the signal is
            // large enough to be meaningful.
            let latency_regressed = state.best_service_us >= 1_000.0
                && state.service_ewma_us > state.best_service_us * 1.50;
            // 延迟随负载上升才算过载（同 TCP Vegas、Scrapy AutoThrottle）：基线是这个源站
            // 无错窗口里服务时间的最低值。过载时连停放名额一起收紧，否则停放的请求绕过热连接
            // 目标、并发照样升到上限；延迟回到基线附近就解除。
            state.slow_limited = latency_regressed;
            if error_rate >= 0.25 {
                state.target = (state.target / 2).max(1);
                state.cooldown = WINDOW;
            } else if error_rate > 0.05 || latency_regressed {
                state.target = state.target.saturating_sub(1).max(1);
                state.cooldown = WINDOW / 2;
            } else if state.window_saturated && state.cooldown == 0 && state.target < state.max {
                // Additive probing keeps growth bounded and avoids endless sweeps.
                state.target += 1;
            }
            if error_rate == 0.0
                && (state.best_service_us == 0.0 || state.service_ewma_us < state.best_service_us)
            {
                state.best_service_us = state.service_ewma_us;
            }
            reset_window(&mut state);
        }
        drop(state);
        self.changed.notify_waiters();
    }
}

impl HostPermit {
    pub(crate) fn reused(&mut self) {
        self.reused = true;
    }

    pub(crate) fn connected_in(&mut self, elapsed: Duration) {
        self.reused = false;
        self.connect_time = Some(elapsed);
    }

    /// 过了这个时刻还没结束就算慢请求，该调用 [`HostPermit::park`]。
    pub(crate) fn slow_at(&self) -> tokio::time::Instant {
        tokio::time::Instant::from_std(self.started + self.slow_after)
    }

    /// 请求等首字节或读正文超过“慢”阈值（[`HostPermit::slow_at`]）时调用：让出热连接名额。
    ///
    /// 热连接目标从 2 起步、攒够一个观察窗口才加一。慢请求一直占着名额的话，同一源站
    /// 后面的快请求要排到慢请求结束（批量抓取里快页面被 5 s 的慢页面拖住）。所以让它停放：
    /// 不再挤占热连接，但仍受 `max` 这个安全上限约束，结束时照常计入服务时间和结果。
    /// 慢本身不算过载；延迟随负载上升才算，见 `complete`。可重复调用。
    pub(crate) fn park(&mut self) {
        if !self.parked && !self.finished && self.control.park() {
            self.parked = true;
        }
    }

    pub(crate) fn finish(mut self, outcome: HostOutcome) {
        self.finished = true;
        self.control.complete(
            outcome,
            self.started.elapsed(),
            self.reused,
            self.connect_time,
            false,
            self.parked,
            self.started.elapsed() >= self.slow_after,
        );
    }

    pub(crate) fn abandon(mut self) {
        self.finished = true;
        self.control.complete(
            HostOutcome::Neutral,
            self.started.elapsed(),
            self.reused,
            self.connect_time,
            false,
            self.parked,
            self.started.elapsed() >= self.slow_after,
        );
    }
}

impl Drop for HostPermit {
    fn drop(&mut self) {
        if !self.finished {
            self.finished = true;
            self.control.complete(
                HostOutcome::Failure,
                self.started.elapsed(),
                self.reused,
                self.connect_time,
                true,
                self.parked,
                self.started.elapsed() >= self.slow_after,
            );
        }
    }
}

fn reset_window(state: &mut State) {
    state.window_samples = 0;
    state.window_failures = 0;
    state.window_saturated = false;
}

fn ewma(current: f64, sample: f64) -> f64 {
    if current == 0.0 {
        sample
    } else {
        current * (1.0 - EWMA_ALPHA) + sample * EWMA_ALPHA
    }
}

fn micros(value: f64) -> Duration {
    Duration::from_micros(value.max(0.0).min(u64::MAX as f64) as u64)
}

#[cfg(test)]
#[path = "tests_host_connections.rs"]
mod tests;
