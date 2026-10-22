//! Explicit on-disk ownership for an HTTP session. Persistence is opt-in and the
//! caller saves after requests; no hidden shared cookie file or automatic identity swap.
use anyhow::{Context, Result, ensure};
use pagesift_net::{CookieSession, SessionConfig, Url};
use std::fs::{self, File, OpenOptions};
use std::io::{Read, Write};
use std::path::{Path, PathBuf};
use std::sync::Arc;

pub struct SessionFile {
    path: PathBuf,
    session: Arc<CookieSession>,
    _lock: File,
}
impl SessionFile {
    pub fn open(
        path: impl AsRef<Path>,
        origin: &Url,
        config: SessionConfig,
        reset: bool,
    ) -> Result<Self> {
        // Validate limits before using them as filesystem read budgets.
        let fresh = CookieSession::new(origin, config.clone())?;
        let path = path.as_ref().to_path_buf();
        let parent = parent(&path);
        fs::create_dir_all(parent)?;
        private_regular(&path)?;
        let lock_path = path.with_extension("session.lock");
        private_regular(&lock_path)?;
        let mut options = OpenOptions::new();
        options.create(true).truncate(false).read(true).write(true);
        owner_only(&mut options);
        let lock = options.open(lock_path)?;
        fs2::FileExt::try_lock_exclusive(&lock).context("session state already has a writer")?;
        let session = if reset {
            fresh
        } else {
            match File::open(&path) {
                Ok(file) => {
                    let mut bytes = Vec::new();
                    file.take(config.max_snapshot_bytes as u64 + 1)
                        .read_to_end(&mut bytes)?;
                    CookieSession::restore(origin, config, &bytes)?
                }
                Err(e) if e.kind() == std::io::ErrorKind::NotFound => fresh,
                Err(e) => return Err(e.into()),
            }
        };
        Ok(Self {
            path,
            session: Arc::new(session),
            _lock: lock,
        })
    }
    pub fn session(&self) -> Arc<CookieSession> {
        self.session.clone()
    }
    pub fn save(&self) -> Result<()> {
        let snapshot = self.session.snapshot()?;
        let temp = self.path.with_extension("session.tmp");
        private_regular(&temp)?;
        let mut options = OpenOptions::new();
        options.create(true).truncate(true).write(true);
        owner_only(&mut options);
        let mut file = options.open(&temp)?;
        file.write_all(snapshot.as_bytes())?;
        file.sync_all()?;
        drop(file);
        fs::rename(&temp, &self.path)?;
        #[cfg(unix)]
        File::open(parent(&self.path))?.sync_all()?;
        Ok(())
    }
}
fn parent(path: &Path) -> &Path {
    path.parent()
        .filter(|p| !p.as_os_str().is_empty())
        .unwrap_or_else(|| Path::new("."))
}
fn owner_only(options: &mut OpenOptions) {
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        options.mode(0o600);
    }
    #[cfg(not(unix))]
    let _ = options;
}
fn private_regular(path: &Path) -> Result<()> {
    match fs::symlink_metadata(path) {
        Ok(m) => {
            ensure!(
                m.is_file() && !m.file_type().is_symlink(),
                "session state must be a regular file"
            );
            #[cfg(unix)]
            {
                use std::os::unix::fs::PermissionsExt;
                ensure!(
                    m.permissions().mode() & 0o077 == 0,
                    "session state must not be accessible by group or others"
                );
            }
        }
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => (),
        Err(e) => return Err(e.into()),
    }
    Ok(())
}
