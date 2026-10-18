//! Create the chroot tarballs that sbuild uses in unshare mode.

use clap::Parser;
use janitor::config::{Config, Distribution};
use std::path::{Path, PathBuf};

#[derive(Parser)]
struct Args {
    /// Include specified package
    #[clap(long)]
    include: Vec<String>,

    /// Base directory for chroots [default: ~/.cache/sbuild]
    #[clap(long)]
    base_directory: Option<PathBuf>,

    /// Path to configuration
    #[clap(long, default_value = "janitor.conf")]
    config: PathBuf,

    /// Distributions to create chroots for (default: all)
    distribution: Vec<String>,

    #[command(flatten)]
    logging: janitor::logging::LoggingArgs,
}

/// Build distributions of the campaigns that build on a distribution.
fn build_distributions(config: &Config, distribution: &str) -> Vec<String> {
    config
        .campaign
        .iter()
        .filter(|c| c.has_debian_build())
        .map(|c| c.debian_build())
        .filter(|b| b.base_distribution() == distribution)
        .map(|b| b.build_distribution().to_string())
        .collect()
}

/// The mmdebstrap command that creates the tarball for a distribution.
fn mmdebstrap_command(
    distribution: &Distribution,
    tarball: &Path,
    arch: &str,
    include: &[String],
) -> Vec<String> {
    let mirror = distribution.archive_mirror_uri();
    let mut cmd = vec![
        "mmdebstrap".to_string(),
        "--variant=buildd".to_string(),
        distribution.name().to_string(),
        tarball.display().to_string(),
        mirror.to_string(),
        "--mode=unshare".to_string(),
        format!("--arch={}", arch),
        format!("--components={}", distribution.component.join(",")),
    ];
    if !include.is_empty() {
        cmd.push(format!("--include={}", include.join(",")));
    }
    for extra in &distribution.extra {
        cmd.push(format!(
            "--extra-repository=deb {} {} {}",
            mirror,
            extra,
            distribution.component.join(" ")
        ));
    }
    cmd
}

/// Point `<suite>-<arch>-sbuild.tar.xz` at the tarball for each build distribution.
fn link_suites(tarball: &Path, suites: &[String], arch: &str) -> std::io::Result<()> {
    let dir = tarball.parent().unwrap();
    let target = tarball.file_name().unwrap();
    for suite in suites {
        let link = dir.join(format!("{}-{}-sbuild.tar.xz", suite, arch));
        if link.is_symlink() {
            std::fs::remove_file(&link)?;
        }
        std::os::unix::fs::symlink(target, &link)?;
    }
    Ok(())
}

fn run(command: &[String]) -> Result<(), String> {
    log::info!(
        "{}",
        shlex::try_join(command.iter().map(|s| s.as_str())).unwrap()
    );
    let status = std::process::Command::new(&command[0])
        .args(&command[1..])
        .status()
        .map_err(|e| format!("unable to run {}: {}", command[0], e))?;
    if !status.success() {
        return Err(format!("{} failed: {}", command[0], status));
    }
    Ok(())
}

fn build_arch() -> Result<String, String> {
    let output = std::process::Command::new("dpkg-architecture")
        .arg("-qDEB_BUILD_ARCH")
        .output()
        .map_err(|e| format!("unable to run dpkg-architecture: {}", e))?;
    if !output.status.success() {
        return Err(format!("dpkg-architecture failed: {}", output.status));
    }
    Ok(String::from_utf8_lossy(&output.stdout).trim().to_string())
}

fn create(args: &Args) -> Result<(), String> {
    let config = janitor::config::read_file(&args.config)
        .map_err(|e| format!("unable to read {}: {}", args.config.display(), e))?;
    let names: Vec<String> = if args.distribution.is_empty() {
        config
            .distribution
            .iter()
            .map(|d| d.name().to_string())
            .collect()
    } else {
        args.distribution.clone()
    };
    let base_directory = match &args.base_directory {
        Some(dir) => dir.clone(),
        None => std::env::var_os("HOME")
            .map(|home| Path::new(&home).join(".cache/sbuild"))
            .ok_or("HOME is not set; pass --base-directory")?,
    };
    std::fs::create_dir_all(&base_directory)
        .map_err(|e| format!("{}: {}", base_directory.display(), e))?;
    let arch = build_arch()?;
    for name in names {
        let distribution = config
            .get_distribution(&name)
            .ok_or_else(|| format!("no such distribution: {}", name))?;
        let filename = format!("{}.tar.xz", distribution.chroot());
        let tarball = base_directory.join(&filename);
        // Build next to the old tarball and replace it once complete.
        let scratch = tempfile::tempdir_in(&base_directory).map_err(|e| e.to_string())?;
        let new = scratch.path().join(&filename);
        run(&mmdebstrap_command(
            distribution,
            &new,
            &arch,
            &args.include,
        ))?;
        std::fs::rename(&new, &tarball).map_err(|e| format!("{}: {}", tarball.display(), e))?;
        link_suites(&tarball, &build_distributions(&config, &name), &arch)
            .map_err(|e| format!("{}: {}", base_directory.display(), e))?;
    }
    Ok(())
}

fn main() -> std::process::ExitCode {
    let args = Args::parse();
    args.logging.init();
    match create(&args) {
        Ok(()) => std::process::ExitCode::SUCCESS,
        Err(e) => {
            log::error!("{}", e);
            std::process::ExitCode::FAILURE
        }
    }
}

