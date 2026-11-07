//! 指纹模拟路径上的连接：TCP 之后的部分。默认路径不经过这里（见 `connect.rs`）。

use std::collections::HashSet;
use std::sync::OnceLock;

use pagesift_rustls::ClientConfig;
use pagesift_rustls::client::ClientConnection;

use super::*;
use crate::fingerprint::{self, FingerprintProfile, ShapedIo, TlsStream};

/// 这次请求实际用的是什么。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum FingerprintUse {
    Off,
    Active(FingerprintProfile),
    /// 这个主机选了档案只声明没实现的算法，用的是普通连接。
    FellBack(FingerprintProfile),
}

pub(super) struct FingerprintState {
    pub(super) profile: FingerprintProfile,
    pub(super) tls: OnceLock<Arc<ClientConfig>>,
    /// 退回过普通连接的主机：记住，之后不再试（不反复握手失败）。
    pub(super) fell_back: Mutex<HashSet<Key>>,
}

impl FingerprintState {
    pub(super) fn new(profile: FingerprintProfile) -> Self {
        Self {
            profile,
            tls: OnceLock::new(),
            fell_back: Mutex::new(HashSet::new()),
        }
    }

    pub(super) fn has_fallen_back(&self, key: &Key) -> bool {
        self.fell_back.lock().expect("lock poisoned").contains(key)
    }

    fn tls_config(&self) -> Result<Arc<ClientConfig>> {
        if let Some(config) = self.tls.get() {
            return Ok(config.clone());
        }
        let config = fingerprint::tls_config(self.profile, None)?;
        Ok(self.tls.get_or_init(|| config).clone())
    }
}

impl Client {
    /// 这个主机这次用哪个档案；`None` 是普通连接。
    pub(super) fn fingerprint_use(&self, key: &Key) -> FingerprintUse {
        match &self.0.fingerprint {
            None => FingerprintUse::Off,
            Some(state) if state.has_fallen_back(key) => FingerprintUse::FellBack(state.profile),
            Some(state) => FingerprintUse::Active(state.profile),
        }
    }

    /// 测试用：用自己的 CA 代替系统根证书。证书校验本身不变。
    #[cfg(test)]
    pub(crate) fn preset_fingerprint_roots(&self, roots: pagesift_rustls::RootCertStore) {
        let state = self.0.fingerprint.as_ref().expect("fingerprint is on");
        let config = fingerprint::tls_config(state.profile, Some(roots)).expect("TLS config");
        state.tls.set(config).expect("first preset");
    }

    #[cfg(test)]
    pub(crate) fn has_fallen_back(&self, https: bool, host: &str, port: u16) -> bool {
        self.0.fingerprint.as_ref().is_some_and(|state| {
            state.has_fallen_back(&Key {
                https,
                host: host.to_string(),
                port,
            })
        })
    }

    pub(super) async fn connect_shaped(
        &self,
        key: &Key,
        wait: &HostWait,
        tcp: super::proxy::BoxStream,
        handshake: StagePermit,
        state: &FingerprintState,
    ) -> Result<Conn> {
        if !key.https {
            let result = h1_handshake(TokioIo::new(ShapedIo::http1(tcp))).await;
            handshake.finish(
                result
                    .as_ref()
                    .map_or_else(stage_outcome_for_error, |_| StageOutcome::Success),
            );
            return result;
        }
        let name = pagesift_rustls::pki_types::ServerName::try_from(
            key.host.trim_matches(['[', ']']).to_string(),
        )
        .map_err(|_| Error::InvalidUrl(key.host.clone()))?;
        let config = state.tls_config()?;
        let conn = ClientConnection::new(config, name).map_err(|e| Error::Tls {
            kind: crate::tls::TlsErrorKind::Other,
            message: e.to_string(),
        })?;
        let tls_started = Instant::now();
        let tls_deadline = tls_started + wait.limit(Phase::Tls, self.0.tls_timeout);
        let tls = match timeout_at(tls_deadline, TlsStream::connect(tcp, conn)).await {
            Err(_) => {
                wait.gave_up(Phase::Tls, tls_started.elapsed(), self.0.tls_timeout);
                self.0.stages.record_phase(
                    TransportPhase::Tls,
                    tls_started.elapsed(),
                    StageOutcome::TransientFailure,
                );
                handshake.finish(StageOutcome::TransientFailure);
                return Err(Error::Timeout(TimeoutPhase::Tls));
            }
            Ok(Err(error)) => {
                wait.done(Phase::Tls, tls_started.elapsed());
                self.0.stages.record_phase(
                    TransportPhase::Tls,
                    tls_started.elapsed(),
                    StageOutcome::DeterministicFailure,
                );
                if fingerprint::is_unimplemented_choice(&error) {
                    // 服务器没有问题，只是选了档案里只声明的算法：不算对这个主机的负反馈
                    handshake.finish(StageOutcome::Success);
                    state
                        .fell_back
                        .lock()
                        .expect("lock poisoned")
                        .insert(key.clone());
                    return Box::pin(self.connect(key, wait)).await;
                }
                handshake.finish(StageOutcome::DeterministicFailure);
                return Err(fingerprint::classify_io(&error));
            }
            Ok(Ok(tls)) => {
                wait.done(Phase::Tls, tls_started.elapsed());
                self.0.stages.record_phase(
                    TransportPhase::Tls,
                    tls_started.elapsed(),
                    StageOutcome::Success,
                );
                tls
            }
        };
        let http2 = &state.profile.data().http2;
        if tls.alpn_protocol() == Some(b"h2") {
            let result = http2::Builder::new(TokioExecutor::new())
                .initial_stream_window_size(http2.initial_stream_window)
                .initial_connection_window_size(http2.connection_window)
                .header_table_size(http2.header_table_size)
                .max_header_list_size(http2.max_header_list_size)
                // Chrome 不发 SETTINGS_MAX_FRAME_SIZE；h2 只在这里设了值时才写进 SETTINGS
                .max_frame_size(None)
                .handshake(TokioIo::new(ShapedIo::http2(tls, http2)))
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
        let result = h1_handshake(TokioIo::new(ShapedIo::http1(tls))).await;
        handshake.finish(
            result
                .as_ref()
                .map_or_else(stage_outcome_for_error, |_| StageOutcome::Success),
        );
        result
    }
}
