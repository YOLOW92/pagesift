//! 出口池的模块测试：每条规则一个合成场景。

use super::*;
use pagesift_stat::rng::Rng;

fn pool(members: &[(&str, EgressClass)], seed: u64) -> EgressPool {
    EgressPool::new(
        members.iter().map(|(n, c)| (n.to_string(), *c)).collect(),
        PoolConfig {
            seed: Some(seed),
            ..Default::default()
        },
    )
}

const A: Site<'static> = Site {
    host: "www.a.example",
    domain: "a.example",
};
const B: Site<'static> = Site {
    host: "www.b.example",
    domain: "b.example",
};

/// 按出口的真实被拒率模拟一次请求，返回（最终用的出口，第一次的动作）。
fn request(p: &EgressPool, site: Site<'_>, deny: &[f64], rng: &mut Rng) -> (usize, Action) {
    let pick = p.choose(site, false, 64);
    let outcome = |e: usize, rng: &mut Rng| {
        if rng.uniform() < deny[e] {
            Outcome::Blocked
        } else {
            Outcome::Served
        }
    };
    let first = outcome(pick.id(), rng);
    let action = p.report(site, &pick, first);
    if let Action::Switch { other_class } = action {
        let retry = p.choose_retry(site, &pick, other_class, 64).expect("retry");
        assert_ne!(retry.id(), pick.id(), "换出口必须换到别的出口");
        let second = outcome(retry.id(), &mut *rng);
        let again = p.report(site, &retry, second);
        assert!(
            !matches!(again, Action::Switch { .. }),
            "一次请求最多换一次出口"
        );
        return (retry.id(), action);
    }
    (pick.id(), action)
}

/// 某出口被站点 A 按 IP 封（被拒率 0.95），另一个出口正常（0.02）。
/// 先强制两个出口轮流抓（非自适应，便于检验）：没有对照数据时被拒不换；有了对照、差异显著后换；
/// 被封出口的后验收敛到真实水平附近。再交给池子自适应选择：选择集中到正常出口。
/// 站点 B 两个出口都正常，不受 A 的影响。
#[test]
fn ip_ban_is_detected_switched_away_and_the_posterior_converges() {
    let p = pool(
        &[
            ("dc", EgressClass::Datacenter),
            ("res", EgressClass::Residential),
        ],
        7,
    );
    let mut rng = Rng::seed(1);
    let deny = [0.95, 0.02];
    // 第一次在被封出口上被拒：另一个出口还没有数据，不能断定是 IP 信誉
    let first = p.lease(0, false, false);
    assert_eq!(p.report(A, &first, Outcome::Blocked), Action::Keep);
    drop(first);
    let mut switched_at = None;
    for i in 0..200 {
        let e = i % 2;
        let pick = p.lease(e, false, false);
        let o = if rng.uniform() < deny[e] {
            Outcome::Blocked
        } else {
            Outcome::Served
        };
        let action = p.report(A, &pick, o);
        if e == 1 {
            assert!(
                !matches!(action, Action::Switch { .. }),
                "正常出口偶尔被拒不算信誉问题"
            );
        } else if matches!(action, Action::Switch { .. }) {
            switched_at.get_or_insert(i);
        }
    }
    let at = switched_at.expect("显著后应换出口");
    assert!(at <= 10, "几次对照之后就应显著：{at}");
    let banned = p.denial_mean(A, 0);
    let fine = p.denial_mean(A, 1);
    assert!((banned - 0.95).abs() < 0.08, "被封出口的后验：{banned}");
    assert!(fine < 0.08, "正常出口的后验：{fine}");
    let mut late = [0usize; 2];
    for _ in 0..300 {
        let (used, _) = request(&p, A, &deny, &mut rng);
        late[used] += 1;
        request(&p, B, &[0.02, 0.02], &mut rng);
    }
    // 自适应阶段几乎都用正常出口（偶尔试探到被封出口，被拒后也会换到正常出口）
    assert!(late[1] >= 295, "{late:?}");
    // B 的观测把它的后验拉离 A 被封出口：类别一层的收缩会带过来一部分 A 的证据，B 自己在出口 0 上
    // 有多少观测取决于采样，所以只断言 B 的正常出口正常、B 的出口 0 明显好于 A 被封的出口
    assert!(p.denial_mean(B, 1) < 0.15);
    assert!(p.denial_mean(B, 0) < banned / 2.0);
}

/// 429、带 Retry-After 的 503、限速类拦截页：一律退避，不换出口，也不计入被拒后验。
#[test]
fn throttling_backs_off_and_never_switches() {
    let p = pool(
        &[
            ("dc", EgressClass::Datacenter),
            ("res", EgressClass::Residential),
        ],
        3,
    );
    assert_eq!(Outcome::from_status(429, false), Outcome::Throttled);
    assert_eq!(Outcome::from_status(503, true), Outcome::Throttled);
    assert_eq!(Outcome::from_status(503, false), Outcome::Neutral);
    assert_eq!(
        Outcome::from_barrier("block", "generic.rate_limited"),
        Outcome::Throttled
    );
    let before = p.denial_mean(A, 0);
    for _ in 0..500 {
        let pick = p.choose(A, false, 64);
        assert_eq!(p.report(A, &pick, Outcome::Throttled), Action::Backoff);
    }
    assert_eq!(p.denial_mean(A, 0), before, "限速不改被拒后验");
}

/// 挑战页：换一次出口，优先换到不同类别；新出口上仍是挑战页就如实报告，不再换。
#[test]
fn challenge_switches_once_preferring_another_class() {
    let p = pool(
        &[
            ("dc1", EgressClass::Datacenter),
            ("dc2", EgressClass::Datacenter),
            ("mob", EgressClass::Mobile),
        ],
        5,
    );
    assert_eq!(
        Outcome::from_barrier("challenge", "cloudflare.challenge"),
        Outcome::Challenge
    );
    assert_eq!(
        Outcome::from_barrier("captcha", "generic.captcha"),
        Outcome::Challenge
    );
    for round in 0..50 {
        let host = format!("s{round}.example");
        let site = Site {
            host: &host,
            domain: &host,
        };
        let pick = p.choose(site, false, 64);
        let action = p.report(site, &pick, Outcome::Challenge);
        assert_eq!(action, Action::Switch { other_class: true });
        let retry = p.choose_retry(site, &pick, true, 64).unwrap();
        if pick.class() == EgressClass::Datacenter {
            assert_eq!(retry.class(), EgressClass::Mobile, "优先换到不同类别");
        } else {
            assert_eq!(retry.class(), EgressClass::Datacenter);
        }
        // 新出口上仍是挑战页：如实报告，不能再换
        let again = p.report(site, &retry, Outcome::Challenge);
        assert!(!matches!(again, Action::Switch { .. }));
        assert!(p.choose_retry(site, &retry, true, 64).is_none());
    }
}

/// 所有出口都被拒：如实报告全站拒绝，不再反复换出口。
#[test]
fn site_wide_denial_is_reported_not_retried() {
    let p = pool(
        &[
            ("dc", EgressClass::Datacenter),
            ("res", EgressClass::Residential),
        ],
        9,
    );
    let mut rng = Rng::seed(2);
    let mut site_wide = 0;
    let mut switches = 0;
    for _ in 0..100 {
        match request(&p, A, &[1.0, 1.0], &mut rng).1 {
            Action::SiteWide => site_wide += 1,
            Action::Switch { .. } => switches += 1,
            _ => {}
        }
    }
    assert!(site_wide >= 90, "site_wide={site_wide} switches={switches}");
    assert!(switches <= 5, "{switches}");
}

/// 同一站点带 Cookie 的请求固定在同一出口：被拒也不换，`choose_retry` 拒绝。
/// 不带 Cookie 的请求不受绑定影响。
#[test]
fn cookie_sessions_stay_on_one_egress() {
    let p = pool(
        &[
            ("a", EgressClass::Datacenter),
            ("b", EgressClass::Residential),
            ("c", EgressClass::Mobile),
        ],
        11,
    );
    let first = p.choose(A, true, 64);
    assert!(first.sticky());
    let bound = first.id();
    for _ in 0..200 {
        let pick = p.choose(A, true, 64);
        assert_eq!(pick.id(), bound);
        let action = p.report(A, &pick, Outcome::Blocked);
        assert!(!matches!(action, Action::Switch { .. }), "{action:?}");
        assert!(p.choose_retry(A, &pick, false, 64).is_none());
    }
    let mut seen = [false; 3];
    for _ in 0..200 {
        seen[p.choose(A, false, 64).id()] = true;
    }
    assert!(
        seen.iter().filter(|s| **s).count() >= 2,
        "不带 Cookie 的请求照常选择"
    );
}

/// 被封的出口在站点解封后会被重新试探并逐步回来：折扣让旧证据变弱，后验的不确定性回升。
#[test]
fn a_banned_egress_is_reprobed_after_the_site_unbans_it() {
    let p = pool(
        &[
            ("dc", EgressClass::Datacenter),
            ("res", EgressClass::Residential),
        ],
        13,
    );
    let mut rng = Rng::seed(3);
    for _ in 0..300 {
        request(&p, A, &[0.95, 0.02], &mut rng);
    }
    let mut late = [0usize; 2];
    for i in 0..8000 {
        let (used, _) = request(&p, A, &[0.02, 0.02], &mut rng);
        if i >= 7000 {
            late[used] += 1;
        }
    }
    assert!(late[0] >= 100, "解封后应回到原出口一部分：{late:?}");
}

/// 影子价格：被拒率相同时，在途多的出口变贵，新请求分到空闲的出口。
#[test]
fn shadow_price_spreads_load_between_equal_egresses() {
    let p = pool(
        &[
            ("a", EgressClass::Datacenter),
            ("b", EgressClass::Datacenter),
        ],
        17,
    );
    // 容量 4：每个出口的均分份额是 2；a 上已经有 2 个在途（饱和）
    let held = [p.lease(0, false, false), p.lease(0, false, false)];
    for _ in 0..50 {
        assert_eq!(p.choose(A, false, 4).id(), 1, "饱和的出口只在别无选择时用");
    }
    drop(held);
}

/// 冷启动：没有数据时所有被拒后验都是 Beta(1,1)，类别先验也是。
#[test]
fn cold_start_is_uninformative() {
    let p = pool(
        &[("a", EgressClass::Datacenter), ("b", EgressClass::Direct)],
        19,
    );
    assert_eq!(p.denial_mean(A, 0), 0.5);
    assert_eq!(
        p.stats.class_prior(EgressClass::Datacenter),
        pagesift_stat::beta::Prior::UNIFORM
    );
    // 单个出口：被拒也不换
    let one = pool(&[("only", EgressClass::Direct)], 1);
    let pick = one.choose(A, false, 8);
    assert!(!matches!(
        one.report(A, &pick, Outcome::Blocked),
        Action::Switch { .. }
    ));
}

/// 同类别的格子足够多时，类别先验由矩估计得出，均值接近该类别的真实被拒率。
#[test]
fn class_prior_is_estimated_from_many_domains() {
    let p = pool(
        &[
            ("dc", EgressClass::Datacenter),
            ("res", EgressClass::Residential),
        ],
        23,
    );
    let mut rng = Rng::seed(4);
    for d in 0..200 {
        let host = format!("d{d}.example");
        let site = Site {
            host: &host,
            domain: &host,
        };
        // 非自适应地轮流给出口：自适应选择下被拒后很少再抽该出口，小样本组的比例会偏高
        for i in 0..20 {
            let pick = p.lease(i % 2, false, false);
            let deny = if pick.id() == 0 { 0.3 } else { 0.05 };
            let o = if rng.uniform() < deny {
                Outcome::Blocked
            } else {
                Outcome::Served
            };
            // 只记后验，不重试
            p.report(site, &pick, o);
        }
    }
    let dc = p.stats.class_prior(EgressClass::Datacenter);
    let res = p.stats.class_prior(EgressClass::Residential);
    let mdc = dc.alpha / (dc.alpha + dc.beta);
    let mres = res.alpha / (res.alpha + res.beta);
    assert!((mdc - 0.3).abs() < 0.08, "datacenter 先验均值 {mdc}");
    assert!((mres - 0.05).abs() < 0.05, "residential 先验均值 {mres}");
}

/// 统计表有界：站点数不超过（每分片上限 × 分片数）。
#[test]
fn tables_stay_within_the_memory_budget() {
    let p = EgressPool::new(
        vec![
            ("a".into(), EgressClass::Datacenter),
            ("b".into(), EgressClass::Residential),
        ],
        PoolConfig {
            memory_budget: 64 * 1024,
            seed: Some(1),
            ..Default::default()
        },
    );
    let shards = std::thread::available_parallelism()
        .map_or(1, |n| n.get())
        .next_power_of_two();
    let bound = p.stats.cap() * shards;
    for d in 0..20_000 {
        let host = format!("h{d}.example");
        let site = Site {
            host: &host,
            domain: &host,
        };
        let pick = p.choose(site, false, 8);
        p.report(site, &pick, Outcome::Served);
    }
    assert!(p.stats.sites() <= bound, "{} > {bound}", p.stats.sites());
}

/// 单出口或没有出口：Policy 不建池子，行为与以前相同（请求不指定出口就走默认出口）。
#[test]
fn policy_builds_a_pool_only_for_two_or_more_egresses() {
    use std::sync::Arc;
    let fetcher = Arc::new(pagesift_net::Fetcher::new(Default::default()).unwrap());
    let mut policy = crate::Policy::new(fetcher.clone(), Default::default());
    assert!(policy.egress_pool().is_none());
    policy.add_egress("one", fetcher.clone());
    assert!(policy.egress_pool().is_none(), "一个出口不建池子");
    policy.add_egress_class("two", EgressClass::Residential, fetcher);
    let pool = policy.egress_pool().expect("两个出口建池子");
    assert_eq!(pool.len(), 2);
    assert_eq!(
        pool.class(pool.position("one").unwrap()),
        EgressClass::Direct
    );
}
