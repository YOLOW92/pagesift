//! 测试夹具：同一组行为测试分别跑单主机与"网关 + 多工人"（一个进程内，真实帧协议，内存管道代替 TCP）。
#![allow(dead_code)]

use std::sync::Arc;
use std::time::Duration;

use pagesift_cluster::{
    Completed, Coordinator, Dial, GatewayConfig, GatewayCoordinator, Journal, Ledger, LedgerConfig,
    LocalCoordinator, ManualClock, RemoteCoordinator, Resource, Resources, SiteKey, SiteStates,
    Task, TokenStore, WorkerConfig, WorkerId,
};
use pagesift_stat::Delta;
use tokio::sync::mpsc;

pub type Coord = Arc<dyn Coordinator<Vec<u8>>>;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Mode {
    Local,
    Cluster(usize),
}

pub struct Harness {
    pub clock: Arc<ManualClock>,
    pub ledger: Arc<Ledger>,
    /// 接收提交、取消的入口（单主机是本进程，多服务器是网关）。
    pub front: Coord,
    pub workers: Vec<Coord>,
    pub results: mpsc::UnboundedReceiver<Completed<Vec<u8>>>,
    pub gateway: Option<GatewayCoordinator>,
    pub remotes: Vec<Arc<RemoteCoordinator<Vec<u8>>>>,
}

pub fn token(id: &str) -> String {
    format!("token-for-{id}")
}

pub fn task(site: u64, body: &str) -> Task {
    Task {
        site: SiteKey(site),
        class: 1,
        timeout: Duration::from_secs(60),
        need: Resources::default().with(Resource::Cpu, 1.0),
        body: body.as_bytes().to_vec(),
    }
}

pub fn free() -> Resources {
    Resources::default().with(Resource::Cpu, 4.0)
}

pub fn gateway(
    clock: Arc<ManualClock>,
    journal: Option<Journal>,
) -> (
    GatewayCoordinator,
    mpsc::UnboundedReceiver<Completed<Vec<u8>>>,
) {
    let mut cfg = LedgerConfig::new(clock, false);
    cfg.shards = 4;
    if let Some(j) = journal {
        cfg = cfg.with_journal(j);
    }
    let ledger = Arc::new(Ledger::open(cfg).unwrap());
    let (tx, rx) = mpsc::unbounded_channel();
    let gw = GatewayCoordinator::new(
        ledger,
        Arc::new(tx),
        TokenStore::new(),
        GatewayConfig {
            standby: Some(WorkerId::new("standby")),
            ..GatewayConfig::default()
        },
    );
    (gw, rx)
}

pub fn dial(id: &str) -> Dial {
    Dial {
        worker: WorkerId::new(id),
        token: token(id),
        authority: "gateway.test".into(),
        tls: false,
        max_frame: pagesift_cluster::transport::default_max_frame(),
    }
}

pub fn worker_config(id: &str, min_term: u64, sites: Option<Arc<dyn SiteStates>>) -> WorkerConfig {
    WorkerConfig {
        dial: dial(id),
        weight: 1.0,
        boot: 1,
        min_term,
        profile: Vec::new(),
        delta: Delta::DEFAULT,
        sites,
    }
}

/// 起一个工人：登记令牌，内存管道两端分别交给网关与工人。
pub async fn connect(
    gw: &GatewayCoordinator,
    clock: &Arc<ManualClock>,
    cfg: WorkerConfig,
) -> Result<Arc<RemoteCoordinator<Vec<u8>>>, pagesift_cluster::Error> {
    gw.tokens()
        .add(cfg.dial.worker.clone(), &token(cfg.dial.worker.as_str()));
    let (a, b) = tokio::io::duplex(1 << 16);
    let g = gw.clone();
    tokio::spawn(async move {
        let _ = g.serve(b).await;
    });
    RemoteCoordinator::connect(a, cfg, clock.clone())
        .await
        .map(Arc::new)
}

impl Harness {
    pub async fn new(mode: Mode) -> Self {
        let clock = ManualClock::new();
        match mode {
            Mode::Local => {
                let mut cfg = LedgerConfig::new(clock.clone(), true);
                cfg.shards = 4;
                let ledger = Arc::new(Ledger::open(cfg).unwrap());
                let (tx, rx) = mpsc::unbounded_channel();
                let local: Coord = Arc::new(LocalCoordinator::new(ledger.clone(), Arc::new(tx), 1));
                Self {
                    clock,
                    ledger,
                    front: local.clone(),
                    workers: vec![local],
                    results: rx,
                    gateway: None,
                    remotes: Vec::new(),
                }
            }
            Mode::Cluster(n) => {
                let (gw, rx) = gateway(clock.clone(), None);
                let mut remotes = Vec::new();
                for i in 0..n {
                    let r = connect(&gw, &clock, worker_config(&format!("w{i}"), 0, None))
                        .await
                        .unwrap();
                    remotes.push(r);
                }
                // 每个工人做一次往返，保证所有成员表推送都已到达
                for r in &remotes {
                    let _ = Coordinator::<Vec<u8>>::pull(&**r, Resources::default()).await;
                }
                let workers = remotes.iter().map(|r| r.clone() as Coord).collect();
                Self {
                    clock,
                    ledger: gw.ledger().clone(),
                    front: Arc::new(gw.clone()),
                    workers,
                    results: rx,
                    gateway: Some(gw),
                    remotes,
                }
            }
        }
    }

    /// 让所有工人轮流拉取并立即提交（结果 = 任务体），直到一整轮都拉不到。
    pub async fn drain(&self) -> usize {
        let mut done = 0;
        loop {
            let mut any = false;
            for w in &self.workers {
                if let Some(l) = w.pull(free()).await {
                    w.commit(&l, l.work.body.clone()).await.unwrap();
                    done += 1;
                    any = true;
                }
            }
            if !any {
                return done;
            }
        }
    }

    pub fn tick(&self) {
        match &self.gateway {
            Some(g) => {
                g.tick();
            }
            None => {
                self.ledger.tick();
            }
        }
    }

    pub fn collect(&mut self) -> Vec<Completed<Vec<u8>>> {
        let mut out = Vec::new();
        while let Ok(c) = self.results.try_recv() {
            out.push(c);
        }
        out
    }
}
