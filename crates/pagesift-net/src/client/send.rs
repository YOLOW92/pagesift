//! 发请求：建客户端、取连接、发 GET、读响应头。

use super::*;

impl Client {
    pub(crate) fn new(
        resolver: Arc<GuardedResolver>,
        connect_timeout: Duration,
        dns_timeout: Duration,
        tls_timeout: Duration,
        first_byte_timeout: Duration,
        happy_eyeballs_delay: Duration,
        tail_threshold: Duration,
        idle_timeout: Duration,
        max_idle: usize,
        adaptive_connections: bool,
        max_hot_per_host: usize,
        stages: StageController,
        user_agent: HeaderValue,
    ) -> Result<Self> {
        let mut roots = rustls::RootCertStore::empty();
        let native = rustls_native_certs::load_native_certs();
        for cert in native.certs {
            // 个别系统证书格式不对时跳过，不影响其他证书
            let _ = roots.add(cert);
        }
        if roots.is_empty() {
            return Err(Error::Config("no system root certificates found".into()));
        }
        let provider = Arc::new(rustls::crypto::ring::default_provider());
        let mut tls = rustls::ClientConfig::builder_with_provider(provider)
            .with_safe_default_protocol_versions()
            .map_err(|e| Error::Config(e.to_string()))?
            .with_root_certificates(roots)
            .with_no_client_auth();
        tls.alpn_protocols = vec![b"h2".to_vec(), b"http/1.1".to_vec()];
        Ok(Self(Arc::new(Inner {
            resolver,
            tls: TlsConnector::from(Arc::new(tls)),
            connect_timeout,
            dns_timeout,
            tls_timeout,
            first_byte_timeout,
            happy_eyeballs_delay,
            tail_threshold,
            idle_timeout,
            max_idle,
            adaptive_connections,
            max_hot_per_host: max_hot_per_host.max(1),
            user_agent,
            pools: Mutex::new(Pools::default()),
            hosts: Mutex::new(HashMap::new()),
            stages,
        })))
    }

    /// 发一个 GET。`headers` 是调用方的请求头（凭证、条件请求等），连接相关的头由这里加。
    pub(crate) async fn get(&self, url: &Url, headers: HeaderMap) -> Result<Response> {
        let https = match url.scheme() {
            "https" => true,
            "http" => false,
            s => return Err(Error::InvalidUrl(format!("unsupported scheme {s}"))),
        };
        let host = url
            .host_str()
            .ok_or_else(|| Error::InvalidUrl(crate::redact_url(url.as_str())))?;
        let port = url
            .port_or_known_default()
            .unwrap_or(if https { 443 } else { 80 });
        let key = Key {
            https,
            host: host.to_string(),
            port,
        };
        let control = self.0.host_control(&key);
        let mut lifecycle = Some(control.acquire_within(Some(self.0.tail_threshold)).await);
        let mut stage = None;
        let path = match url.query() {
            Some(q) => format!("{}?{q}", url.path()),
            None => url.path().to_string(),
        };
        let authority = match url.port() {
            Some(p) => format!("{host}:{p}"),
            None => host.to_string(),
        };

        // 先用池里的连接；若池化连接已在空闲期被对端单方面关闭（如 broken pipe / RST / channel closed），
        // 自动利用全新连接进行一次透明重试（符合 RFC 规范中对幂等 GET 请求在池化连接上的标准容错要求）。
        let mut pooled = self.0.take(&key).await;
        let mut retried = false;

        loop {
            let (conn, from_pool) = match pooled.take() {
                Some(c) => {
                    lifecycle.as_mut().expect("lifecycle permit").reused();
                    (c, true)
                }
                None => {
                    let started = Instant::now();
                    match self.connect(&key).await {
                        Ok(conn) => {
                            lifecycle
                                .as_mut()
                                .expect("lifecycle permit")
                                .connected_in(started.elapsed());
                            (conn, false)
                        }
                        Err(error) => {
                            lifecycle
                                .take()
                                .expect("lifecycle permit")
                                .finish(HostOutcome::Failure);
                            return Err(error);
                        }
                    }
                }
            };

            if stage.is_none() {
                stage = Some(self.0.stages.acquire(NetworkStage::Http).await);
            }

            let req_uri = match &conn {
                Conn::H1(_) => path.parse::<Uri>(),
                Conn::H2(_) => format!("{}://{authority}{path}", url.scheme()).parse::<Uri>(),
            }
            .map_err(|_| Error::InvalidUrl(crate::redact_url(url.as_str())))?;

            let mut req = Request::get(req_uri)
                .body(Empty::<Bytes>::new())
                .map_err(|e| Error::Network(e.to_string()))?;

            let h = req.headers_mut();
            if matches!(conn, Conn::H1(_)) {
                h.insert(
                    header::HOST,
                    HeaderValue::from_str(&authority).map_err(|_| Error::Header("host".into()))?,
                );
            }
            h.insert(header::USER_AGENT, self.0.user_agent.clone());
            // 现代合规 Accept 与 Accept-Language 头部，显著降低 CDN/WAF 误判拦截率
            h.insert(
                header::ACCEPT,
                HeaderValue::from_static("text/html,application/xhtml+xml,application/xml;q=0.9,image/avif,image/webp,*/*;q=0.8"),
            );
            h.insert(
                header::ACCEPT_LANGUAGE,
                HeaderValue::from_static("en-US,en;q=0.9,zh-CN;q=0.8,zh;q=0.7"),
            );
            h.insert(
                header::ACCEPT_ENCODING,
                HeaderValue::from_static(ACCEPT_ENCODING),
            );
            for (k, v) in headers.iter() {
                h.insert(k, v.clone());
            }

            let send_started = Instant::now();
            let send_deadline = send_started + self.0.first_byte_timeout;
            let park_at = send_started + self.0.tail_threshold;
            match conn {
                Conn::H1(mut s) => {
                    match first_byte(s.send_request(req), send_deadline, park_at, &mut lifecycle)
                        .await
                    {
                        Ok(Ok(resp)) => {
                            let (parts, body) = resp.into_parts();
                            self.0.stages.record_phase(
                                TransportPhase::Ttfb,
                                send_started.elapsed(),
                                stage_outcome_for_status(parts.status.as_u16()),
                            );
                            return Ok(Response {
                                status: parts.status.as_u16(),
                                headers: parts.headers,
                                ttfb: send_started.elapsed(),
                                body,
                                release: Some((self.0.clone(), key, s)),
                                lifecycle,
                                stage,
                                stages: self.0.stages.clone(),
                                body_started: Instant::now(),
                                body_recorded: false,
                                tail_threshold: self.0.tail_threshold,
                                in_tail: false,
                            });
                        }
                        Ok(Err(_e)) if from_pool && !retried => {
                            self.0.stages.record_phase(
                                TransportPhase::Ttfb,
                                send_started.elapsed(),
                                StageOutcome::TransientFailure,
                            );
                            retried = true;
                            continue;
                        }
                        Ok(Err(e)) => {
                            lifecycle
                                .take()
                                .expect("lifecycle permit")
                                .finish(HostOutcome::Failure);
                            let error = network(&e);
                            self.0.stages.record_phase(
                                TransportPhase::Ttfb,
                                send_started.elapsed(),
                                stage_outcome_for_error(&error),
                            );
                            stage
                                .take()
                                .expect("HTTP stage permit")
                                .finish(stage_outcome_for_error(&error));
                            return Err(error);
                        }
                        Err(_) => {
                            lifecycle
                                .take()
                                .expect("lifecycle permit")
                                .finish(HostOutcome::Failure);
                            let error = Error::Timeout(TimeoutPhase::FirstByte);
                            self.0.stages.record_phase(
                                TransportPhase::Ttfb,
                                send_started.elapsed(),
                                StageOutcome::TransientFailure,
                            );
                            stage
                                .take()
                                .expect("HTTP stage permit")
                                .finish(StageOutcome::TransientFailure);
                            return Err(error);
                        }
                    }
                }
                Conn::H2(mut s) => {
                    match first_byte(s.send_request(req), send_deadline, park_at, &mut lifecycle)
                        .await
                    {
                        Ok(Ok(resp)) => {
                            let (parts, body) = resp.into_parts();
                            self.0.stages.record_phase(
                                TransportPhase::Ttfb,
                                send_started.elapsed(),
                                stage_outcome_for_status(parts.status.as_u16()),
                            );
                            return Ok(Response {
                                status: parts.status.as_u16(),
                                headers: parts.headers,
                                ttfb: send_started.elapsed(),
                                body,
                                release: None,
                                lifecycle,
                                stage,
                                stages: self.0.stages.clone(),
                                body_started: Instant::now(),
                                body_recorded: false,
                                tail_threshold: self.0.tail_threshold,
                                in_tail: false,
                            });
                        }
                        Ok(Err(_e)) if from_pool && !retried => {
                            self.0.stages.record_phase(
                                TransportPhase::Ttfb,
                                send_started.elapsed(),
                                StageOutcome::TransientFailure,
                            );
                            retried = true;
                            self.0.remove_h2(&key);
                            continue;
                        }
                        Ok(Err(e)) => {
                            lifecycle
                                .take()
                                .expect("lifecycle permit")
                                .finish(HostOutcome::Failure);
                            let error = network(&e);
                            self.0.stages.record_phase(
                                TransportPhase::Ttfb,
                                send_started.elapsed(),
                                stage_outcome_for_error(&error),
                            );
                            stage
                                .take()
                                .expect("HTTP stage permit")
                                .finish(stage_outcome_for_error(&error));
                            return Err(error);
                        }
                        Err(_) => {
                            lifecycle
                                .take()
                                .expect("lifecycle permit")
                                .finish(HostOutcome::Failure);
                            let error = Error::Timeout(TimeoutPhase::FirstByte);
                            self.0.stages.record_phase(
                                TransportPhase::Ttfb,
                                send_started.elapsed(),
                                StageOutcome::TransientFailure,
                            );
                            stage
                                .take()
                                .expect("HTTP stage permit")
                                .finish(StageOutcome::TransientFailure);
                            return Err(error);
                        }
                    }
                }
            }
        }
    }

    pub(crate) fn host_connection_stats(&self) -> Vec<HostConnectionStats> {
        self.0.host_connection_stats()
    }
}

/// 等响应头，`deadline` 前没到就超时。过了 `park_at`（尾部阈值）还没到，先把源站的
/// 热连接名额让出来再接着等（见 [`HostPermit::park`]）。
async fn first_byte<F: std::future::Future>(
    fut: F,
    deadline: Instant,
    park_at: Instant,
    lifecycle: &mut Option<HostPermit>,
) -> std::result::Result<F::Output, tokio::time::error::Elapsed> {
    tokio::pin!(fut);
    if park_at < deadline {
        if let Ok(out) = timeout_at(park_at, &mut fut).await {
            return Ok(out);
        }
        if let Some(permit) = lifecycle.as_mut() {
            permit.park();
        }
    }
    timeout_at(deadline, fut).await
}
