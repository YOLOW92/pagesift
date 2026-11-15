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
