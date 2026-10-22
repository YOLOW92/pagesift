use anyhow::Result;
use pagesift::session::SessionFile;
use pagesift_net::{SessionConfig, Url};

#[test]
fn save_reopen_lock_and_origin_guard() -> Result<()> {
    let dir = tempfile::tempdir()?;
    let path = dir.path().join("identity.json");
    let origin: Url = "https://one.test/login".parse()?;
    {
        let state = SessionFile::open(&path, &origin, Default::default(), false)?;
        assert!(SessionFile::open(&path, &origin, Default::default(), false).is_err());
        state
            .session()
            .set_cookie(&origin, "id=SECRET_STATE; Path=/; Secure; HttpOnly");
        state.save()?;
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            assert_eq!(
                std::fs::metadata(&path)?.permissions().mode() & 0o777,
                0o600
            );
        }
    }
    let state = SessionFile::open(&path, &origin, Default::default(), false)?;
    assert_eq!(state.session().stats().cookies, 1);
    state.session().retire();
    state.save()?;
    drop(state);
    let state = SessionFile::open(&path, &origin, Default::default(), false)?;
    assert!(state.session().stats().retired);
    drop(state);
    assert!(
        SessionFile::open(
            &path,
            &"https://other.test/".parse()?,
            Default::default(),
            false
        )
        .is_err()
    );
    let state = SessionFile::open(&path, &origin, Default::default(), true)?;
    assert_eq!(state.session().stats().cookies, 0);
    assert!(!state.session().stats().retired);
    Ok(())
}
#[test]
fn rejects_corrupt_oversized_and_permissive_state() -> Result<()> {
    let dir = tempfile::tempdir()?;
    let path = dir.path().join("session.json");
    let url = "https://one.test".parse()?;
    let state = SessionFile::open(&path, &url, Default::default(), false)?;
    state.save()?;
    drop(state);
    std::fs::write(&path, b"not JSON SECRET")?;
    let result = SessionFile::open(&path, &url, Default::default(), false);
    assert!(result.is_err());
    assert!(!result.err().unwrap().to_string().contains("SECRET"));
    std::fs::write(&path, vec![b'x'; 300_000])?;
    assert!(SessionFile::open(&path, &url, SessionConfig::default(), false).is_err());
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o644))?;
        assert!(SessionFile::open(&path, &url, Default::default(), true).is_err());
    }
    Ok(())
}
