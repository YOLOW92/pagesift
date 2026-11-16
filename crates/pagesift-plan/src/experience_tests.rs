use super::*;

#[test]
fn family_experience_tracks_rates_and_roundtrips_json() {
    let table = FamilyExperienceTable::new();
    let fam = "https://docs.example.com/guide";
    for i in 0..8 {
        table.record_observation(
            fam,
            &FamilyObservation {
                observed_at_ms: 1000 + i * 1000,
                http_sufficient: Some(true),
                browser_gain: Some(false),
                conditional_hit: Some(i > 0),
                embedded_json_observed: Some(i % 2 == 0),
                substantive_change: Some((3600.0, false)),
                yielded_claims_or_links: Some(true),
                encountered_barrier: Some(false),
                cost_ms: 42.0,
                ..Default::default()
            },
        );
    }
    let stats = table.get_family(fam).expect("family exists");
    assert!(stats.http_sufficient.mean() > 0.8);
    assert!(stats.conditional_hit.mean() > 0.7);
    assert!(stats.barrier_rate.mean() < 0.2);

    let encoded = table.encode_json();
    let restored = FamilyExperienceTable::new();
    assert!(restored.decode_merge_json(&encoded));
    let stats2 = restored.get_family(fam).expect("restored family");
    assert_eq!(stats2.total_observations, 8);
}

#[test]
fn direct_browser_and_censored_failures_do_not_pollute_http_sufficiency() {
    let table = FamilyExperienceTable::new();
    let fam = "https://spa.example.com/app";
    for i in 0..3 {
        table.record_observation(
            fam,
            &FamilyObservation {
                observed_at_ms: 100 + i * 100,
                http_sufficient: Some(false),
                browser_gain: Some(true),
                conditional_hit: None,
                embedded_json_observed: Some(false),
                substantive_change: Some((600.0, false)),
                yielded_claims_or_links: Some(true),
                encountered_barrier: Some(false),
                cost_ms: 500.0,
                ..Default::default()
            },
        );
    }
    let before = table.get_family(fam).unwrap().http_sufficient;

    // DirectBrowser 周期不采样 HTTP，且发生传输故障时按删失处理
    for i in 0..10 {
        table.record_observation(
            fam,
            &FamilyObservation {
                observed_at_ms: 1000 + i * 100,
                http_sufficient: None,
                browser_gain: Some(i % 2 == 0),
                conditional_hit: None,
                embedded_json_observed: None,
                substantive_change: None,
                yielded_claims_or_links: None,
                encountered_barrier: Some(i % 3 == 0),
                cost_ms: 700.0,
                ..Default::default()
            },
        );
    }
    let after = table.get_family(fam).unwrap().http_sufficient;
    assert_eq!(before, after, "unobserved http_sufficient must not drift");
}

#[test]
fn change_rate_snapshot_relearns_on_regime_shifts() {
    let mut snap = ChangeRateSnapshot::default();
    for _ in 0..40 {
        snap.observe(86_400.0, false);
    }
    let static_rate = snap.to_change_rate().mean();

    // 站点头条/报价改为高频更新后，应能在有限次变化内重新学高 λ
    for _ in 0..8 {
        snap.observe(300.0, true);
    }
    let busy_rate = snap.to_change_rate().mean();
    assert!(
        busy_rate > static_rate * 10.0,
        "expected static->busy relearning: static={static_rate}, busy={busy_rate}"
    );

    // 再次恢复长期静态后，旧变化区间退役、速率逐渐回落
    for _ in 0..24 {
        snap.observe(86_400.0, false);
    }
    let cooled_rate = snap.to_change_rate().mean();
    assert!(
        cooled_rate < busy_rate * 0.25,
        "expected busy->static decay: busy={busy_rate}, cooled={cooled_rate}"
    );
}

#[test]
fn learned_url_rule_activates_and_revokes_on_falsification() {
    let table = FamilyExperienceTable::new();
    let rule = RuleKind::DropParam {
        name: "ref".to_string(),
    };
    table.confirm_url_rule("example.com", rule.clone(), 100);
    let mut u = Url::parse("https://example.com/item?id=1&ref=nav").unwrap();
    assert!(table.rewrite_url(&mut u).is_empty());

    table.confirm_url_rule("example.com", rule.clone(), 200);
    let applied = table.rewrite_url(&mut u);
    assert_eq!(applied, vec![rule.clone()]);
    assert_eq!(u.as_str(), "https://example.com/item?id=1");

    // 证伪后立即撤销，不再改写
    table.falsify_url_rule("example.com", &rule, 300);
    let mut u2 = Url::parse("https://example.com/item?id=1&ref=important").unwrap();
    assert!(table.rewrite_url(&mut u2).is_empty());
    assert_eq!(u2.as_str(), "https://example.com/item?id=1&ref=important");
}

#[test]
fn fully_censored_observations_do_not_pollute_any_family_statistics() {
    let table = FamilyExperienceTable::new();
    let fam = "https://docs.example.com/ref";
    table.record_observation(
        fam,
        &FamilyObservation {
            observed_at_ms: 1_000,
            http_sufficient: Some(true),
            browser_gain: None,
            conditional_hit: Some(true),
            embedded_json_observed: Some(false),
            substantive_change: Some((300.0, false)),
            yielded_claims_or_links: Some(true),
            encountered_barrier: Some(false),
            cost_ms: 50.0,
            ..Default::default()
        },
    );
    let baseline = table.get_family(fam).expect("baseline stats");

    // Simulate repeated transport timeouts / budget exhaustion (all signals censored).
    for i in 0..20 {
        table.record_observation(
            fam,
            &FamilyObservation {
                observed_at_ms: 2_000 + i * 100,
                http_sufficient: None,
                browser_gain: None,
                conditional_hit: None,
                embedded_json_observed: None,
                substantive_change: None,
                yielded_claims_or_links: None,
                encountered_barrier: None,
                cost_ms: 30_000.0,
                ..Default::default()
            },
        );
    }

    let after = table.get_family(fam).expect("after censored batch");
    assert_eq!(after.total_observations, baseline.total_observations);
    assert_eq!(after.barrier_rate, baseline.barrier_rate);
    assert_eq!(after.http_sufficient, baseline.http_sufficient);
    assert_eq!(after.ewma_cost_ms, baseline.ewma_cost_ms);
    assert_eq!(after.last_observed_at_ms, baseline.last_observed_at_ms);
}

#[test]
fn unreliable_decode_censors_sufficiency_and_change_learning() {
    let table = FamilyExperienceTable::new();
    let fam = "https://charset.example.com/feed";
    table.record_observation(
        fam,
        &FamilyObservation {
            observed_at_ms: 1_000,
            http_sufficient: Some(true),
            conditional_hit: Some(true),
            substantive_change: Some((300.0, false)),
            yielded_claims_or_links: Some(true),
            encountered_barrier: Some(false),
            cost_ms: 40.0,
            ..Default::default()
        },
    );
    let before = table.get_family(fam).unwrap();
    // 不确定编码（decode:*）或截断正文即便带着幻觉差异也不应污染 http_sufficient 与 substantive_change
    for i in 0..5 {
        table.record_observation(
            fam,
            &FamilyObservation {
                observed_at_ms: 2_000 + i * 500,
                http_sufficient: Some(false),
                substantive_change: Some((60.0, true)),
                yielded_claims_or_links: Some(false),
                encountered_barrier: Some(false),
                cost_ms: 120.0,
                unreliable_observation: true,
                ..Default::default()
            },
        );
    }
    let after = table.get_family(fam).unwrap();
    assert_eq!(after.http_sufficient, before.http_sufficient);
    assert_eq!(after.substantive_change, before.substantive_change);
    assert_eq!(
        after.yielded_claims_or_links_count(),
        before.yielded_claims_or_links_count()
    );
}

trait YieldHelper {
    fn yielded_claims_or_links_count(&self) -> f64;
}
impl YieldHelper for FamilyStats {
    fn yielded_claims_or_links_count(&self) -> f64 {
        self.claim_yield.trials()
    }
}

#[test]
fn modality_separated_costs_and_surprise_forgetting_work() {
    let mut st = FamilyStats::new("https://hybrid.example.com/p");
    for i in 0..5 {
        st.observe(&FamilyObservation {
            observed_at_ms: 1_000 + i * 1_000,
            http_sufficient: Some(false),
            browser_gain: Some(true),
            used_browser: true,
            http_cost_ms: Some(40.0),
            browser_cost_ms: Some(1_800.0),
            cost_ms: 1_840.0,
            ..Default::default()
        });
    }
    st.observe(&FamilyObservation {
        observed_at_ms: 7_000,
        conditional_hit: Some(true),
        is_304: true,
        http_cost_ms: Some(18.0),
        cost_ms: 18.0,
        ..Default::default()
    });
    assert!(st.ewma_304_cost_ms < 25.0);
    assert!(st.ewma_http_cost_ms < 60.0);
    assert!(st.ewma_browser_cost_ms > 1_500.0);

    // 突发从 DirectBrowser 回切为 SSR HTTP 充分：惊异遗忘应在 3 轮内翻越 0.5 门槛
    for j in 0..3 {
        st.observe(&FamilyObservation {
            observed_at_ms: 10_000 + j * 1_000,
            http_sufficient: Some(true),
            browser_gain: Some(false),
            used_browser: false,
            http_cost_ms: Some(38.0),
            cost_ms: 38.0,
            ..Default::default()
        });
    }
    assert!(
        st.http_sufficient.mean() > 0.60 && st.drift_count >= 1,
        "expected fast surprise recovery, got mean={}, drift={}",
        st.http_sufficient.mean(),
        st.drift_count
    );
}

#[test]
fn change_rate_learns_non_aliased_period_and_resists_aliasing() {
    let mut snap = ChangeRateSnapshot::default();
    // 反混叠：若每次粗轮询都在改且中间从未确认过“未变更”，不能锁定假周期
    for k in 1..=5 {
        snap.observe_at(300.0, true, k * 300_000, None);
    }
    assert!(
        snap.regularity.detected_period().is_none(),
        "aliased always-changing coarse polls must not lock a fake period"
    );

    // 真实周期 120s，在每个周期的 105s 确认未改、130s 确认改动（非混叠）
    let mut reg = ChangeRateSnapshot::default();
    reg.observe_at(60.0, true, 1_000_000, None);
    for c in 1..=4 {
        let base = 1_000_000 + (c - 1) * 120_000;
        reg.observe_at(105.0, false, base + 105_000, None);
        reg.observe_at(25.0, true, base + 125_000, None);
    }
    let est = reg
        .regularity
        .detected_period()
        .expect("should detect valid regular period");
    assert!((est.mean_secs - 122.0).abs() < 10.0, "est={est:?}");
    let delay = reg
        .regularity
        .next_delay_secs(1_000_000 + 3 * 120_000 + 10_000, 10.0, 3600.0)
        .expect("delay");
    assert!((100.0..=160.0).contains(&delay), "delay={delay}");
}

#[test]
fn hierarchical_transfer_respects_tenant_isolation_and_rejects_negative_transfer() {
    let table = FamilyExperienceTable::new();
    let tenant_a = Some("tenant-alpha");
    let tenant_b = Some("tenant-beta");

    // Tenant A 成熟同域同构族表现为 SPA（需要浏览器）
    for fam in [
        "https://shop.example.com/items/1",
        "https://shop.example.com/items/2",
    ] {
        for i in 0..5 {
            table.record_observation_for_tenant(
                fam,
                tenant_a,
                &FamilyObservation {
                    observed_at_ms: 1_000 + i * 500,
                    http_sufficient: Some(false),
                    browser_gain: Some(true),
                    used_browser: true,
                    cost_ms: 900.0,
                    ..Default::default()
                },
            );
        }
    }

    // Tenant B 访问同一域名但新族绝不可看到 Tenant A 的先验
    table.record_observation_for_tenant(
        "https://shop.example.com/items/99",
        tenant_b,
        &FamilyObservation {
            observed_at_ms: 10_000,
            http_sufficient: Some(true),
            browser_gain: Some(false),
            cost_ms: 40.0,
            ..Default::default()
        },
    );
    let b_stats = table
        .get_family("https://shop.example.com/items/99")
        .unwrap();
    assert!(b_stats.transfer.donor_description.is_none());

    // 同 Tenant A 出现新族但实际该分支为 SSR 负迁移场景（前 3 次本地观测全部打脸 SPA 先验）
    let cold_a = "https://shop.example.com/items/new";
    for i in 0..3 {
        table.record_observation_for_tenant(
            cold_a,
            tenant_a,
            &FamilyObservation {
                observed_at_ms: 20_000 + i * 500,
                http_sufficient: Some(true),
                browser_gain: Some(false),
                cost_ms: 35.0,
                ..Default::default()
            },
        );
    }
    let a_cold = table.get_family(cold_a).unwrap();
    assert!(
        a_cold.transfer.rejected,
        "expected negative transfer rejection"
    );
    assert!(a_cold.http_sufficient.mean() > 0.70);
}
