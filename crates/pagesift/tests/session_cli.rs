//! Every command runs a separate native binary process. Fixture accounts are
//! controlled test identities, not proof of real-world OAuth or browser login.
#[path = "../../pagesift-net/tests/common/mod.rs"]
mod common;
use common::{ok, response, serve};
use std::path::Path;
use std::process::{Command, Output};

fn invoke(url: &str, state: &Path, cfg: &Path, reset: bool) -> Output {
    let mut cmd = Command::new(env!("CARGO_BIN_EXE_pagesift"));
    cmd.args(["session-fetch", url, "--state"])
        .arg(state)
        .arg("--config")
        .arg(cfg)
        .args(["--format", "json"]);
    if reset {
        cmd.arg("--reset-session");
    }
    cmd.output().unwrap()
}
fn json(out: &Output) -> serde_json::Value {
    assert!(
        out.status.success(),
        "{}",
        String::from_utf8_lossy(&out.stderr)
    );
    serde_json::from_slice(&out.stdout).unwrap()
}
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn independent_cli_processes_reuse_only_their_own_identity_and_retirement()
-> anyhow::Result<()> {
    let srv=serve(|s|Some(match s.path.as_str() {
        "/robots.txt"=>ok("User-agent: *\nDisallow: /blocked\n"),
        "/login/alice"=>response(302,&[("Location","/account"),("Set-Cookie","sid=ALICE_FIXTURE; Path=/; HttpOnly")],b""),
        "/login/bob"=>response(302,&[("Location","/account"),("Set-Cookie","sid=BOB_FIXTURE; Path=/; HttpOnly")],b""),
        "/account"=>match s.header("cookie") {
            Some("sid=ALICE_FIXTURE")=>ok("<article><h1>Alice account</h1><p>Private Alice fixture content.</p></article>"),
            Some("sid=BOB_FIXTURE")=>ok("<article><h1>Bob account</h1><p>Private Bob fixture content.</p></article>"),
            _=>response(401,&[],b"login required"),
        },
        "/deny"=>response(401,&[],b"expired access"),
        "/challenge"=>ok("<html><title>Just a moment...</title><body>Checking your browser</body></html>"),
        _=>ok("other")
    })).await;
    let dir = tempfile::tempdir()?;
    let config = dir.path().join("pagesift.toml");
    std::fs::write(
        &config,
        "[fetch]\nallow_private_network = true\n[policy]\ninterval_ms = 0\n",
    )?;
    let a = dir.path().join("alice.json");
    let b = dir.path().join("bob.json");
    let logged = json(&invoke(&srv.url("/login/alice"), &a, &config, false));
    assert_eq!(logged["session"]["uses"], 1);
    assert!(logged["markdown"].as_str().unwrap().contains("Alice"));
    let reused = json(&invoke(&srv.url("/account"), &a, &config, false));
    assert_eq!(reused["session"]["uses"], 2);
    assert!(reused["markdown"].as_str().unwrap().contains("Alice"));
    assert!(
        !invoke(&srv.url("/account"), &b, &config, false)
            .status
            .success()
    );
    json(&invoke(&srv.url("/login/bob"), &b, &config, false));
    assert!(
        json(&invoke(&srv.url("/account"), &b, &config, false))["markdown"]
            .as_str()
            .unwrap()
            .contains("Bob")
    );
    assert!(
        json(&invoke(&srv.url("/account"), &a, &config, false))["markdown"]
            .as_str()
            .unwrap()
            .contains("Alice")
    );
    for _ in 0..3 {
        assert!(
            !invoke(&srv.url("/deny"), &a, &config, false)
                .status
                .success()
        );
    }
    let retired = invoke(&srv.url("/account"), &a, &config, false);
    assert!(!retired.status.success());
    assert!(String::from_utf8_lossy(&retired.stderr).contains("retired"));
    let fresh = json(&invoke(&srv.url("/login/alice"), &a, &config, true));
    assert_eq!(fresh["session"]["uses"], 1);
    let mut expired: serde_json::Value = serde_json::from_slice(&std::fs::read(&a)?)?;
    let now = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)?
        .as_secs();
    expired["created_at"] = serde_json::json!(now - 60);
    expired["expires_at"] = serde_json::json!(now - 1);
    std::fs::write(&a, serde_json::to_vec(&expired)?)?;
    let expired = invoke(&srv.url("/account"), &a, &config, false);
    assert!(!expired.status.success());
    assert!(String::from_utf8_lossy(&expired.stderr).contains("expired"));
    json(&invoke(&srv.url("/login/alice"), &a, &config, true));
    let blocked = invoke(&srv.url("/challenge"), &a, &config, false);
    assert!(!blocked.status.success());
    assert!(String::from_utf8_lossy(&blocked.stderr).contains("access barrier"));
    let saved: serde_json::Value = serde_json::from_slice(&std::fs::read(&a)?)?;
    assert_eq!(saved["retired"], true);
    let other = serve(|_| Some(ok("wrong origin"))).await;
    assert!(
        !invoke(&other.url("/account"), &a, &config, false)
            .status
            .success()
    );
    assert!(other.seen().is_empty());
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        assert_eq!(std::fs::metadata(&a)?.permissions().mode() & 0o777, 0o600);
    }
    Ok(())
}
