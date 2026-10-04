//! Clean up owned repositories that are no longer needed for merge proposals.
//!
//! This is necessary in particular because some hosting sites
//! (e.g. default GitLab) have restrictions on the number of repositories
//! that a single user can own (in the case of GitLab, 1000).

use clap::Parser;
use pyo3::prelude::*;
use std::collections::HashSet;
use std::process::ExitCode;

#[derive(Parser)]
struct Args {
    #[clap(long)]
    /// Only report what would be deleted.
    dry_run: bool,

    #[clap(flatten)]
    logging: janitor::logging::LoggingArgs,
}

/// The ways a forge can fail that this tool tells apart.
#[derive(Debug, PartialEq, Eq)]
enum Error {
    /// The forge does not support the operation.
    Unsupported(String),
    /// The forge has no credentials, or a dependency of it is missing.
    Unavailable(String),
    /// The project does not exist.
    NoSuchProject(String),
    /// Anything else.
    Other(String),
}

impl std::fmt::Display for Error {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Error::NoSuchProject(project) => write!(f, "No such project: {}", project),
            Error::Unsupported(msg) | Error::Unavailable(msg) | Error::Other(msg) => {
                write!(f, "{}", msg)
            }
        }
    }
}

/// Access to the projects and merge proposals of one forge instance.
trait ForgeProjects {
    /// Name of the forge instance, for messages.
    fn name(&self) -> String;

    /// Name of the logged in user, if any.
    fn current_user(&self) -> Result<Option<String>, Error>;

    /// Source project of every proposal that is neither closed nor merged.
    fn open_proposal_sources(&self) -> Result<Vec<Result<Option<String>, Error>>, Error>;

    /// Forks owned by the current user.
    fn my_forks(&self) -> Result<Vec<String>, Error>;

    /// Delete a project.
    fn delete_project(&self, project: &str) -> Result<(), Error>;
}

/// What happened on one forge instance.
#[derive(Debug, PartialEq, Eq)]
enum Outcome {
    /// The forge was processed; `removed` lists what was (or would be) deleted.
    Cleaned {
        removed: Vec<String>,
        failed: Vec<String>,
    },
    /// The forge does not support this or is not available; that is not a failure.
    Skipped,
    /// The projects in use could not be determined, so nothing was deleted.
    Failed,
}

impl Outcome {
    fn is_failure(&self) -> bool {
        match self {
            Outcome::Cleaned { failed, .. } => !failed.is_empty(),
            Outcome::Skipped => false,
            Outcome::Failed => true,
        }
    }
}

fn is_unsupported(e: &Error) -> bool {
    matches!(e, Error::Unsupported(_))
}

fn is_skippable(e: &Error) -> bool {
    matches!(e, Error::Unsupported(_) | Error::Unavailable(_))
}

/// Projects that back a live proposal; a source project that is gone protects nothing.
fn in_use_projects(sources: Vec<Result<Option<String>, Error>>) -> Result<HashSet<String>, Error> {
    let mut in_use = HashSet::new();
    for source in sources {
        match source {
            Ok(Some(project)) => {
                in_use.insert(project);
            }
            Ok(None) | Err(Error::NoSuchProject(_)) => {}
            Err(e) => return Err(e),
        }
    }
    Ok(in_use)
}

/// Own forks that no live proposal uses, in the order the forge lists them.
fn projects_to_remove(forge: &dyn ForgeProjects) -> Result<Vec<String>, Error> {
    let user = forge
        .current_user()?
        .ok_or_else(|| Error::Unavailable("not logged in".to_string()))?;
    let sources = forge.open_proposal_sources()?;
    let live = sources.len();
    let in_use = in_use_projects(sources)?;
    let mut seen = HashSet::new();
    let candidates: Vec<String> = forge
        .my_forks()?
        .into_iter()
        .filter(|project| {
            project
                .split_once('/')
                .is_some_and(|(owner, _)| owner == user)
                && !in_use.contains(project)
                && seen.insert(project.clone())
        })
        .collect();
    if live > 0 && in_use.is_empty() && !candidates.is_empty() {
        return Err(Error::Other(format!(
            "none of the {} open proposals has a source project",
            live
        )));
    }
    Ok(candidates)
}

/// Delete (or with `dry_run` only report) the unused forks on one forge.
fn cleanup_forge(forge: &dyn ForgeProjects, dry_run: bool) -> Outcome {
    let name = forge.name();
    let candidates = match projects_to_remove(forge) {
        Ok(candidates) => candidates,
        Err(e) if is_skippable(&e) => {
            log::warn!("Skipping {}: {}", name, e);
            return Outcome::Skipped;
        }
        Err(e) => {
            log::error!(
                "Unable to determine the projects in use on {}, not deleting anything: {}",
                name,
                e
            );
            return Outcome::Failed;
        }
    };

    let mut removed = Vec::new();
    let mut failed = Vec::new();
    for project in candidates {
        if dry_run {
            log::info!("Would delete {} from {}", project, name);
            removed.push(project);
            continue;
        }
        log::info!("Deleting {} from {}", project, name);
        match forge.delete_project(&project) {
            Ok(()) => removed.push(project),
            Err(e) if is_unsupported(&e) => {
                log::warn!("Skipping {} that can not delete: {}", name, e);
                if failed.is_empty() {
                    return Outcome::Skipped;
                }
                break;
            }
            Err(e) => {
                log::error!("Failed to delete {} from {}: {}", project, name, e);
                failed.push(project);
            }
        }
    }

    log::info!(
        "{}: {} {}, {} failed",
        name,
        removed.len(),
        if dry_run {
            "would be deleted"
        } else {
            "deleted"
        },
        failed.len()
    );
    Outcome::Cleaned { removed, failed }
}

/// Clean up every instance; returns whether anything failed.
fn cleanup_all<F: ForgeProjects>(
    instances: Vec<Result<F, (String, Error)>>,
    dry_run: bool,
) -> bool {
    if instances.is_empty() {
        log::warn!("No forge instances found");
    }
    let mut failed = false;
    for instance in instances {
        match instance {
            Ok(forge) => failed |= cleanup_forge(&forge, dry_run).is_failure(),
            Err((kind, e)) if is_skippable(&e) => {
                log::warn!("Skipping the {} forges: {}", kind, e);
            }
            Err((kind, e)) => {
                log::error!("Unable to list the {} forges: {}", kind, e);
                failed = true;
            }
        }
    }
    failed
}

// Forge access through Python; this can move to breezyshim's own bindings once released.
pyo3::import_exception!(breezy.forge, NoSuchProject);
pyo3::import_exception!(breezy.forge, UnsupportedForge);
pyo3::import_exception!(breezy.errors, DependencyNotPresent);

struct PyForge(Py<PyAny>);

fn py_instances_of(forges: &Bound<'_, PyAny>, kind: &str) -> PyResult<Vec<PyForge>> {
    forges
        .call_method1("get", (kind,))?
        .call_method0("iter_instances")?
        .try_iter()?
        .map(|instance| Ok(PyForge(instance?.unbind())))
        .collect()
}

/// Instances of every kind of forge; a kind that can not be listed does not hide the others.
fn py_registered_instances(forges: &Bound<'_, PyAny>) -> Vec<Result<PyForge, (String, Error)>> {
    let py = forges.py();
    let kinds = forges.call_method0("keys").and_then(|keys| {
        keys.try_iter()?
            .map(|kind| kind?.extract::<String>())
            .collect::<PyResult<Vec<_>>>()
    });
    let kinds = match kinds {
        Ok(kinds) => kinds,
        Err(e) => return vec![Err(("registered".to_string(), py_error(py, e)))],
    };
    let mut instances = Vec::new();
    for kind in kinds {
        match py_instances_of(forges, &kind) {
            Ok(found) => instances.extend(found.into_iter().map(Ok)),
            Err(e) => instances.push(Err((kind, py_error(py, e)))),
        }
    }
    instances
}

fn py_forge_instances() -> Vec<Result<PyForge, (String, Error)>> {
    Python::attach(
        |py| match py.import("breezy.forge").and_then(|m| m.getattr("forges")) {
            Ok(forges) => py_registered_instances(&forges),
            Err(e) => vec![Err(("registered".to_string(), py_error(py, e)))],
        },
    )
}

/// Classify a Python exception without relying on the shape of its attributes.
fn py_error(py: Python<'_>, e: PyErr) -> Error {
    if e.is_instance_of::<NoSuchProject>(py) {
        let project = e
            .value(py)
            .getattr("project")
            .and_then(|p| p.str())
            .map(|p| p.to_string())
            .unwrap_or_default();
        Error::NoSuchProject(project)
    } else if e.is_instance_of::<UnsupportedForge>(py)
        || e.is_instance_of::<pyo3::exceptions::PyNotImplementedError>(py)
    {
        Error::Unsupported(e.to_string())
    } else if e.is_instance_of::<DependencyNotPresent>(py) {
        Error::Unavailable(e.to_string())
    } else {
        Error::Other(e.to_string())
    }
}

fn py_is_live(mp: &Bound<'_, PyAny>) -> PyResult<bool> {
    Ok(
        !mp.call_method0("is_closed")?.is_truthy()?
            && !mp.call_method0("is_merged")?.is_truthy()?,
    )
}

fn py_source_project(mp: &Bound<'_, PyAny>) -> PyResult<Option<String>> {
    mp.call_method0("get_source_project")?.extract()
}

impl ForgeProjects for PyForge {
    fn name(&self) -> String {
        Python::attach(|py| {
            let forge = self.0.bind(py);
            match forge.repr() {
                Ok(repr) => repr.to_string(),
                Err(_) => forge.get_type().to_string(),
            }
        })
    }

    fn current_user(&self) -> Result<Option<String>, Error> {
        Python::attach(|py| {
            self.0
                .bind(py)
                .call_method0("get_current_user")
                .and_then(|user| user.extract())
                .map_err(|e| py_error(py, e))
        })
    }

    fn open_proposal_sources(&self) -> Result<Vec<Result<Option<String>, Error>>, Error> {
        Python::attach(|py| {
            let mut sources = Vec::new();
            let proposals = self
                .0
                .bind(py)
                .call_method0("iter_my_proposals")
                .and_then(|proposals| proposals.try_iter())
                .map_err(|e| py_error(py, e))?;
            for mp in proposals {
                let mp = mp.map_err(|e| py_error(py, e))?;
                if !py_is_live(&mp).map_err(|e| py_error(py, e))? {
                    continue;
                }
                sources.push(py_source_project(&mp).map_err(|e| py_error(py, e)));
            }
            Ok(sources)
        })
    }

    fn my_forks(&self) -> Result<Vec<String>, Error> {
        Python::attach(|py| {
            let mut forks = Vec::new();
            let iter = self
                .0
                .bind(py)
                .call_method0("iter_my_forks")
                .and_then(|forks| forks.try_iter())
                .map_err(|e| py_error(py, e))?;
            for project in iter {
                let project = project
                    .and_then(|project| project.extract::<String>())
                    .map_err(|e| py_error(py, e))?;
                forks.push(project);
            }
            Ok(forks)
        })
    }

    fn delete_project(&self, project: &str) -> Result<(), Error> {
        Python::attach(|py| {
            self.0
                .bind(py)
                .call_method1("delete_project", (project,))
                .map_err(|e| py_error(py, e))?;
            Ok(())
        })
    }
}

fn main() -> ExitCode {
    let args = Args::parse();

    args.logging.init();

    breezyshim::init();
    let _ = breezyshim::plugin::load_plugins();

    if cleanup_all(py_forge_instances(), args.dry_run) {
        ExitCode::FAILURE
    } else {
        ExitCode::SUCCESS
    }
}
