//! Statistical evidence for closed-loop scheduling decisions.
//!
//! A single telemetry sample is not evidence. One-second QPS readings routinely
//! move by more than the effect a controller is trying to detect, so every
//! decision here compares window means instead of adjacent samples. A window is
//! ready when it holds enough samples for the observed variance to resolve the
//! controller's minimum effect, or when it reaches its hard sample cap.

/// Fixed-capacity rolling window of finite, non-negative samples.
///
/// The window keeps the most recent `capacity` values. `ready` implements a
/// sequential stopping rule: sampling stops early once the relative standard
/// error is small enough, and always stops at the cap so a noisy host cannot
/// stall adaptation forever.
#[derive(Debug, Clone)]
pub struct SampleWindow {
    samples: Box<[f64]>,
    next: usize,
    len: usize,
}

impl SampleWindow {
    pub fn new(capacity: usize) -> Self {
        let capacity = capacity.max(1);
        Self {
            samples: vec![0.0; capacity].into_boxed_slice(),
            next: 0,
            len: 0,
        }
    }

    pub fn capacity(&self) -> usize {
        self.samples.len()
    }

    pub fn len(&self) -> usize {
        self.len
    }

    pub fn is_empty(&self) -> bool {
        self.len == 0
    }

    /// Records one sample. Non-finite values are ignored so a telemetry glitch
    /// cannot poison the mean.
    pub fn push(&mut self, value: f64) {
        if !value.is_finite() {
            return;
        }
        self.samples[self.next] = value.max(0.0);
        self.next = (self.next + 1) % self.samples.len();
        self.len = (self.len + 1).min(self.samples.len());
    }

    pub fn clear(&mut self) {
        self.next = 0;
        self.len = 0;
    }

    pub fn mean(&self) -> f64 {
        if self.len == 0 {
            return 0.0;
        }
        self.samples[..self.len].iter().sum::<f64>() / self.len as f64
    }

    pub fn max(&self) -> f64 {
        self.samples[..self.len].iter().copied().fold(0.0, f64::max)
    }

    /// Unbiased sample variance. Zero for a single observation.
    pub fn variance(&self) -> f64 {
        if self.len < 2 {
            return 0.0;
        }
        let mean = self.mean();
        self.samples[..self.len]
            .iter()
            .map(|value| {
                let delta = value - mean;
                delta * delta
            })
            .sum::<f64>()
            / (self.len - 1) as f64
    }

    /// Standard error of the mean, relative to the mean itself. Infinite when
    /// the mean is zero so an all-zero window only becomes ready at the cap.
    pub fn relative_sem(&self) -> f64 {
        let mean = self.mean();
        if mean <= 0.0 {
            return f64::INFINITY;
        }
        self.variance().max(0.0).sqrt() / (self.len as f64).sqrt() / mean
    }

    /// True when the window holds at least `min_samples` observations and
    /// either reached the cap or is precise enough for `relative_sem_target`.
    pub fn ready(&self, min_samples: usize, relative_sem_target: f64) -> bool {
        let min_samples = min_samples.clamp(1, self.capacity());
        if self.len < min_samples {
            return false;
        }
        if self.len >= self.capacity() {
            return true;
        }
        self.mean() <= 0.0 || self.relative_sem() <= relative_sem_target.max(0.0)
    }

    pub fn evidence(&self) -> WindowEvidence {
        WindowEvidence {
            samples: self.len,
            mean: self.mean(),
            relative_sem: self.relative_sem(),
        }
    }
}

/// Signed relative change from `baseline` to `measured`.
pub fn relative_change(baseline: f64, measured: f64) -> f64 {
    if baseline <= 0.0 || !baseline.is_finite() {
        return 0.0;
    }
    (measured - baseline) / baseline
}

/// Snapshot of the evidence behind the most recent decision.
#[derive(Debug, Clone, Copy, Default, PartialEq)]
pub struct WindowEvidence {
    pub samples: usize,
    pub mean: f64,
    pub relative_sem: f64,
}

impl WindowEvidence {
    /// Relative effect that must be exceeded before `self` counts as a real
    /// improvement over `baseline`.
    ///
    /// A fixed percentage cannot work on every host: on a quiet machine a 1%
    /// gain is real, while on a noisy one a 3% swing is measurement error. The
    /// required effect is therefore the larger of the configured floor and
    /// `noise_sigmas` combined standard errors of the two window means. The
    /// floor only stops the controller from chasing statistically significant
    /// but operationally irrelevant changes.
    pub fn required_effect(&self, baseline: &WindowEvidence, floor: f64, noise_sigmas: f64) -> f64 {
        let floor = if floor.is_finite() {
            floor.max(0.0)
        } else {
            0.0
        };
        let sigmas = if noise_sigmas.is_finite() {
            noise_sigmas.max(0.0)
        } else {
            0.0
        };
        let baseline_sem = evidence_sem(baseline);
        let measured_sem = evidence_sem(self);
        let noise = sigmas * (baseline_sem * baseline_sem + measured_sem * measured_sem).sqrt();
        floor.max(noise)
    }

    /// True when `self` is a real improvement over `baseline`.
    pub fn is_improvement(&self, baseline: &WindowEvidence, floor: f64, noise_sigmas: f64) -> bool {
        if baseline.mean <= 0.0 || self.mean <= 0.0 {
            return false;
        }
        let effect = (self.mean - baseline.mean) / baseline.mean;
        effect > self.required_effect(baseline, floor, noise_sigmas)
    }
}

/// A single-observation window has no variance estimate; treat its standard
/// error as unknown (infinite) so it can never justify a level change on its
/// own.
fn evidence_sem(evidence: &WindowEvidence) -> f64 {
    if evidence.samples < 2 || !evidence.relative_sem.is_finite() {
        f64::INFINITY
    } else {
        evidence.relative_sem.max(0.0)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn mean_and_variance_use_only_recorded_samples() {
        let mut window = SampleWindow::new(5);
        window.push(100.0);
        window.push(102.0);
        assert_eq!(window.len(), 2);
        assert!((window.mean() - 101.0).abs() < 1e-9);
        assert!(window.variance() > 0.0);
    }

    #[test]
    fn window_keeps_most_recent_capacity_samples() {
        let mut window = SampleWindow::new(3);
        for value in [10.0, 20.0, 30.0, 40.0] {
            window.push(value);
        }
        assert_eq!(window.len(), 3);
        assert!((window.mean() - 30.0).abs() < 1e-9);
    }

    #[test]
    fn quiet_window_stops_before_the_cap() {
        let mut window = SampleWindow::new(5);
        for value in [100.0, 100.1, 99.9] {
            window.push(value);
        }
        assert!(window.ready(3, 0.01));
    }

    #[test]
    fn noisy_window_waits_for_the_cap() {
        let mut window = SampleWindow::new(5);
        for value in [80.0, 120.0, 90.0, 110.0] {
            window.push(value);
        }
        assert!(!window.ready(3, 0.01));
        window.push(100.0);
        assert!(window.ready(3, 0.01));
    }

    #[test]
    fn clear_discards_previous_evidence() {
        let mut window = SampleWindow::new(4);
        window.push(100.0);
        window.clear();
        assert!(window.is_empty());
        assert_eq!(window.mean(), 0.0);
    }

    #[test]
    fn a_real_small_gain_beats_the_floor_when_windows_are_quiet() {
        // 1.5% is below the old fixed 2% floor but far outside the noise of
        // two low-variance windows, so it must be accepted.
        let baseline = WindowEvidence {
            samples: 5,
            mean: 958_000.0,
            relative_sem: 0.0015,
        };
        let measured = WindowEvidence {
            samples: 5,
            mean: 973_000.0,
            relative_sem: 0.0011,
        };
        assert!(measured.is_improvement(&baseline, 0.005, 2.0));
        assert!(measured.required_effect(&baseline, 0.005, 2.0) < 0.015);
    }

    #[test]
    fn a_noisy_swing_is_not_an_improvement() {
        let baseline = WindowEvidence {
            samples: 3,
            mean: 970_000.0,
            relative_sem: 0.015,
        };
        let measured = WindowEvidence {
            samples: 3,
            mean: 985_000.0,
            relative_sem: 0.014,
        };
        // +1.5% with ~1.5% standard errors is inside the noise band.
        assert!(!measured.is_improvement(&baseline, 0.005, 2.0));
    }

    #[test]
    fn a_single_sample_never_justifies_a_change() {
        let baseline = WindowEvidence {
            samples: 1,
            mean: 900_000.0,
            relative_sem: 0.0,
        };
        let measured = WindowEvidence {
            samples: 5,
            mean: 1_000_000.0,
            relative_sem: 0.001,
        };
        assert!(!measured.is_improvement(&baseline, 0.0, 2.0));
    }
}
