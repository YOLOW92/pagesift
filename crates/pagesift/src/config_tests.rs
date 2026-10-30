//! `config.rs` 的单元测试。

use super::*;

/// `api()` 会解析 `api_keys` 里引用的环境变量；测试里只看 keys、限额和 webhook。
fn cfg_without_env_keys(cfg: &Config) -> pagesift_api::Config {
    let mut c = Config::default();
    c.server.limits = Limits {
        requests_per_minute: cfg.server.limits.requests_per_minute,
        max_concurrent_jobs: cfg.server.limits.max_concurrent_jobs,
        pages_per_day: cfg.server.limits.pages_per_day,
    };
    c.server.keys = cfg
        .server
        .keys
        .iter()
        .map(|k| ApiKey {
            key: k.key.clone(),
            name: k.name.clone(),
            requests_per_minute: k.requests_per_minute,
            max_concurrent_jobs: k.max_concurrent_jobs,
            pages_per_day: k.pages_per_day,
        })
        .collect();
    c.server.webhook.max_attempts = cfg.server.webhook.max_attempts;
    c.api().unwrap()
}

#[test]
fn parses_a_full_config_and_rejects_browser_like_user_agents() {
    let cfg: Config = toml::from_str(
        r#"
        [server]
        port = 4000
        api_keys = ["k1", { env = "PAGESIFT_TEST_KEY_UNSET" }]

        [server.limits]
        requests_per_minute = 600

        [[server.keys]]
        key = "team-a-key"
        name = "team-a"
        requests_per_minute = 60
        max_concurrent_jobs = 2
        pages_per_day = 5000

        [server.webhook]
        max_attempts = 3

        [log]
        format = "json"

        [fetch]
        user_agent = "acme-bot/1.0 (+https://acme.example/bot)"
        timeout_secs = 20
        allow_hosts = ["docs.internal"]

        [[egress]]
        name = "tunnel-a"
        proxy = "socks5://127.0.0.1:1081"

        [policy]
        interval_ms = 500
        allow_crawl_ignore_robots = true

        [browser]
        max_browsers = 1

        [cache]
        enabled = false

        [[credentials]]
        host = "partner.example"
        headers = { Authorization = "Bearer abc" }
        cookies = { session = { env = "HOME" } }

        [llm]
        base_url = "http://localhost:11434/v1"
        model = "qwen3"
        "#,
    )
    .unwrap();
    assert_eq!(cfg.server.port, Some(4000));
    assert!(cfg.api_keys().is_err(), "unset env secret is an error");
    let k = &cfg.server.keys[0];
    assert_eq!(k.name.as_deref(), Some("team-a"));
    assert_eq!(k.pages_per_day, Some(5000));
    let api = cfg_without_env_keys(&cfg);
    assert_eq!(api.key_limits["team-a-key"].max_concurrent_jobs, Some(2));
    assert_eq!(api.default_limits.requests_per_minute, Some(600));
    assert_eq!(api.webhook.max_attempts, 3);
    assert!(!api.webhook.ssrf.allow_private_network);
    assert_eq!(cfg.server.limits.requests_per_minute, Some(600));
    assert_eq!(cfg.server.webhook.max_attempts, Some(3));
    if std::env::var_os("PAGESIFT_LOG_FORMAT").is_none() {
        assert!(cfg.log_json().unwrap());
    }
    let sc = cfg.scraper(true).unwrap();
    assert!(sc.store.is_none(), "cache disabled");
    assert_eq!(sc.fetcher.credentials.len(), 1);
    assert_eq!(sc.policy.rate.default_interval, Duration::from_millis(500));
    assert!(sc.policy.allow_crawl_ignore_robots);
    assert_eq!(sc.llm.as_ref().unwrap().model, "qwen3");
    assert_eq!(sc.fetcher.ssrf.allow_hosts, ["docs.internal"]);
    assert_eq!(
        sc.egress,
        [(
            "tunnel-a".to_string(),
            "socks5://127.0.0.1:1081".to_string()
        )]
    );
    let dup: Config = toml::from_str(
        "[[egress]]\nname = \"a\"\nproxy = \"socks5://h:1\"\n[[egress]]\nname = \"a\"\nproxy = \"socks5://h:2\"",
    )
    .unwrap();
    assert!(dup.scraper(false).is_err(), "duplicate egress names");

    let bad: Config =
        toml::from_str("[fetch]\nuser_agent = \"Mozilla/5.0 (Windows NT 10.0)\"").unwrap();
    assert!(bad.scraper(false).is_err());
    assert!(
        toml::from_str::<Config>("[fetch]\nstealth = true").is_err(),
        "unknown keys are rejected"
    );
}

#[test]
fn example_config_parses() {
    let cfg: Config = toml::from_str(include_str!("../../../docs/pagesift.example.toml")).unwrap();
    assert_eq!(cfg.server.persist_jobs, Some(true));
}

#[test]
fn default_config_path_follows_xdg_and_home() {
    let dir = tempfile::tempdir().unwrap();
    let base = dir.path();
    let home = base.join("home");
    let xdg = base.join("xdg");

    assert_eq!(
        config_base(Some(&xdg), Some(&home), None, None, false),
        Some(xdg.clone())
    );
    // 相对 XDG 不是合法 base，回退 HOME。
    assert_eq!(
        config_base(Some(Path::new("relative")), Some(&home), None, None, false),
        Some(home.join(".config"))
    );
    // 空/相对 HOME 当作未设置。
    assert_eq!(
        config_base(None, Some(Path::new("")), None, None, false),
        None
    );
    assert_eq!(
        config_base(None, Some(Path::new("relative")), None, None, false),
        None
    );
}

#[test]
fn default_config_path_on_windows_uses_appdata_then_profile() {
    let dir = tempfile::tempdir().unwrap();
    let base = dir.path();
    let appdata = base.join("appdata");
    let profile = base.join("profile");

    assert_eq!(
        config_base(None, None, Some(&appdata), Some(&profile), true),
        Some(appdata.clone())
    );
    assert_eq!(
        config_base(None, None, None, Some(&profile), true),
        Some(profile.join(".config"))
    );
    assert_eq!(config_base(None, None, None, None, true), None);
}

#[test]
fn load_resolved_reads_the_default_file_and_records_its_source() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("pagesift.toml");
    std::fs::write(&path, "[server]\nport = 3999\n").unwrap();

    let cfg = Config::load_resolved(None, None, Some(path.clone())).unwrap();
    assert_eq!(cfg.server.port, Some(3999));
    assert_eq!(cfg.source(), Some(path.as_path()));
    assert!(!cfg.config_files_disabled());
}

#[test]
fn missing_default_file_is_not_an_error() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("missing.toml");
    let cfg = Config::load_resolved(None, None, Some(path)).unwrap();
    assert_eq!(cfg.server.port, None);
    assert_eq!(cfg.source(), None);
}

#[test]
fn empty_pagesift_config_disables_default_files() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("pagesift.toml");
    std::fs::write(&path, "[server]\nport = 3999\n").unwrap();

    let cfg = Config::load_resolved(None, Some(std::ffi::OsString::new()), Some(path)).unwrap();
    assert_eq!(cfg.server.port, None);
    assert_eq!(cfg.source(), None);
    assert!(cfg.config_files_disabled());
}

#[test]
fn explicit_config_wins_over_the_disable_switch() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("pagesift.toml");
    std::fs::write(&path, "[server]\nport = 3999\n").unwrap();

    let cfg = Config::load_resolved(Some(&path), Some(std::ffi::OsString::new()), None).unwrap();
    assert_eq!(cfg.server.port, Some(3999));
    assert_eq!(cfg.source(), Some(path.as_path()));
    assert!(!cfg.config_files_disabled());
}

#[test]
fn pagesift_config_env_path_is_used_before_the_default_path() {
    let dir = tempfile::tempdir().unwrap();
    let env_path = dir.path().join("env.toml");
    let default_path = dir.path().join("default.toml");
    std::fs::write(&env_path, "[server]\nport = 4001\n").unwrap();
    std::fs::write(&default_path, "[server]\nport = 4002\n").unwrap();

    let cfg = Config::load_resolved(
        None,
        Some(env_path.clone().into_os_string()),
        Some(default_path),
    )
    .unwrap();
    assert_eq!(cfg.server.port, Some(4001));
    assert_eq!(cfg.source(), Some(env_path.as_path()));
}

#[test]
fn missing_explicit_config_still_errors() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("missing.toml");
    assert!(Config::load_resolved(Some(&path), None, None).is_err());
}

#[test]
fn server_timeouts_are_configurable_and_reject_zero() {
    let cfg: Config = toml::from_str(
        "[server.timeouts]\nheader_read_secs = 5\nupload_body_secs = 600\nrequest_secs = 900\n",
    )
    .unwrap();
    let t = cfg.api().unwrap().timeouts;
    let d = pagesift_api::Timeouts::default();
    assert_eq!(t.header_read, Duration::from_secs(5));
    assert_eq!(t.body, d.body);
    assert_eq!(t.upload_body, Duration::from_secs(600));
    assert_eq!(t.request, Duration::from_secs(900));
    assert_eq!(Config::default().api().unwrap().timeouts, d);

    let cfg: Config = toml::from_str("[server.timeouts]\nbody_secs = 0\n").unwrap();
    let err = cfg.api().unwrap_err().to_string();
    assert!(err.contains("server.timeouts.body_secs"), "{err}");
    assert!(toml::from_str::<Config>("[server.timeouts]\nbody = 3\n").is_err());
}
