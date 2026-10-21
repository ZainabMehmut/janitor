//! Main upload processing logic

use janitor::RunId;
use std::path::{Path, PathBuf};

use sqlx::PgPool;
use tracing::{error, info, warn};

use crate::artifacts::ArtifactProcessor;
use crate::database::{get_run_build, RunBuild};
use crate::error::{Result, UploadError};
use crate::upload::{sign_package, upload_package, UploadConfig};
use crate::utils::{find_changes_files, fix_file_permissions};

fn is_build_changes(path: &Path, build: &RunBuild) -> bool {
    let Ok(changes) = deb822_lossless::Deb822::from_file(path) else {
        return false;
    };
    let Some(para) = changes.paragraphs().next() else {
        return false;
    };
    para.get("Source").as_deref() == Some(build.source.as_str())
        && para.get("Distribution").as_deref() == Some(build.distribution.as_str())
        && para
            .get("Version")
            .and_then(|v| v.parse::<debversion::Version>().ok())
            .as_ref()
            == Some(&build.version)
}

/// The `.changes` files in `dir` that belong to the run's `debian_build` row.
pub async fn run_changes_files(
    pool: &PgPool,
    run_id: &RunId,
    dir: &Path,
    source_only: bool,
) -> Result<Vec<PathBuf>> {
    let Some(build) = get_run_build(pool, run_id).await? else {
        warn!(%run_id, "no debian_build row for run, not uploading");
        return Ok(Vec::new());
    };
    let mut changes_files = find_changes_files(dir, source_only).await?;
    changes_files.retain(|path| {
        let matches = is_build_changes(path, &build);
        if !matches {
            warn!(
                %run_id,
                changes_file = %path.display(),
                source = %build.source,
                version = %build.version,
                distribution = %build.distribution,
                "skipping changes file that does not match the run's build"
            );
        }
        matches
    });
    Ok(changes_files)
}

/// Retrieve, sign, and upload a build result.
pub async fn upload_build_result(
    run_id: &RunId,
    pool: &PgPool,
    artifact_processor: &ArtifactProcessor,
    upload_config: &UploadConfig,
) -> Result<()> {
    info!(
        %run_id,
        dput_host = upload_config.dput_host.as_deref().unwrap_or("<default>"),
        "processing upload"
    );

    let temp_dir = artifact_processor.retrieve_artifacts(run_id).await?;
    let artifacts_path = temp_dir.path();

    fix_file_permissions(artifacts_path).await?;

    let changes_files =
        run_changes_files(pool, run_id, artifacts_path, upload_config.source_only).await?;
    info!(%run_id, count = changes_files.len(), "found changes files");
    if changes_files.is_empty() {
        return Ok(());
    }

    let mut had_failures = false;
    let mut successful_uploads = 0;

    for changes_path in &changes_files {
        let changes_file = changes_path.display();

        if let Err(e) = sign_package(changes_path, upload_config.debsign_keyid.as_deref()).await {
            error!(%run_id, %changes_file, error = %e, "debsign failed");
            had_failures = true;
            continue;
        }
        info!(%run_id, %changes_file, "signed");

        match upload_package(changes_path, upload_config.dput_host.as_deref()).await {
            Ok(_) => {
                info!(%run_id, %changes_file, "uploaded");
                successful_uploads += 1;
            }
            Err(e) => {
                error!(%run_id, %changes_file, error = %e, "dput failed");
                had_failures = true;
            }
        }
    }

    let total = changes_files.len();
    if !had_failures {
        info!(%run_id, successful_uploads, "all packages uploaded");
        Ok(())
    } else if successful_uploads > 0 {
        warn!(%run_id, successful_uploads, total, "partial upload");
        Err(UploadError::PartialUpload {
            successful: successful_uploads,
            total,
        })
    } else {
        error!(%run_id, "no packages uploaded");
        Err(UploadError::NoPackagesUploaded)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use janitor::schema::setup_debian_test_database;
    use janitor::test_with_database;
    use tempfile::TempDir;

    fn write_changes(dir: &Path, source: &str) -> PathBuf {
        let path = dir.join(format!("{source}_2.10-3_amd64.changes"));
        let body = format!(
            "Format: 1.8\nSource: {source}\nVersion: 2.10-3\nDistribution: lintian-fixes\n"
        );
        std::fs::write(&path, body).unwrap();
        path
    }

    async fn insert_build(pool: &PgPool) {
        for query in [
            "INSERT INTO codebase (name, branch_url, url, vcs_type)
             VALUES ('hello', 'https://example.invalid/hello', 'https://example.invalid/hello', 'git')",
            "INSERT INTO change_set (id, campaign) VALUES ('cs-1', 'lintian-fixes')",
            "INSERT INTO run (id, suite, codebase, result_code, logfilenames, change_set)
             VALUES ('run-1', 'lintian-fixes', 'hello', 'success', '{}', 'cs-1')",
            "INSERT INTO debian_build (run_id, version, distribution, source)
             VALUES ('run-1', '2.10-3', 'lintian-fixes', 'hello')",
        ] {
            sqlx::query(query).execute(pool).await.unwrap();
        }
    }

    test_with_database! {
        async fn run_changes_files_skips_other_packages(test_db: TestDatabase) {
            setup_debian_test_database(test_db.pool()).await.unwrap();
            insert_build(test_db.pool()).await;
            let dir = TempDir::new().unwrap();
            let own = write_changes(dir.path(), "hello");
            write_changes(dir.path(), "other");

            let found = run_changes_files(test_db.pool(), &RunId::new("run-1").unwrap(), dir.path(), false)
                .await
                .unwrap();
            assert_eq!(found, vec![own]);
        }
    }

    test_with_database! {
        async fn run_changes_files_without_debian_build_is_empty(test_db: TestDatabase) {
            setup_debian_test_database(test_db.pool()).await.unwrap();
            let dir = TempDir::new().unwrap();
            write_changes(dir.path(), "hello");

            let found = run_changes_files(test_db.pool(), &RunId::new("run-1").unwrap(), dir.path(), false)
                .await
                .unwrap();
            assert_eq!(found, Vec::<PathBuf>::new());
        }
    }
}
