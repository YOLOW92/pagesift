//! 建连接：Happy Eyeballs 拨号、TLS 与 ALPN 握手、连接池存取。

use super::*;

impl Client {
    pub(super) async fn connect(&self, key: &Key) -> Result<Conn> {
        let ips: Vec<IpAddr> = match key.host.trim_matches(['[', ']']).parse::<IpAddr>() {
            // IP 字面量在 check_url 里已经按 SSRF 规则校验过
            Ok(ip) => vec![ip],
            Err(_) => {
                let dns_deadline = Instant::now() + self.0.dns_timeout;
                match timeout_at(dns_deadline, self.0.resolver.resolve_checked(&key.host)).await {
                    Err(_) => return Err(Error::Timeout(TimeoutPhase::Dns)),
                    Ok(Err(ResolveError::Blocked(Blocked::NoAddress(h)))) => {
                        return Err(Error::Dns(format!("no address for `{h}`")));
                    }
                    Ok(Err(ResolveError::Blocked(b))) => return Err(Error::Blocked(b)),
                    Ok(Err(ResolveError::Io(e))) => {
                        return Err(Error::Dns(format!("{}: {e}", key.host)));
                    }
                    Ok(Ok(ips)) => ips,
                }
            }
        };
        let handshake = self.0.stages.acquire(NetworkStage::Handshake).await;
        let connect_deadline = Instant::now() + self.0.connect_timeout;
        let tcp_started = Instant::now();
        let tcp = match happy_eyeballs(ips, key.port, connect_deadline, self.0.happy_eyeballs_delay)
            .await
        {
            Ok(tcp) => {
                self.0.stages.record_phase(
                    TransportPhase::Tcp,
                    tcp_started.elapsed(),
                    StageOutcome::Success,
                );
                tcp
            }
            Err(error) => {
                self.0.stages.record_phase(
                    TransportPhase::Tcp,
                    tcp_started.elapsed(),
                    stage_outcome_for_error(&error),
                );
                handshake.finish(StageOutcome::TransientFailure);
                return Err(error);
            }
        };
        let _ = tcp.set_nodelay(true);
        if !key.https {
            let result = h1_handshake(TokioIo::new(tcp)).await;
            handshake.finish(
                result
                    .as_ref()
                    .map_or_else(stage_outcome_for_error, |_| StageOutcome::Success),
            );
            return result;
        }
        let name = ServerName::try_from(key.host.trim_matches(['[', ']']).to_string())
            .map_err(|_| Error::InvalidUrl(key.host.clone()))?;
        let tls_deadline = Instant::now() + self.0.tls_timeout;
        let tls_started = Instant::now();
        let tls = match timeout_at(tls_deadline, self.0.tls.connect(name, tcp)).await {
            Err(_) => {
                self.0.stages.record_phase(
                    TransportPhase::Tls,
                    tls_started.elapsed(),
                    StageOutcome::TransientFailure,
                );
                handshake.finish(StageOutcome::TransientFailure);
                return Err(Error::Timeout(TimeoutPhase::Tls));
            }
            Ok(Err(error)) => {
                self.0.stages.record_phase(
                    TransportPhase::Tls,
                    tls_started.elapsed(),
                    StageOutcome::DeterministicFailure,
                );
                handshake.finish(StageOutcome::DeterministicFailure);
                return Err(crate::tls::from_io(&error));
            }
            Ok(Ok(tls)) => {
                self.0.stages.record_phase(
                    TransportPhase::Tls,
                    tls_started.elapsed(),
                    StageOutcome::Success,
                );
                tls
            }
        };
        if tls.get_ref().1.alpn_protocol() == Some(b"h2") {
            let result = http2::Builder::new(TokioExecutor::new())
                .handshake(TokioIo::new(tls))
                .await;
            let (sender, conn) = match result {
                Ok(connection) => connection,
                Err(error) => {
                    let error = network(&error);
                    handshake.finish(stage_outcome_for_error(&error));
                    return Err(error);
                }
            };
            tokio::spawn(conn);
            self.0.put_h2(key.clone(), sender.clone());
            handshake.finish(StageOutcome::Success);
            return Ok(Conn::H2(sender));
        }
        let result = h1_handshake(TokioIo::new(tls)).await;
        handshake.finish(
            result
                .as_ref()
                .map_or_else(stage_outcome_for_error, |_| StageOutcome::Success),
        );
        result
    }
}

pub(super) async fn happy_eyeballs(
    ips: Vec<IpAddr>,
    port: u16,
    deadline: Instant,
    fallback_delay: Duration,
) -> Result<TcpStream> {
    let candidates = interleave_address_families(ips);
    if candidates.is_empty() {
        return Err(Error::Network("no addresses to connect to".into()));
    }
    let mut attempts = JoinSet::new();
    for (index, ip) in candidates.into_iter().enumerate() {
        let delay = fallback_delay.saturating_mul(index as u32);
        attempts.spawn(async move {
            if !delay.is_zero() {
                tokio::time::sleep(delay).await;
            }
            match timeout_at(deadline, TcpStream::connect((ip, port))).await {
                Ok(result) => result,
                Err(_) => Err(std::io::Error::new(
                    std::io::ErrorKind::TimedOut,
                    "connect timed out",
                )),
            }
        });
    }
    let mut last = None;
    while let Some(result) = attempts.join_next().await {
        match result {
            Ok(Ok(stream)) => {
                attempts.abort_all();
                return Ok(stream);
            }
            Ok(Err(error)) => last = Some(error),
            Err(error) if error.is_cancelled() => {}
            Err(error) => {
                last = Some(std::io::Error::other(format!(
                    "connect task failed: {error}"
                )))
            }
        }
        if Instant::now() >= deadline {
            attempts.abort_all();
            return Err(Error::Timeout(TimeoutPhase::Connect));
        }
    }
    if Instant::now() >= deadline
        || last
            .as_ref()
            .is_some_and(|error| error.kind() == std::io::ErrorKind::TimedOut)
    {
        return Err(Error::Timeout(TimeoutPhase::Connect));
    }
    Err(Error::Network(format!(
        "connect error: {}",
        last.map_or_else(|| "no viable address".into(), |error| error.to_string())
    )))
}

pub(super) fn interleave_address_families(ips: Vec<IpAddr>) -> Vec<IpAddr> {
    let mut v4 = ips.iter().copied().filter(IpAddr::is_ipv4);
    let mut v6 = ips.iter().copied().filter(IpAddr::is_ipv6);
    let start_v6 = ips.first().is_some_and(IpAddr::is_ipv6);
    let mut out = Vec::with_capacity(ips.len());
    loop {
        let before = out.len();
        if start_v6 {
            out.extend(v6.next());
            out.extend(v4.next());
        } else {
            out.extend(v4.next());
            out.extend(v6.next());
        }
        if out.len() == before {
            return out;
        }
    }
}

pub(super) fn stage_outcome_for_status(status: u16) -> StageOutcome {
    if matches!(status, 429 | 503) {
        StageOutcome::Overload
    } else {
        StageOutcome::Success
    }
}

pub(super) fn stage_outcome_for_error(error: &Error) -> StageOutcome {
    match error {
        Error::Timeout(_) | Error::Dns(_) => StageOutcome::TransientFailure,
        Error::Blocked(_) | Error::RedirectRejected { .. } => StageOutcome::PolicyRejected,
        Error::InvalidUrl(_)
        | Error::Config(_)
        | Error::Session(_)
        | Error::UserAgent(_)
        | Error::Header(_)
        | Error::TooManyRedirects(_)
        | Error::ContentDecoding(_) => StageOutcome::DeterministicFailure,
        Error::Tls { .. } => StageOutcome::DeterministicFailure,
        Error::Network(_) => StageOutcome::TransientFailure,
    }
}

pub(super) enum Conn {
    H1(H1),
    H2(H2),
}

pub(super) async fn h1_handshake<T>(io: T) -> Result<Conn>
where
    T: hyper::rt::Read + hyper::rt::Write + Unpin + Send + 'static,
{
    let (sender, conn) = http1::Builder::new()
        .read_buf_exact_size(Some(READ_BUF))
        .handshake(io)
        .await
        .map_err(|e| network(&e))?;
    tokio::spawn(async move {
        // 连接出错或对方关闭都在这里结束；请求那边会从 send_request 拿到错误
        let _ = conn.await;
    });
    Ok(Conn::H1(sender))
}

impl Inner {
    pub(super) fn host_control(&self, key: &Key) -> Arc<HostConnectionControl> {
        let mut hosts = self.hosts.lock().expect("host controls lock poisoned");
        hosts
            .entry(key.clone())
            .or_insert_with(|| {
                HostConnectionControl::new(self.adaptive_connections, self.max_hot_per_host)
            })
            .clone()
    }

    pub(super) fn host_connection_stats(&self) -> Vec<HostConnectionStats> {
        let hosts = self.hosts.lock().expect("host controls lock poisoned");
        let mut stats = hosts
            .iter()
            .map(|(key, control)| {
                let scheme = if key.https { "https" } else { "http" };
                control.stats(format!("{scheme}://{}:{}", key.host, key.port))
            })
            .collect::<Vec<_>>();
        stats.sort_by(|a, b| a.origin.cmp(&b.origin));
        stats
    }

    pub(super) async fn take(&self, key: &Key) -> Option<Conn> {
        if let Some(h2) = self.h2_for(key) {
            let mut s = h2;
            if s.ready().await.is_ok() {
                return Some(Conn::H2(s));
            }
            self.pools.lock().expect("lock poisoned").h2.remove(key);
        }
        loop {
            let (mut s, since) = {
                let mut pools = self.pools.lock().expect("lock poisoned");
                let list = pools.h1.get_mut(key)?;
                let entry = list.pop();
                if list.is_empty() {
                    pools.h1.remove(key);
                }
                let entry = entry?;
                pools.idle -= 1;
                entry
            };
            if since.elapsed() >= self.idle_timeout || s.is_closed() {
                continue;
            }
            if s.ready().await.is_ok() {
                return Some(Conn::H1(s));
            }
        }
    }

    pub(super) fn h2_for(&self, key: &Key) -> Option<H2> {
        let pools = self.pools.lock().expect("lock poisoned");
        let s = pools.h2.get(key)?;
        (!s.is_closed()).then(|| s.clone())
    }

    pub(super) fn put_h2(&self, key: Key, sender: H2) {
        self.pools
            .lock()
            .expect("lock poisoned")
            .h2
            .insert(key, sender);
    }

    pub(super) fn remove_h2(&self, key: &Key) {
        let mut pools = self.pools.lock().expect("lock poisoned");
        pools.h2.remove(key);
    }

    pub(super) fn put_h1(&self, key: Key, sender: H1) {
        if self.max_idle == 0 || sender.is_closed() {
            return;
        }
        // Keep one cold probe connection beyond the current hot watermark.
        // Re-probing then reuses an existing socket instead of reconnecting on
        // every 4 -> 5 -> 4 capacity oscillation.
        let idle_target = self
            .host_control(&key)
            .target()
            .saturating_add(1)
            .min(self.max_idle);
        let mut pools = self.pools.lock().expect("lock poisoned");
        if pools.idle >= SWEEP_AT || pools.h1.len() >= 512 {
            let timeout = self.idle_timeout;
            let mut removed = 0;
            pools.h1.retain(|_, list| {
                let before = list.len();
                list.retain(|(s, since)| since.elapsed() < timeout && !s.is_closed());
                removed += before - list.len();
                !list.is_empty()
            });
            pools.idle -= removed;
            pools.h2.retain(|_, s| !s.is_closed());
        }
        let list = pools.h1.entry(key).or_default();
        if list.len() >= idle_target {
            return;
        }
        list.push((sender, Instant::now()));
        pools.idle += 1;
    }
}

pub(super) fn network(e: &hyper::Error) -> Error {
    let mut msg = e.to_string();
    let mut src = std::error::Error::source(e);
    while let Some(s) = src {
        msg.push_str(": ");
        msg.push_str(&s.to_string());
        src = s.source();
    }
    Error::Network(msg)
}
