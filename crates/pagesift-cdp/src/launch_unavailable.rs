//! Local browser process groups are Unix-only; remote CDP remains available.

use std::path::PathBuf;
use std::time::Duration;

pub(crate) use crate::endpoint::resolve_endpoint;
use crate::error::{Error, Result};

pub(crate) const PORT: &str = "{port}";

pub(crate) fn find_in_path(_: &[&str]) -> Option<PathBuf> {
    None
}

pub fn detect_browser() -> Option<PathBuf> {
    None
}

fn unsupported() -> Error {
    Error::InvalidOptions(
        "local browser process management is not supported on this platform; use HTTP extraction or a remote CDP endpoint".into(),
    )
}

pub(crate) fn ensure_not_root() -> Result<()> {
    Err(unsupported())
}

pub(crate) fn check_args(_: &[String]) -> Result<()> {
    Err(unsupported())
}

// Pool configuration is portable, but these local-launch fields are never consumed here.
#[allow(dead_code)]
#[derive(Debug, Clone)]
pub(crate) struct LaunchSpec {
    pub executable: PathBuf,
    pub args_override: Option<Vec<String>>,
    pub extra_args: Vec<String>,
    pub timeout: Duration,
}

// An uninhabited handle prevents accidental construction of a fake running process.
pub(crate) enum BrowserProcess {}

impl BrowserProcess {
    pub async fn launch(_: &LaunchSpec) -> Result<Self> {
        Err(unsupported())
    }

    pub fn ws_url(&self) -> &str {
        match *self {}
    }

    pub fn pid(&self) -> u32 {
        match *self {}
    }

    pub fn is_alive(&mut self) -> bool {
        match *self {}
    }

    pub async fn shutdown(self) {
        match self {}
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn local_launch_is_explicitly_unavailable() {
        assert!(detect_browser().is_none());
        assert!(find_in_path(&["chrome"]).is_none());
        let error = ensure_not_root().unwrap_err();
        assert!(matches!(error, Error::InvalidOptions(_)));
        assert!(error.to_string().contains("remote CDP"));
    }

    #[tokio::test]
    async fn direct_remote_endpoint_remains_available() {
        let endpoint = "ws://127.0.0.1:9222/devtools/browser/test";
        assert_eq!(resolve_endpoint(endpoint).await.unwrap(), endpoint);
    }
}
