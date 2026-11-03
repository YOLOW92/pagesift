//! 长期运行的服务和自检类子命令。

use crate::*;

pub(crate) fn run(command: Command, mut cfg: pagesift::config::Config) -> Result<ExitCode> {
    match command {
        Command::Serve {
            port,
            host,
            api_keys,
            public_url,
            data_dir,
            no_cache,
            role,
            allow,
        } => runtime()?.block_on(async move {
            use pagesift::config::Role;
            if data_dir.is_some() {
                cfg.cache.dir = data_dir;
            }
            let role = cfg.cluster.role(role.as_deref())?;
            // 网关的账本日志和温备回放的日志都放在数据目录的存储里
            let journal = match role {
                Role::Gateway => cfg
                    .cluster
                    .journal
                    .unwrap_or(cfg.cluster.standby.is_some()),
                Role::Standby => true,
                Role::Standalone | Role::Worker => false,
            };
            // 缓存和任务共用数据目录里的一个存储
            let cache = !no_cache && cfg.cache.enabled.unwrap_or(true);
            let persist = cfg.server.persist_jobs.unwrap_or(true);
            let store = if cache || persist || journal {
                Some(cfg.open_store()?)
            } else {
                None
            };
            let mut config = cfg.scraper(false)?;
            allow.apply(&mut config);
            config.store = store.clone().filter(|_| cache);
            if role == Role::Worker && config.audit.is_none() {
                // 没配置审计日志的工人也收集审计记录，随结果交给网关统一追加
                config.audit = Some(Arc::new(pagesift::audit::Audit::collect_only("worker")));
            }
            if store.is_some() {
                eprintln!(
                    "data: {} (cache {}, jobs {})",
                    cfg.data_dir().join("store").display(),
                    if cache { "on" } else { "off" },
                    if persist { "persisted" } else { "in memory" }
                );
            }
            // 请求没给 timeout 时页面任务的总预算（冷启动的租约时长、工人空闲退避的先验）
            let page_budget = config.fetcher.total_timeout;
            let scraper = Arc::new(pagesift::Scraper::new(config)?);
            if role != Role::Gateway {
                // 后台确认每个浏览器内核能不能启动，结果写进日志，不耽误服务开始监听
                // （网关不执行页面，不需要浏览器）
                let probe = scraper.clone();
                tokio::spawn(async move { probe.probe_browsers().await });
            }
            if role == Role::Worker {
                let opts = pagesift::cluster::WorkerOptions {
                    gateways: cfg.cluster.gateways.clone(),
                    link: cfg.cluster.link()?,
                    task_prior: page_budget,
                };
                eprintln!(
                    "pagesift worker {} (gateways: {})",
                    opts.link.id,
                    opts.gateways.join(", ")
                );
                let r =
                    pagesift::cluster::worker::run(scraper.clone(), opts, shutdown_signal()).await;
                scraper.shutdown().await;
                r?;
                return Ok(ExitCode::SUCCESS);
            }
            // 网关的配额和每分钟请求数记在账本里（单主机为 None，照旧）
            let mut ledger_for_api = None;
            // 网关的 /metrics 追加按工人加标签的统计
            let mut extra_metrics = None;
            // 任务记录随账本复制（网关开了日志、温备）；单主机为 None
            let mut job_mirror = None;
            let engine: Arc<dyn pagesift::api::Engine> = match role {
                Role::Standalone | Role::Worker => scraper.clone(),
                Role::Gateway => {
                    let ledger = open_ledger(store.as_ref().filter(|_| journal), false)?;
                    if journal && persist && let Some(s) = &store {
                        job_mirror = Some(pagesift::cluster::replicate_jobs(&ledger, s));
                    }
                    let d = start_gateway(ledger, &cfg.cluster, page_budget).await?;
                    ledger_for_api = Some(d.coordinator().ledger().clone());
                    let m = d.clone();
                    extra_metrics = Some(Arc::new(move || m.metrics()) as pagesift::api::ExtraMetrics);
                    Arc::new(pagesift::cluster::ClusterEngine::new(scraper.clone(), d))
                }
                Role::Standby => {
                    let ledger = open_ledger(store.as_ref(), true)?;
                    // 统计种类在回放主网关的日志之前登记，统计增量才能随日志合并进来
                    pagesift::cluster::register_stats(&ledger);
                    if let Some(s) = &store {
                        job_mirror = Some(pagesift::cluster::replicate_jobs(&ledger, s));
                    }
                    let opts = pagesift::cluster::StandbyOptions {
                        primary: cfg.cluster.primary()?.to_owned(),
                        link: cfg.cluster.link()?,
                    };
                    eprintln!("pagesift standby: following {}", opts.primary);
                    let followed = pagesift::cluster::follow_until_lost(
                        ledger.clone(),
                        &opts,
                        shutdown_signal(),
                    )
                    .await?;
                    if followed.is_none() {
                        scraper.shutdown().await;
                        return Ok(ExitCode::SUCCESS);
                    }
                    let d = start_gateway(ledger, &cfg.cluster, page_budget).await?;
                    ledger_for_api = Some(d.coordinator().ledger().clone());
                    let m = d.clone();
                    extra_metrics = Some(Arc::new(move || m.metrics()) as pagesift::api::ExtraMetrics);
                    Arc::new(pagesift::cluster::ClusterEngine::new(scraper.clone(), d))
                }
            };
            if role != Role::Standalone
                && let Some(mcp_port) = cfg.cluster.mcp_port
            {
                let tools =
                    pagesift::mcp::engine_tools(engine.clone(), scraper.audit().cloned());
                tokio::spawn(async move {
                    if let Err(e) =
                        pagesift::mcp::serve_tools(tools, "http", mcp_port, shutdown_signal()).await
                    {
                        tracing::error!("MCP server stopped: {e:#}");
                    }
                });
            }
            let mut api = cfg.api()?;
            api.api_keys.extend(api_keys);
            if public_url.is_some() {
                api.public_url = public_url;
            }
            api.store = store.filter(|_| persist);
            api.ledger = ledger_for_api;
            api.job_mirror = job_mirror;
            api.extra_metrics = extra_metrics;
            let host = host
                .or(cfg.server.host.clone())
                .unwrap_or_else(|| "127.0.0.1".into());
            let port = port.or(cfg.server.port).unwrap_or(3002);
            // 请求头读取时限在连接层生效，其余在路由的中间件里
            let timeouts = api.timeouts;
            let app = pagesift::api::router(engine, api);
            let listener = tokio::net::TcpListener::bind((host.as_str(), port))
                .await
                .with_context(|| format!("binding {host}:{port}"))?;
            eprintln!("pagesift listening on http://{}", listener.local_addr()?);
            pagesift::api::serve_with(listener, app, timeouts, shutdown_signal()).await?;
            scraper.shutdown().await;
            Ok(ExitCode::SUCCESS)
        }),
        Command::Diagnose { performance, json } => runtime()?.block_on(async move {
            // 0.86 起并发、超时都是在线估计：这里报告硬件、资源预算和冷启动值；
            // 运行中的 USL 参数和每站点并发见服务的指标。
            let report = diagnosis(&cfg, performance)?;
            if json {
                println!("{}", serde_json::to_string_pretty(&report)?);
            } else {
                println!("pagesift {} diagnosis", env!("CARGO_PKG_VERSION"));
                print_report(&report, "");
            }
            Ok(ExitCode::SUCCESS)
        }),
        Command::Tune { profile, out } => runtime()?.block_on(async move {
            // 旧的离线调优（AutonomousTuningEngine）已删除：并发和超时在运行中按数据调整，
            // 不需要预先生成档案。为兼容保留命令，输出冷启动值和硬件信息。
            let mut report = diagnosis(&cfg, true)?;
            report["profile_mode"] = serde_json::json!(profile);
            report["note"] = serde_json::json!(
                "concurrency and timeouts adapt online (USL, latency gradient); this is the cold-start state"
            );
            let text = serde_json::to_string_pretty(&report)?;
            if let Some(path) = out {
                std::fs::write(&path, &text)?;
                println!("written to {}", path.display());
            } else {
                println!("{text}");
            }
            Ok(ExitCode::SUCCESS)
        }),
        Command::Doctor => runtime()?.block_on(cli::doctor(&cfg)),
        Command::Mcp { transport, port } => runtime()?.block_on(async move {
            let mut cfg = cfg;
            cfg.audit.source = Some(format!("mcp-{transport}"));
            let mut sc = cfg.scraper(false)?;
            // http 传输是一个常驻进程，按配置开缓存；stdio 每个客户端各起一个进程，存储不能多进程共用，
            // 不开缓存。数据目录被别的进程（比如 serve）占着时照常运行，只是不缓存
            if transport == "http" && cfg.cache.enabled.unwrap_or(true) {
                match cfg.open_store() {
                    Ok(store) => {
                        eprintln!("cache: {}", cfg.data_dir().join("store").display());
                        sc.store = Some(store);
                    }
                    Err(e)
                        if e.downcast_ref::<pagesift_store::Error>()
                            .is_some_and(pagesift_store::Error::is_locked) =>
                    {
                        eprintln!("warning: {e:#}; running without the cache");
                    }
                    Err(e) => return Err(e),
                }
            }
            let scraper = Arc::new(pagesift::Scraper::new(sc)?);
            let res = pagesift::mcp::run_server_until(
                scraper.clone(),
                &transport,
                port,
                shutdown_signal(),
            )
            .await;
            scraper.shutdown().await;
            res?;
            Ok(ExitCode::SUCCESS)
        }),
        _ => unreachable!(),
    }
}

/// 多服务器模式的账本：`store` 给出时带日志（网关重启从它恢复；温备回放主网关发来的日志）。
fn open_ledger(
    store: Option<&pagesift_store::Store>,
    follower: bool,
) -> Result<Arc<pagesift_cluster::Ledger>> {
    use pagesift_cluster::{Journal, Ledger, LedgerConfig, MonoClock};
    let mut lc = LedgerConfig::new(MonoClock::new(), false);
    if let Some(s) = store {
        lc = lc.with_journal(Journal::new(s.clone()));
    }
    let ledger = if follower {
        Ledger::follower(lc)?
    } else {
        Ledger::open(lc)?
    };
    Ok(Arc::new(ledger))
}

/// 网关的协调器：接工人连接（另一个端口）、跑租约计时器，返回给 HTTP 引擎用的分发器。
async fn start_gateway(
    ledger: Arc<pagesift_cluster::Ledger>,
    c: &pagesift::config::Cluster,
    page_budget: std::time::Duration,
) -> Result<Arc<pagesift::cluster::Dispatch>> {
    let tls = c.server_tls()?;
    let boot = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map_or(0, |d| d.as_millis() as u64);
    let gcfg = pagesift_cluster::GatewayConfig {
        boot,
        ..Default::default()
    };
    let d = pagesift::cluster::Dispatch::new(ledger, c.tokens()?, gcfg, page_budget);
    let addr = c.listen()?;
    let listener = tokio::net::TcpListener::bind(addr)
        .await
        .with_context(|| format!("binding the cluster listener {addr}"))?;
    eprintln!(
        "cluster gateway: workers connect to {} ({}, term {})",
        listener.local_addr()?,
        if tls.is_some() { "TLS" } else { "h2c" },
        d.coordinator().ledger().term()
    );
    let gw = d.coordinator().clone();
    tokio::spawn(async move { gw.run_ticker().await });
    let gw = d.coordinator().clone();
    tokio::spawn(async move {
        if let Err(e) = pagesift::cluster::serve_workers(gw, listener, tls).await {
            tracing::error!("cluster listener stopped: {e}");
        }
    });
    Ok(d)
}

/// 硬件拓扑、内存预算、文件描述符和冷启动的并发值。
fn diagnosis(cfg: &pagesift::config::Config, performance: bool) -> Result<serde_json::Value> {
    let topo = pagesift_policy::topology::CpuTopology::detect();
    let sc = cfg.scraper(false)?;
    let limiter = pagesift_policy::RateLimiter::new(sc.policy.rate.clone());
    let mem = pagesift_net::resources::read_memory();
    let mut report = serde_json::json!({
        "cpu": {
            "logical": topo.logical_cpus,
            "physical": topo.physical_cores,
            "sockets": topo.sockets,
            "numa_nodes": topo.numa_nodes,
            "threads_per_core": topo.threads_per_core,
            "quota_cores": topo.quota_cores,
            "worker_cores": topo.worker_cores,
            "node_cpu_sets": topo.node_cpu_sets(),
        },
        "memory": mem.map(|m| serde_json::json!({
            "total_bytes": m.total_bytes,
            "available_bytes": m.available_bytes,
            "process_rss_bytes": m.process_rss_bytes,
            "budget_bytes": pagesift_net::resources::budget(),
        })),
        "fd": {
            "soft_limit": pagesift_net::resources::fd_soft_limit(),
            "open": pagesift_net::resources::open_fds(),
        },
        "concurrency": {
            "global_target": limiter.global_limit(),
            "global_ceiling": sc.policy.rate.global_ceiling,
            "per_host_target": sc.policy.rate.per_host_concurrency,
            "politeness": sc.policy.rate.politeness,
        },
        "max_body_bytes": pagesift_net::resources::body_cap(sc.fetcher.max_body_bytes),
    });
    if performance {
        report["usl"] = match limiter.global_fit() {
            Some(f) => serde_json::json!({
                "lambda": f.lambda, "sigma": f.sigma, "kappa": f.kappa,
                "peak": f.peak, "peak_lower": f.peak_lower,
            }),
            None => serde_json::json!("no samples yet (fitted online while serving)"),
        };
    }
    Ok(report)
}

fn print_report(v: &serde_json::Value, indent: &str) {
    match v {
        serde_json::Value::Object(map) => {
            for (k, v) in map {
                if v.is_object() {
                    println!("{indent}{k}:");
                    print_report(v, &format!("{indent}  "));
                } else {
                    println!("{indent}{k}: {v}");
                }
            }
        }
        other => println!("{indent}{other}"),
    }
}
