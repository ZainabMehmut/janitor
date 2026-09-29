//! Sign and upload Debian `.changes` files.
//!
//! Thin wrappers around `silver_platter::debian::uploader::{debsign,
//! dput_changes}`. Those are blocking, so we call them via
//! [`tokio::task::spawn_blocking`].

use std::path::Path;

use silver_platter::debian::uploader as sp_uploader;
use tracing::info;

use crate::error::{Result, UploadError};
use crate::{DEBSIGN_FAILED_COUNT, UPLOAD_FAILED_COUNT};

/// Sign the `.changes` file at `changes_path` with `debsign`, optionally
/// under GPG key `keyid`.
pub async fn sign_package(changes_path: &Path, keyid: Option<&str>) -> Result<()> {
    info!("Signing package: {}", changes_path.display());

    let path = changes_path.to_path_buf();
    let keyid = keyid.map(str::to_string);
    tokio::task::spawn_blocking(move || sp_uploader::debsign(&path, keyid.as_deref()))
        .await
        .map_err(|e| UploadError::DebsignFailure(format!("debsign task panicked: {}", e)))?
        .map_err(|e| {
            DEBSIGN_FAILED_COUNT.inc();
            match e {
                sp_uploader::SignError::Failed(msg) => UploadError::DebsignFailure(msg),
                sp_uploader::SignError::IOError(e) => UploadError::Io(e),
            }
        })
}

/// Upload the `.changes` file at `changes_path` via `dput`.
///
/// When `dput_host` is `None`, dput picks its default host from `dput.cf`.
pub async fn upload_package(changes_path: &Path, dput_host: Option<&str>) -> Result<()> {
    info!(
        "Uploading package: {} to {}",
        changes_path.display(),
        dput_host.unwrap_or("<default>")
    );

    let path = changes_path.to_path_buf();
    let host = dput_host.map(str::to_string);
    tokio::task::spawn_blocking(move || sp_uploader::dput_changes(&path, host.as_deref()))
        .await
        .map_err(|e| UploadError::DputFailure(format!("dput task panicked: {}", e)))?
        .map_err(|e| {
            UPLOAD_FAILED_COUNT.inc();
            match e {
                sp_uploader::UploadError::Failed(msg) => UploadError::DputFailure(msg),
                sp_uploader::UploadError::IOError(e) => UploadError::Io(e),
            }
        })
}

/// Which packages to upload and how.
#[derive(Debug, Clone)]
pub struct UploadConfig {
    /// GPG key ID for signing (passed to debsign as `-k`).
    pub debsign_keyid: Option<String>,
    /// dput target host, matching an entry in `dput.cf`. `None` lets dput
    /// pick the default host.
    pub dput_host: Option<String>,
    /// Only upload `_source.changes` files.
    pub source_only: bool,
    /// If non-empty, only upload builds whose distribution appears here.
    pub distributions: Vec<String>,
}

impl UploadConfig {
    /// True if `distribution` is in the allow-list (or the list is empty).
    pub fn should_upload_distribution(&self, distribution: &str) -> bool {
        self.distributions.is_empty() || self.distributions.iter().any(|d| d == distribution)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn cfg(distributions: Vec<String>) -> UploadConfig {
        UploadConfig {
            dput_host: Some("test-host".into()),
            debsign_keyid: None,
            source_only: false,
            distributions,
        }
    }

    #[test]
    fn distribution_allow_list_matches_configured_entries() {
        let config = cfg(vec!["unstable".to_string(), "experimental".to_string()]);
        assert!(config.should_upload_distribution("unstable"));
        assert!(config.should_upload_distribution("experimental"));
        assert!(!config.should_upload_distribution("stable"));
    }

    #[test]
    fn empty_allow_list_accepts_any_distribution() {
        let config = cfg(vec![]);
        assert!(config.should_upload_distribution("unstable"));
        assert!(config.should_upload_distribution("stable"));
    }
}
