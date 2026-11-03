//! 并发控制律的模块测试：冷启动等于先验、公式的值、阶跃负载下不振荡。

use super::*;
use crate::rng::Rng;

const D: Delta = Delta::DEFAULT;

#[test]
fn gradient_cold_start_equals_prior_and_needs_saturation_to_grow() {
    let mut g = GradientLimit::new(2.0, D);
    assert_eq!(g.permits(), 2);
    assert_eq!(g.slow_after(), None, "没有样本时没有慢阈值");
    // 名额从没用满：再多样本也不增长
    let mut rng = Rng::seed(1);
    for _ in 0..200 {
        g.complete((0.1 * (1.0 + 0.1 * rng.normal())).max(1e-3), false, 32.0);
    }
    assert_eq!(g.permits(), 2);
    // 用满且延迟平稳：每轮加 √limit
    let before = g.limit();
    for _ in 0..2 {
        g.complete(0.1, true, 32.0);
    }
    assert!(
        (g.limit() - (before + before.sqrt())).abs() < 1e-9,
        "{}",
        g.limit()
    );
}

#[test]
fn gradient_follows_the_formula_on_a_detected_rise() {
    let mut g = GradientLimit::new(16.0, D);
    let mut rng = Rng::seed(2);
    let noisy = |rng: &mut Rng, m: f64| m * (0.05 * rng.normal()).exp();
    for _ in 0..400 {
        g.complete(noisy(&mut rng, 0.1), false, 16.0);
    }
    assert_eq!(g.permits(), 16);
    // RTT 翻 4 倍：检测到上升后本轮结束时 limit ≈ 16·¼ + 4 = 8
    let mut changed = None;
    for _ in 0..64 {
        if let Some(p) = g.complete(noisy(&mut rng, 0.4), false, 16.0) {
            changed = Some(p);
            break;
        }
    }
    let p = changed.expect("a 4× rise must shrink the limit");
    assert!((7..=9).contains(&p), "{p}");
}

#[test]
fn gradient_overload_backs_off_once_per_overshoot() {
    let mut g = GradientLimit::new(9.0, D);
    assert!(g.overload());
    assert!(!g.overload(), "同一批在途请求的第二个 429 不再减");
    assert!((g.limit() - 8.0).abs() < 1e-9, "过载减一个名额（同 0.81）");
    // 减的那一刻在途的 9 个请求都结束后，新的 429 才算新一次越界
    for _ in 0..8 {
        g.complete(0.1, true, 32.0);
    }
    assert!(!g.overload());
    g.complete(0.1, true, 32.0);
    assert!(g.overload());
}

/// 一个容量为 `c` 的服务端：在途数超过 `c` 后排队，RTT ≈ base·max(1, n/c)。
/// 从冷启动开始用满名额，控制器应在 `c` 附近来回，不发散，也不一路压到 1。
#[test]
fn gradient_does_not_oscillate_under_a_step_load() {
    let cap = 64.0;
    let mut rng = Rng::seed(3);
    for c in [4.0, 12.0] {
        let mut g = GradientLimit::new(2.0, D);
        let mut tail = Vec::new();
        for i in 0..20_000 {
            let n = g.permits() as f64;
            let rtt = 0.05 * (n / c).max(1.0) * (0.1 * rng.normal()).exp();
            g.complete(rtt, true, cap);
            if i >= 10_000 {
                tail.push(g.limit());
            }
        }
        let lo = tail.iter().cloned().fold(f64::INFINITY, f64::min);
        let hi = tail.iter().cloned().fold(0.0, f64::max);
        assert!(lo >= c / 2.0 && hi <= c * 3.0, "c={c}: [{lo}, {hi}]");
    }
}

#[test]
fn gradient_slow_threshold_is_the_prediction_bound() {
    let mut g = GradientLimit::new(2.0, D);
    let mut rng = Rng::seed(4);
    let mut xs = Vec::new();
    for _ in 0..2000 {
        let x = 0.2 * (0.5 * rng.normal()).exp();
        xs.push(x);
        g.complete(x, false, 2.0);
    }
    // 对数正态 σ=0.5：q(0.95) = 0.2·exp(1.645·0.5) ≈ 0.455
    let t = g.slow_after().unwrap();
    assert!((t - 0.455).abs() < 0.03, "{t}");
    let above = xs.iter().filter(|&&x| x > t).count() as f64 / xs.len() as f64;
    assert!((above - 0.05).abs() < 0.015, "{above}");
}

fn usl_x(n: f64) -> f64 {
    // λ=100、σ=0.02、κ=0.0005：N* = √(0.98/0.0005) ≈ 44
    100.0 * n / (1.0 + 0.02 * (n - 1.0) + 0.0005 * n * (n - 1.0))
}

#[test]
fn usl_cold_start_equals_prior_and_does_not_move_without_demand() {
    let mut u = UslLimit::new(64.0, D);
    assert_eq!(u.permits(), 64);
    for _ in 0..50 {
        u.window(10.0, usl_x(10.0), false, f64::INFINITY);
    }
    assert_eq!(u.permits(), 64, "需求不够时不探测也不减");
}

#[test]
fn usl_converges_below_the_peak_from_above_and_below() {
    let mut rng = Rng::seed(6);
    for prior in [8.0, 120.0] {
        let mut u = UslLimit::new(prior, D);
        let mut tail = Vec::new();
        for i in 0..400 {
            let n = u.limit();
            let x = usl_x(n) * (1.0 + 0.01 * rng.normal());
            u.window(n, x, true, 1000.0);
            if i >= 300 {
                tail.push(u.limit());
            }
        }
        let peak = (0.98f64 / 0.0005).sqrt();
        let lo = tail.iter().cloned().fold(f64::INFINITY, f64::min);
        let hi = tail.iter().cloned().fold(0.0, f64::max);
        // 停在峰值附近（下界一侧），不振荡、不发散
        assert!(
            lo > peak * 0.5 && hi < peak * 1.6,
            "prior={prior}: [{lo}, {hi}]"
        );
        assert!(hi - lo < peak * 0.6, "prior={prior}: [{lo}, {hi}]");
    }
}

#[test]
fn usl_probing_respects_room() {
    let mut u = UslLimit::new(64.0, D);
    // 线性扩展、看不到回落：只能涨到 room 允许的位置
    for _ in 0..100 {
        let n = u.limit();
        let room = (100.0 - n).max(0.0);
        u.window(n, 10.0 * n, true, room);
    }
    assert_eq!(u.permits(), 100);
}
