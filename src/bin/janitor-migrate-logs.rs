//! Move the log files of all runs from one log store to another.

use clap::Parser;
use futures::StreamExt;
use janitor::logs::{get_log_manager, Error as LogError, LogFileManager};
use std::io::Write;
use std::num::NonZeroUsize;
use std::process::ExitCode;

const WORKER_LOG_FILENAME: &str = "worker.log";
const BUILD_LOG_FILENAME: &str = "build.log";

#[derive(Parser)]
#[command(about = "Move run logs from one log store to another")]
struct Args {
    #[clap(long, default_value = "janitor.conf")]
    /// Path to configuration.
    config: std::path::PathBuf,

    #[clap(long)]
    /// List what would be moved, but don't change anything.
    dry_run: bool,

    #[clap(long)]
    /// Copy the logs, leaving them in place in the source.
    keep: bool,

    #[clap(long, default_value = "100")]
    /// Number of runs to process concurrently.
    concurrency: NonZeroUsize,

    #[clap(value_parser = clap::builder::NonEmptyStringValueParser::new())]
    /// Location to move logs from.
    from_location: String,

    #[clap(value_parser = clap::builder::NonEmptyStringValueParser::new())]
    /// Location to move logs to.
    to_location: String,

    #[clap(flatten)]
    logging: janitor::logging::LoggingArgs,
}

#[derive(Debug, Clone, Copy, Default)]
struct Options {
    dry_run: bool,
    keep: bool,
}

#[derive(Debug)]
enum Error {
    /// An operation on a specific log file failed.
    Log {
        action: &'static str,
        name: String,
        error: LogError,
    },
    Io(std::io::Error),
    Database(sqlx::Error),
}

impl std::fmt::Display for Error {
    fn fmt(&self, f: &mut std::fmt::Formatter) -> std::fmt::Result {
        match self {
            Error::Log {
                action,
                name,
                error,
            } => write!(f, "failed to {} {}: {}", action, name, error),
            Error::Io(e) => write!(f, "I/O error: {}", e),
            Error::Database(e) => write!(f, "database error: {}", e),
        }
    }
}

impl std::error::Error for Error {}

impl From<std::io::Error> for Error {
    fn from(e: std::io::Error) -> Self {
        Error::Io(e)
    }
}

impl From<sqlx::Error> for Error {
    fn from(e: sqlx::Error) -> Self {
        Error::Database(e)
    }
}

fn log_error<'a>(action: &'static str, name: &'a str) -> impl FnOnce(LogError) -> Error + 'a {
    move |error| Error::Log {
        action,
        name: name.to_string(),
        error,
    }
}

async fn has_log(
    manager: &dyn LogFileManager,
    codebase: &str,
    run_id: &str,
    name: &str,
) -> Result<bool, Error> {
    manager
        .has_log(codebase, run_id, name)
        .await
        .map_err(log_error("check for", name))
}

/// Find the names of the logs that exist for a run.
async fn discover_log_names(
    manager: &dyn LogFileManager,
    codebase: &str,
    run_id: &str,
) -> Result<Vec<String>, Error> {
    let mut names = Vec::new();
    for name in [WORKER_LOG_FILENAME, BUILD_LOG_FILENAME] {
        if has_log(manager, codebase, run_id, name).await? {
            names.push(name.to_string());
        }
    }
    for i in 1.. {
        let name = format!("{}.{}", BUILD_LOG_FILENAME, i);
        if !has_log(manager, codebase, run_id, &name).await? {
            break;
        }
        names.push(name);
    }
    Ok(names)
}

/// The logs of a run that were moved, and those left where they were.
#[derive(Debug, Default, PartialEq, Eq)]
struct Outcome {
    moved: Vec<String>,
    skipped: Vec<String>,
}

/// Move the named logs of a run, leaving those the destination already has.
async fn migrate_run_logs(
    from_manager: &dyn LogFileManager,
    to_manager: &dyn LogFileManager,
    codebase: &str,
    run_id: &str,
    names: &[String],
    options: Options,
) -> Result<Outcome, Error> {
    let mut outcome = Outcome::default();
    for name in names {
        if has_log(to_manager, codebase, run_id, name).await? {
            // Never import over, or remove, a log the destination has.
            if has_log(from_manager, codebase, run_id, name).await? {
                log::info!(
                    "{} of {} is already in the destination, leaving it",
                    name,
                    run_id
                );
                outcome.skipped.push(name.clone());
            }
            continue;
        }
        if options.dry_run {
            if has_log(from_manager, codebase, run_id, name).await? {
                outcome.moved.push(name.clone());
            }
            continue;
        }
        let mut log = match from_manager.get_log(codebase, run_id, name).await {
            Ok(log) => log,
            Err(LogError::NotFound) => continue,
            Err(e) => return Err(log_error("fetch", name)(e)),
        };
        let tmp = tokio::task::spawn_blocking(move || -> std::io::Result<_> {
            let mut tmp = tempfile::NamedTempFile::new()?;
            std::io::copy(&mut log, &mut tmp)?;
            tmp.flush()?;
            Ok(tmp)
        })
        .await
        .map_err(std::io::Error::other)??;
        let path = tmp.path().to_str().ok_or_else(|| {
            std::io::Error::other(format!("non-UTF-8 temporary path {:?}", tmp.path()))
        })?;
        to_manager
            .import_log(codebase, run_id, path, None, Some(name))
            .await
            .map_err(log_error("import", name))?;
        if !options.keep {
            from_manager
                .delete_log(codebase, run_id, name)
                .await
                .map_err(log_error("delete", name))?;
        }
        outcome.moved.push(name.clone());
    }
    Ok(outcome)
}

/// Process a single run, recording its log names if they were not known.
async fn process_run(
    pool: &sqlx::PgPool,
    from_manager: &dyn LogFileManager,
    to_manager: &dyn LogFileManager,
    codebase: &str,
    run_id: &str,
    logfilenames: Option<Vec<String>>,
    options: Options,
) -> Result<Outcome, Error> {
    let logfilenames = match logfilenames {
        Some(names) => names,
        None => {
            let names = discover_log_names(from_manager, codebase, run_id).await?;
            if !options.dry_run {
                sqlx::query("UPDATE run SET logfilenames = $1 WHERE id = $2")
                    .bind(&names)
                    .bind(run_id)
                    .execute(pool)
                    .await?;
            }
            names
        }
    };
    let outcome = migrate_run_logs(
        from_manager,
        to_manager,
        codebase,
        run_id,
        &logfilenames,
        options,
    )
    .await?;
    if options.dry_run {
        log::info!("Would process {} ({:?})", run_id, outcome.moved);
    } else {
        log::info!("Processed {} ({:?})", run_id, outcome.moved);
    }
    Ok(outcome)
}

/// Process all runs; returns the number of runs, failed runs and skipped logs.
async fn process_all_runs(
    pool: &sqlx::PgPool,
    from_manager: &dyn LogFileManager,
    to_manager: &dyn LogFileManager,
    concurrency: usize,
    options: Options,
) -> Result<(usize, usize, usize), Error> {
    let rows: Vec<(String, String, Option<Vec<String>>)> =
        sqlx::query_as("SELECT codebase, id, logfilenames FROM run")
            .fetch_all(pool)
            .await?;
    let total = rows.len();
    let (failed, skipped) = futures::stream::iter(rows)
        .map(|(codebase, run_id, logfilenames)| async move {
            match process_run(
                pool,
                from_manager,
                to_manager,
                &codebase,
                &run_id,
                logfilenames,
                options,
            )
            .await
            {
                Ok(outcome) => (0, outcome.skipped.len()),
                Err(e) => {
                    log::error!("Error processing run {}: {}", run_id, e);
                    (1, 0)
                }
            }
        })
        .buffer_unordered(concurrency)
        .fold((0, 0), |(failed, skipped), (f, s)| {
            std::future::ready((failed + f, skipped + s))
        })
        .await;
    Ok((total, failed, skipped))
}

#[tokio::main]
async fn main() -> ExitCode {
    let args = Args::parse();

    args.logging.init();

    let config = match janitor::config::read_file(&args.config) {
        Ok(config) => config,
        Err(e) => {
            log::error!("Unable to read config {}: {}", args.config.display(), e);
            return ExitCode::FAILURE;
        }
    };

    let from_manager = match get_log_manager(Some(&args.from_location)).await {
        Ok(manager) => manager,
        Err(e) => {
            log::error!("Unable to open {}: {}", args.from_location, e);
            return ExitCode::FAILURE;
        }
    };
    let to_manager = match get_log_manager(Some(&args.to_location)).await {
        Ok(manager) => manager,
        Err(e) => {
            log::error!("Unable to open {}: {}", args.to_location, e);
            return ExitCode::FAILURE;
        }
    };

    let pool = match janitor::state::create_pool(&config).await {
        Ok(pool) => pool,
        Err(e) => {
            log::error!("Unable to connect to database: {}", e);
            return ExitCode::FAILURE;
        }
    };

    let options = Options {
        dry_run: args.dry_run,
        keep: args.keep,
    };
    let (total, failed, skipped) = match process_all_runs(
        &pool,
        from_manager.as_ref(),
        to_manager.as_ref(),
        args.concurrency.get(),
        options,
    )
    .await
    {
        Ok(counts) => counts,
        Err(e) => {
            log::error!("Error: {}", e);
            return ExitCode::FAILURE;
        }
    };
    if skipped > 0 {
        log::info!("Left {} logs that are already in the destination", skipped);
    }
    if failed > 0 {
        log::error!("Failed to process {} of {} runs", failed, total);
        ExitCode::FAILURE
    } else {
        log::info!("Processed {} runs", total);
        ExitCode::SUCCESS
    }
}
