use std::{
    fs,
    path::PathBuf,
    process::Command,
    sync::{Arc, Mutex},
    thread,
};

use semver::Version;

use crate::registry::{self, Registries, Registry};

/*
 * Registry index lookups are independent, so they run concurrently over
 * a shared connection pool. Beyond a handful of workers the crates.io
 * CDN, not us, is the limit.
 */
const MAX_WORKERS: usize = 8;

#[derive(Debug, Clone)]
pub struct InstalledPackage {
    pub name: String,
    pub version: Version,
    pub source: String,
}

#[derive(Debug, Clone)]
pub struct PackageUpdate {
    pub name: String,
    pub installed: Version,
    pub available: Version,
    pub registry: Option<String>,
}

fn crates_file() -> Result<PathBuf, String> {
    Ok(registry::cargo_home()?.join(".crates.toml"))
}

/*
 * Packages Cargo recorded in .crates.toml, keyed as
 *
 *     "just 1.58.0 (registry+https://github.com/rust-lang/crates.io-index)"
 *
 * Packages installed from a path or a git checkout have no registry to
 * ask about updates, so they are left out.
 */
pub fn installed_packages() -> Result<Vec<InstalledPackage>, String> {
    let path = crates_file()?;

    if !path.is_file() {
        return Ok(Vec::new());
    }

    let text = fs::read_to_string(&path)
        .map_err(|error| format!("could not read {}: {error}", path.display()))?;

    let table: toml::Table = text
        .parse()
        .map_err(|error| format!("could not parse {}: {error}", path.display()))?;

    let Some(installs) = table.get("v1").and_then(toml::Value::as_table) else {
        return Ok(Vec::new());
    };

    let mut packages: Vec<InstalledPackage> =
        installs.keys().filter_map(|key| parse(key)).collect();

    packages.sort_by(|a, b| a.name.cmp(&b.name));

    Ok(packages)
}

fn parse(key: &str) -> Option<InstalledPackage> {
    let (head, source) = key.rsplit_once(" (")?;
    let source = source.strip_suffix(')')?;

    if !source.starts_with("registry+") && !source.starts_with("sparse+") {
        return None;
    }

    let (name, version) = head.rsplit_once(' ')?;

    Some(InstalledPackage {
        name: name.to_string(),
        version: Version::parse(version).ok()?,
        source: source.to_string(),
    })
}

pub fn available_updates() -> Result<Vec<PackageUpdate>, String> {
    let packages = installed_packages()?;

    if packages.is_empty() {
        return Ok(Vec::new());
    }

    let registries = Registries::load(&registry::cargo_home()?)?;

    /*
     * Resolve every package to an index before going to the network, so
     * a misconfigured registry is reported once rather than per fetch.
     */
    let mut targets = Vec::new();
    let mut problems = Vec::new();

    for package in packages {
        match registries.resolve(&package.source) {
            Ok(registry) => targets.push((package, registry)),
            Err(error) => problems.push(format!("{}: {error}", package.name)),
        }
    }

    /*
     * Nothing left to ask about means every package failed to resolve,
     * which is a failed run rather than an empty one.
     */
    if targets.is_empty() {
        return match problems.first() {
            Some(problem) => Err(problem.clone()),
            None => Ok(Vec::new()),
        };
    }

    let mut updates = fetch(targets, &mut problems)?;

    for problem in &problems {
        eprintln!("warning: {problem}");
    }

    if !problems.is_empty() {
        eprintln!();
    }

    updates.sort_by(|a, b| a.name.cmp(&b.name));

    Ok(updates)
}

fn fetch(
    targets: Vec<(InstalledPackage, Registry)>,
    problems: &mut Vec<String>,
) -> Result<Vec<PackageUpdate>, String> {
    if targets.is_empty() {
        return Ok(Vec::new());
    }

    let workers = targets.len().min(MAX_WORKERS);

    let agent = registry::agent();
    let queue = Arc::new(Mutex::new(targets.into_iter()));
    let found = Arc::new(Mutex::new(Vec::new()));

    thread::scope(|scope| {
        for _ in 0..workers {
            let agent = &agent;
            let queue = Arc::clone(&queue);
            let found = Arc::clone(&found);

            scope.spawn(move || {
                loop {
                    /*
                     * Hold the queue lock only long enough to take the
                     * next package, never across a request.
                     */
                    let next = queue.lock().unwrap().next();

                    let Some((package, registry)) = next else {
                        break;
                    };

                    let prerelease = !package.version.pre.is_empty();

                    let result =
                        registry::latest_version(agent, &registry, &package.name, prerelease);

                    found.lock().unwrap().push((package, registry, result));
                }
            });
        }
    });

    let results = Arc::try_unwrap(found)
        .map_err(|_| "internal error: update workers outlived the fetch".to_string())?
        .into_inner()
        .map_err(|_| "internal error: an update worker panicked".to_string())?;

    let attempted = results.len();

    let mut updates = Vec::new();
    let mut failures = Vec::new();

    for (package, registry, result) in results {
        match result {
            Ok(Some(available)) if available > package.version => updates.push(PackageUpdate {
                name: package.name,
                installed: package.version,
                available,
                registry: registry.name,
            }),

            Ok(_) => {}

            Err(error) => failures.push(format!("{}: {error}", package.name)),
        }
    }

    /*
     * A single unreachable crate is a warning, but if every lookup we
     * attempted failed then the network did, and we should say so
     * rather than claim everything is up to date.
     */
    if failures.len() == attempted {
        return Err(failures
            .into_iter()
            .next()
            .unwrap_or_else(|| "could not reach any registry".to_string()));
    }

    problems.append(&mut failures);

    Ok(updates)
}

pub fn print_updates(updates: &[PackageUpdate]) {
    if updates.is_empty() {
        println!("All Cargo packages are up to date.");
        return;
    }

    println!(
        "{} package{} can be upgraded:",
        updates.len(),
        if updates.len() == 1 { "" } else { "s" }
    );

    println!();

    let width = updates
        .iter()
        .map(|update| update.name.len())
        .max()
        .unwrap_or(0);

    for update in updates {
        println!(
            "  {:width$}  {} -> {}",
            update.name,
            update.installed,
            update.available,
            width = width,
        );
    }
}

pub fn upgrade_all() -> Result<(), String> {
    println!("Checking for Cargo package updates...");
    println!();

    let updates = available_updates()?;

    if updates.is_empty() {
        println!("All Cargo packages are up to date.");
        return Ok(());
    }

    print_updates(&updates);

    println!();

    let crates_file = crates_file()?;

    let install_root = crates_file
        .parent()
        .ok_or_else(|| "could not determine Cargo install root".to_string())?;

    for update in &updates {
        println!(
            ":: upgrading {} {} -> {}",
            update.name, update.installed, update.available
        );

        let mut command = Command::new("cargo");

        command
            .arg("install")
            .arg("--root")
            .arg(install_root)
            .arg("--force")
            .arg("--version")
            .arg(format!("={}", update.available));

        if let Some(registry) = &update.registry {
            command.arg("--registry").arg(registry);
        }

        let status = command.arg(&update.name).status().map_err(|error| {
            format!(
                "failed to start Cargo while upgrading {}: {error}",
                update.name
            )
        })?;

        if !status.success() {
            return Err(format!("failed to upgrade {}", update.name));
        }

        println!();
    }

    println!(
        "Upgraded {} package{}.",
        updates.len(),
        if updates.len() == 1 { "" } else { "s" }
    );

    Ok(())
}

pub fn check_updates() -> Result<(), String> {
    println!("Checking Cargo registries...");
    println!();

    let updates = available_updates()?;

    print_updates(&updates);

    Ok(())
}
