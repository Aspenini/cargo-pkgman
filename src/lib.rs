//! Package-manager style frontends for Cargo-installed binaries.
//!
//! All of the logic — argument parsing, registry handling, and update
//! detection — lives in this library. The `cargo-pkgman` binary is a thin
//! wrapper around [`run`]. The `cargo-pacman`, `cargo-apt`, `cargo-dnf`, and
//! `cargo-pm` binaries are tiny launchers that re-invoke `cargo-pkgman` with
//! `--frontend <name>` so the registry and update machinery is linked once.
//!
//! # Frontends
//!
//! | Subcommand | Style |
//! |---|---|
//! | `cargo pm` / `cargo pkgman` | Native commands (`install`, `upgrade`, …) |
//! | `cargo pacman` | Arch Linux `pacman` clustered flags (`-Syu`) |
//! | `cargo apt` | Debian/Ubuntu `apt` |
//! | `cargo dnf` | Fedora `dnf` |
//!
//! # Upgrades
//!
//! Update checks hit each crate's sparse index over HTTPS (no git client).
//! Alternate registries and Cargo source replacement are honoured. An upgrade
//! replays the original `cargo install` flags recorded in `.crates2.json`
//! (features, bins, profile, target).

#![deny(missing_docs)]

use std::env;
use std::process::{Command, ExitCode};

mod backend;
mod cli;
mod error;
mod registry;

pub use backend::{
    InstallOptions, InstalledPackage, PackageSource, PackageUpdate, UpgradeOptions,
    available_updates, check_updates, installed_packages, overlay_crates2, parse_crates_toml,
    print_updates, upgrade_all, upgrade_args,
};
pub use cli::{Dialect, Operation, parse, preprocess, print_help, take_frontend};
pub use error::Error;
pub use registry::{
    CRATES_IO_NAME, IndexCache, Registries, Registry, agent, cargo_home, index_path,
    latest_version, trim_protocol,
};

/// Outcome of a successful [`run_with`] call.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Outcome {
    Success,
    /// `dnf check-update` exits 100 when upgrades are available.
    UpdatesAvailable,
}

/// Collect UTF-8 command-line arguments and run the selected frontend.
pub fn run() -> ExitCode {
    match collect_args() {
        Ok(args) => run_with(args),
        Err(error) => print_error(&error, None),
    }
}

fn collect_args() -> Result<Vec<String>, Error> {
    let mut args = Vec::new();
    for arg in env::args_os().skip(1) {
        match arg.into_string() {
            Ok(arg) => args.push(arg),
            Err(_) => return Err(Error::Utf8),
        }
    }
    Ok(args)
}

/// Run with an explicit argument list (the process name is not included).
pub fn run_with(args: Vec<String>) -> ExitCode {
    let (dialect, rest) = match preprocess(args) {
        Ok(parsed) => parsed,
        Err(error) => return print_error(&error, None),
    };

    let operation = match parse(dialect, &rest) {
        Ok(operation) => operation,
        Err(error) => {
            eprintln!("error: {error}");
            eprintln!();
            print_help(dialect);
            return error.exit_code();
        }
    };

    match execute(dialect, operation) {
        Ok(Outcome::Success) => ExitCode::SUCCESS,
        Ok(Outcome::UpdatesAvailable) => ExitCode::from(100),
        Err(error) => print_error(&error, Some(dialect)),
    }
}

fn print_error(error: &Error, dialect: Option<Dialect>) -> ExitCode {
    eprintln!("error: {error}");
    if matches!(error, Error::Usage(_)) {
        if let Some(dialect) = dialect {
            eprintln!();
            print_help(dialect);
        }
    }
    error.exit_code()
}

fn execute(dialect: Dialect, operation: Operation) -> Result<Outcome, Error> {
    match operation {
        Operation::Install {
            packages,
            cargo_args,
            dry_run,
        } => {
            run_cargo("install", &cargo_args, &packages, dry_run)?;
            Ok(Outcome::Success)
        }

        Operation::Remove {
            packages,
            cargo_args,
            dry_run,
        } => {
            run_cargo("uninstall", &cargo_args, &packages, dry_run)?;
            Ok(Outcome::Success)
        }

        Operation::CheckUpdates => {
            let updates = backend::check_updates()?;
            if dialect == Dialect::Dnf && !updates.is_empty() {
                Ok(Outcome::UpdatesAvailable)
            } else {
                Ok(Outcome::Success)
            }
        }

        Operation::Upgrade {
            dry_run,
            locked,
            also_install,
            install_args,
        } => {
            backend::upgrade_all(UpgradeOptions { dry_run, locked })?;
            if !also_install.is_empty() {
                run_cargo("install", &install_args, &also_install, dry_run)?;
            }
            Ok(Outcome::Success)
        }

        Operation::Search(query) => {
            search_crates(&query)?;
            Ok(Outcome::Success)
        }

        Operation::Info(packages) => {
            info_packages(&packages)?;
            Ok(Outcome::Success)
        }

        Operation::List { query } => {
            list_packages(query.as_deref())?;
            Ok(Outcome::Success)
        }

        Operation::Help => {
            print_help(dialect);
            Ok(Outcome::Success)
        }
    }
}

fn run_cargo(
    command: &str,
    extra: &[String],
    packages: &[String],
    dry_run: bool,
) -> Result<(), Error> {
    if dry_run {
        let mut shown = vec!["cargo".to_string(), command.to_string()];
        shown.extend(extra.iter().cloned());
        shown.extend(packages.iter().cloned());
        println!("Would run: {}", shown.join(" "));
        return Ok(());
    }

    let mut process = Command::new("cargo");
    process.arg(command);
    process.args(extra);
    process.args(packages);

    let status = process.status().map_err(|error| Error::Cargo {
        command: command.into(),
        detail: error.to_string(),
    })?;

    if status.success() {
        Ok(())
    } else {
        Err(Error::Cargo {
            command: command.into(),
            detail: "command failed".into(),
        })
    }
}

fn search_crates(query: &str) -> Result<(), Error> {
    let mut arguments = vec!["search".to_string(), query.to_string()];
    if let Ok(registries) = Registries::load() {
        if let Some(name) = registries
            .default_registry()
            .filter(|name| *name != CRATES_IO_NAME)
        {
            arguments.push("--registry".into());
            arguments.push(name.to_string());
        }
    }
    let status = Command::new("cargo")
        .args(&arguments)
        .status()
        .map_err(|error| Error::Cargo {
            command: "search".into(),
            detail: error.to_string(),
        })?;
    if status.success() {
        Ok(())
    } else {
        Err(Error::Cargo {
            command: "search".into(),
            detail: "command failed".into(),
        })
    }
}

fn info_packages(packages: &[String]) -> Result<(), Error> {
    let installed = backend::installed_packages().unwrap_or_default();
    let registries = Registries::load().ok();

    for package in packages {
        let mut arguments = vec!["info".to_string(), package.clone()];
        if let Some(registry) = installed_registry_name(&installed, registries.as_ref(), package) {
            arguments.push("--registry".into());
            arguments.push(registry);
        }

        let status = Command::new("cargo")
            .args(&arguments)
            .status()
            .map_err(|error| Error::Cargo {
                command: "info".into(),
                detail: error.to_string(),
            })?;

        if !status.success() {
            return Err(Error::Cargo {
                command: "info".into(),
                detail: format!("failed for {package}"),
            });
        }
    }

    Ok(())
}

fn installed_registry_name(
    installed: &[InstalledPackage],
    registries: Option<&Registries>,
    name: &str,
) -> Option<String> {
    let package = installed.iter().find(|package| package.name == name)?;
    let PackageSource::Registry { raw } = &package.source else {
        return None;
    };
    registries?.resolve(raw).ok()?.name
}

fn list_packages(query: Option<&[String]>) -> Result<(), Error> {
    let packages = backend::installed_packages()?;

    let selected: Vec<&InstalledPackage> = if let Some(names) = query {
        let mut selected = Vec::new();
        for name in names {
            match packages.iter().find(|package| package.name == *name) {
                Some(package) => selected.push(package),
                None => return Err(Error::NotInstalled(name.clone())),
            }
        }
        selected
    } else {
        packages.iter().collect()
    };

    let width = selected
        .iter()
        .map(|package| package.name.len())
        .max()
        .unwrap_or(0);

    for package in selected {
        match package.source.list_tag() {
            Some(tag) => println!(
                "{:width$} {} ({tag})",
                package.name,
                package.version,
                width = width
            ),
            None => println!("{:width$} {}", package.name, package.version, width = width),
        }
    }

    Ok(())
}
