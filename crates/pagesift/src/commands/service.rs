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
            allow,
        } => runtime()?.block_on(async move {
            if data_dir.is_some() {
                cfg.cache.dir = data_dir;
            }
            // 缓存和任务共用数据目录里的一个存储
            let cache = !no_cache && cfg.cache.enabled.unwrap_or(true);
            let persist = cfg.server.persist_jobs.unwrap_or(true);
            let store = if cache || persist {
                Some(cfg.open_store()?)
            } else {
                None
            };
            let mut config = cfg.scraper(false)?;
            allow.apply(&mut config);
            config.store = store.clone().filter(|_| cache);
            if store.is_some() {
                eprintln!(
                    "data: {} (cache {}, jobs {})",
                    cfg.data_dir().join("store").display(),
                    if cache { "on" } else { "off" },
                    if persist { "persisted" } else { "in memory" }
                );
            }
            let mut api = cfg.api()?;
            api.api_keys.extend(api_keys);
            if public_url.is_some() {
                api.public_url = public_url;
            }
            api.store = store.filter(|_| persist);
            let host = host
                .or(cfg.server.host.clone())
                .unwrap_or_else(|| "127.0.0.1".into());
            let port = port.or(cfg.server.port).unwrap_or(3002);
            let scraper = Arc::new(pagesift::Scraper::new(config)?);
            // 后台确认每个浏览器内核能不能启动，结果写进日志，不耽误服务开始监听
            let probe = scraper.clone();
            tokio::spawn(async move { probe.probe_browsers().await });
            let app = pagesift::api::router(scraper.clone(), api);
            let listener = tokio::net::TcpListener::bind((host.as_str(), port))
                .await
                .with_context(|| format!("binding {host}:{port}"))?;
            eprintln!("pagesift listening on http://{}", listener.local_addr()?);
            pagesift::api::serve(listener, app, shutdown_signal()).await?;
            scraper.shutdown().await;
            Ok(ExitCode::SUCCESS)
        }),
        Command::Diagnose { performance, json } => runtime()?.block_on(async move {
            let engine = pagesift::adaptive::AutonomousTuningEngine::new();
            let diagnosis = engine.diagnose();
            if json {
                println!("{}", serde_json::to_string_pretty(&diagnosis)?);
            } else {
                println!("============================================================");
                println!(
                    "  PageSift {} Performance & Hardware Diagnosis",
                    env!("CARGO_PKG_VERSION")
                );
                println!("============================================================");
                println!("  Logical CPUs:       {}", diagnosis.logical_cores);
                println!(
                    "  Physical Cores:     {} ({} capacity cores, {} runtime workers)",
                    diagnosis.physical_cores, diagnosis.worker_cores, diagnosis.runtime_workers
                );
                println!(
                    "  Topology:           {} socket(s), {} NUMA node(s), {} thread(s)/core",
                    diagnosis.sockets, diagnosis.numa_nodes, diagnosis.threads_per_core
                );
                println!(
                    "  System Memory:      {:.1} MB (Process RSS: {:.1} MB)",
                    diagnosis.total_memory_mb, diagnosis.process_rss_mb
                );
                println!("  CPU Saturation:     {:.1}%", diagnosis.cpu_saturation_pct);
                println!("  Process CPU Time:   {:.3}s", diagnosis.process_cpu_time_s);
                println!("  FD Limit (ulimit):  {}", diagnosis.fd_limit);
                println!("  Active Concurrency: {}", diagnosis.active_concurrency);
                println!("  Active Domains:     {}", diagnosis.active_sites);
                if performance {
                    println!(
                        "  Dispatch Latency:   p50: {:.2}ms | p95: {:.2}ms | p99: {:.2}ms",
                        diagnosis.p50_latency_ms,
                        diagnosis.p95_latency_ms,
                        diagnosis.p99_latency_ms
                    );
                }
                println!("------------------------------------------------------------");
                println!("  Primary State:      {}", diagnosis.primary_bottleneck);
                println!("  Recommendation:     {}", diagnosis.recommendation);
                println!("============================================================");
            }
            Ok(ExitCode::SUCCESS)
        }),
        Command::Tune { profile, out } => runtime()?.block_on(async move {
            println!("PageSift Autonomous Performance Profiler");
            println!("Exploring hardware response curve with profile: {profile}...");
            let engine = pagesift::adaptive::AutonomousTuningEngine::new();
            let knobs = engine.tick(0.0, 0.0, 0);
            let profile_json = serde_json::to_string_pretty(&serde_json::json!({
                "profile_mode": profile,
                "hardware": {
                    "logical_cores": engine.topo.logical_cores,
                    "physical_cores": engine.topo.physical_cores,
                    "worker_cores": engine.topo.worker_cores,
                    "runtime_workers": engine.topo.runtime_workers,
                    "sockets": engine.topo.sockets,
                    "numa_nodes": engine.topo.numa_nodes,
                    "threads_per_core": engine.topo.threads_per_core,
                    "ram_per_core_mb": engine.topo.ram_per_core_mb,
                    "fd_limit": engine.topo.fd_limit,
                    "worker_cpu_groups": engine.topo.cpu_topology.physical_worker_groups(),
                },
                "calibrated_knobs": knobs,
            }))?;

            if let Some(path) = out {
                std::fs::write(&path, &profile_json)?;
                println!(
                    "Tuning profile successfully calibrated and saved to {}",
                    path.display()
                );
            } else {
                println!("{profile_json}");
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
