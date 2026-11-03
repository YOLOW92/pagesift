//! TLS 与双向 TLS：内存管道上先做 rustls 握手（ALPN h2），再走同一套帧协议。
//! 证书在测试里用 ring 生成 Ed25519 密钥、手写最小的 X.509 v3 DER（RFC 5280 §4.1，RFC 8410）。

mod common;

use common::{free, gateway, task, token, worker_config};
use pagesift_cluster::transport::tls;
use pagesift_cluster::{Coordinator, ManualClock, RemoteCoordinator, WorkerId};
use ring::rand::SystemRandom;
use ring::signature::{Ed25519KeyPair, KeyPair};
use tokio_rustls::rustls::RootCertStore;
use tokio_rustls::rustls::pki_types::{CertificateDer, PrivateKeyDer, PrivatePkcs8KeyDer};

fn der(tag: u8, body: &[u8]) -> Vec<u8> {
    let mut out = vec![tag];
    let n = body.len();
    if n < 0x80 {
        out.push(n as u8);
    } else if n < 0x100 {
        out.extend([0x81, n as u8]);
    } else {
        out.extend([0x82, (n >> 8) as u8, n as u8]);
    }
    out.extend_from_slice(body);
    out
}

fn seq(parts: &[Vec<u8>]) -> Vec<u8> {
    der(0x30, &parts.concat())
}

/// 自签名 Ed25519 证书，SAN 为 `dns`。返回（证书 DER，PKCS#8 私钥）。
fn self_signed(cn: &str, dns: &str) -> (CertificateDer<'static>, PrivateKeyDer<'static>) {
    let pkcs8 = Ed25519KeyPair::generate_pkcs8(&SystemRandom::new()).unwrap();
    let kp = Ed25519KeyPair::from_pkcs8(pkcs8.as_ref()).unwrap();
    let ed25519 = seq(&[der(0x06, &[0x2b, 0x65, 0x70])]);
    let name = seq(&[der(
        0x31,
        &seq(&[der(0x06, &[0x55, 0x04, 0x03]), der(0x0c, cn.as_bytes())]),
    )]);
    let validity = seq(&[der(0x17, b"250101000000Z"), der(0x17, b"491231235959Z")]);
    let mut spki_bits = vec![0];
    spki_bits.extend_from_slice(kp.public_key().as_ref());
    let spki = seq(&[ed25519.clone(), der(0x03, &spki_bits)]);
    let san = seq(&[der(0x82, dns.as_bytes())]);
    let ext = seq(&[der(0x06, &[0x55, 0x1d, 0x11]), der(0x04, &san)]);
    let tbs = seq(&[
        der(0xa0, &der(0x02, &[2])),
        der(0x02, &[0x01, 0x23]),
        ed25519.clone(),
        name.clone(),
        validity,
        name,
        spki,
        der(0xa3, &seq(&[ext])),
    ]);
    let mut sig = vec![0];
    sig.extend_from_slice(kp.sign(&tbs).as_ref());
    let cert = seq(&[tbs, ed25519, der(0x03, &sig)]);
    (
        CertificateDer::from(cert),
        PrivateKeyDer::Pkcs8(PrivatePkcs8KeyDer::from(pkcs8.as_ref().to_vec())),
    )
}

fn roots(c: &CertificateDer<'static>) -> RootCertStore {
    let mut r = RootCertStore::empty();
    r.add(c.clone()).unwrap();
    r
}

async fn run(mutual: bool, present_client_cert: bool) -> Result<(), pagesift_cluster::Error> {
    let clock = ManualClock::new();
    let (gw, mut results) = gateway(clock.clone(), None);
    let (srv_cert, srv_key) = self_signed("gateway", "gateway.test");
    let (cli_cert, cli_key) = self_signed("w0", "w0.test");
    let server = tls::server_config(
        vec![srv_cert.clone()],
        srv_key,
        mutual.then(|| roots(&cli_cert)),
    )?;
    let identity = present_client_cert.then(|| (vec![cli_cert.clone()], cli_key));
    let client = tls::client_config(roots(&srv_cert), identity)?;

    gw.tokens().add(WorkerId::new("w0"), &token("w0"));
    let (a, b) = tokio::io::duplex(1 << 16);
    let g = gw.clone();
    tokio::spawn(async move {
        if let Ok(s) = tls::accept(server, b).await {
            let _ = g.serve(s).await;
        }
    });
    let io = tls::connect(client, "gateway.test", a).await?;
    let mut cfg = worker_config("w0", 0, None);
    cfg.dial.tls = true;
    let w = RemoteCoordinator::<Vec<u8>>::connect(io, cfg, clock).await?;
    gw.ledger().submit(task(1, "tls")).unwrap();
    let l = Coordinator::<Vec<u8>>::pull(&w, free()).await.unwrap();
    Coordinator::<Vec<u8>>::commit(&w, &l, b"over-tls".to_vec()).await?;
    assert_eq!(results.recv().await.unwrap().result, b"over-tls");
    Ok(())
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn tls_and_mutual_tls() {
    run(false, false).await.unwrap();
    run(true, true).await.unwrap();
    // 要求双向 TLS 而工人不出示证书：握手失败或连接被关闭
    assert!(run(true, false).await.is_err());
}
