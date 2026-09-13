//! Installed-package records, update detection, and upgrades.
//!
//! Install metadata is read from Cargo's `.crates.toml` (name, version, source)
//! and overlaid with `.crates2.json` (features, bins, profile, target) so an
//! upgrade can replay the original `cargo install` flags.

use std::fs;
use std::path::{Path, PathBuf};
use std::process::Command;
use std::thread;

use semver::Version;

use crate::error::Error;
use crate::registry::{self, IndexCache, Registries, Registry};

/// Registry index lookups are independent, so they run concurrently over a
/// shared connection pool. Cargo itself keeps this small to avoid flooding
/// the crates.io CDN.
const MAX_WORKERS: usize = 2;

/// How a crate was installed, parsed from a `.crates.toml` key.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum PackageSource {
    /// `registry+` or `sparse+` (can be queried for updates).
    Registry {
        /// The raw source URL including protocol prefix.
        raw: String,
    },
    /// `git+` — listed, but not queried.
    Git {
        /// The raw source specifier.
        raw: String,
    },
    /// `path+` — listed, but not queried.
    Path {
        /// The raw source specifier.
        raw: String,
    },
    /// Anything else Cargo recorded.
    Other {
        /// The raw source specifier.
        raw: String,
    },
}

impl PackageSource {
    /// True when this source has a registry index we might query.
    pub fn is_registry(&self) -> bool {
        matches!(self, Self::Registry { .. })
    }

    /// The raw source string as Cargo stored it.
    pub fn raw(&self) -> &str {
        match self {
            Self::Registry { raw }
            | Self::Git { raw }
            | Self::Path { raw }
            | Self::Other { raw } => raw,
        }
    }

    /// Short tag for `list` output, or `None` for a registry package.
    pub fn list_tag(&self) -> Option<&'static str> {
        match self {
            Self::Registry { .. } => None,
            Self::Git { .. } => Some("git"),
            Self::Path { .. } => Some("path"),
            Self::Other { .. } => Some("other"),
        }
    }
}

/// Feature/bin flags recorded in `.crates2.json` for a single install.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct InstallOptions {
    /// Features passed to `--features`.
    pub features: Vec<String>,
    /// Whether `--all-features` was used.
    pub all_features: bool,
    /// Whether `--no-default-features` was used.
    pub no_default_features: bool,
    /// Binary names passed to `--bin`. Empty means install every binary.
    pub bins: Vec<String>,
    /// Cargo profile (`release`, `dev`, …).
    pub profile: Option<String>,
    /// `--target` triple, if any.
    pub target: Option<String>,
    /// Original version requirement, if any.
    pub version_req: Option<String>,
}

impl InstallOptions {
    /// Extra `cargo install` arguments that reproduce this install.
    pub fn cargo_args(&self) -> Vec<String> {
        let mut args = Vec::new();
        if self.no_default_features {
            args.push("--no-default-features".into());
        }
        if self.all_features {
            args.push("--all-features".into());
        }
        if !self.features.is_empty() {
            args.push("--features".into());
            args.push(self.features.join(","));
        }
        for bin in &self.bins {
            args.push("--bin".into());
            args.push(bin.clone());
        }
        match self.profile.as_deref() {
            None | Some("release") => {}
            Some("dev") | Some("debug") => args.push("--debug".into()),
            Some(profile) => {
                args.push("--profile".into());
                args.push(profile.to_string());
            }
        }
        if let Some(target) = &self.target {
            args.push("--target".into());
            args.push(target.clone());
        }
        args
    }
}

/// A crate Cargo recorded as installed.
#[derive(Debug, Clone)]
pub struct InstalledPackage {
    /// Crate name.
    pub name: String,
    /// Installed version.
    pub version: Version,
    /// Where it was installed from.
    pub source: PackageSource,
    /// Original `cargo install` flags, if `.crates2.json` had them.
    pub install: InstallOptions,
}

/// An installed crate that has a newer publishable version.
#[derive(Debug, Clone)]
pub struct PackageUpdate {
    /// Crate name.
    pub name: String,
    /// Currently installed version.
    pub installed: Version,
    /// Newest non-yanked version on the registry.
    pub available: Version,
    /// `--registry` name, or `None` for crates.io.
    pub registry: Option<String>,
    /// Flags to replay on `cargo install`.
    pub install: InstallOptions,
}

/// Options for [`upgrade_all`].
#[derive(Debug, Clone, Default)]
pub struct UpgradeOptions {
    /// Print planned upgrades without running `cargo install`.
    pub dry_run: bool,
    /// Pass `--locked` to `cargo install`.
    pub locked: bool,
}

fn crates_dir(registries: &Registries) -> Result<PathBuf, Error> {
    Ok(registries.install_root(&registry::cargo_home()?))
}

/// Packages Cargo recorded in `.crates.toml` / `.crates2.json`.
///
/// Keys look like
/// `"just 1.58.0 (registry+https://github.com/rust-lang/crates.io-index)"`.
pub fn installed_packages() -> Result<Vec<InstalledPackage>, Error> {
    let registries = Registries::load()?;
    installed_packages_in(&crates_dir(&registries)?)
}

/// Parse install records from an explicit directory (the Cargo install root).
pub fn installed_packages_in(root: &Path) -> Result<Vec<InstalledPackage>, Error> {
    let mut packages = Vec::new();

    let toml_path = root.join(".crates.toml");
    if toml_path.is_file() {
        let text = fs::read_to_string(&toml_path)
            .map_err(|error| Error::io(format!("could not read {}", toml_path.display()), error))?;
        packages = parse_crates_toml(&text)?;
    }

    let json_path = root.join(".crates2.json");
    if json_path.is_file() {
        let text = fs::read_to_string(&json_path)
            .map_err(|error| Error::io(format!("could not read {}", json_path.display()), error))?;
        overlay_crates2(&mut packages, &text)?;
    }

    packages.sort_by(|left, right| left.name.cmp(&right.name));
    Ok(packages)
}

/// Parse the `[v1]` table of `.crates.toml`.
pub fn parse_crates_toml(text: &str) -> Result<Vec<InstalledPackage>, Error> {
    let table: toml::Table = text
        .parse()
        .map_err(|error| Error::Config(format!("could not parse .crates.toml: {error}")))?;

    let Some(installs) = table.get("v1").and_then(toml::Value::as_table) else {
        return Ok(Vec::new());
    };

    Ok(installs.keys().filter_map(|key| parse_key(key)).collect())
}

fn parse_key(key: &str) -> Option<InstalledPackage> {
    let (head, source) = key.rsplit_once(" (")?;
    let source = source.strip_suffix(')')?;
    let (name, version) = head.rsplit_once(' ')?;

    Some(InstalledPackage {
        name: name.to_string(),
        version: Version::parse(version).ok()?,
        source: classify_source(source),
        install: InstallOptions::default(),
    })
}

fn classify_source(raw: &str) -> PackageSource {
    let owned = raw.to_string();
    if raw.starts_with("registry+") || raw.starts_with("sparse+") {
        PackageSource::Registry { raw: owned }
    } else if raw.starts_with("git+") {
        PackageSource::Git { raw: owned }
    } else if raw.starts_with("path+") {
        PackageSource::Path { raw: owned }
    } else {
        PackageSource::Other { raw: owned }
    }
}

/// Overlay `.crates2.json` install flags onto packages parsed from `.crates.toml`.
///
/// Entries that are only in the JSON file are appended.
pub fn overlay_crates2(packages: &mut Vec<InstalledPackage>, text: &str) -> Result<(), Error> {
    let value: serde_json::Value = serde_json::from_str(text)
        .map_err(|error| Error::Config(format!("could not parse .crates2.json: {error}")))?;

    let Some(installs) = value.get("installs").and_then(|value| value.as_object()) else {
        return Ok(());
    };

    for (key, record) in installs {
        let Some(mut parsed) = parse_key(key) else {
            continue;
        };
        parsed.install = install_options_from_value(record);

        if let Some(existing) = packages.iter_mut().find(|package| {
            package.name == parsed.name
                && package.version == parsed.version
                && package.source.raw() == parsed.source.raw()
        }) {
            existing.install = parsed.install;
        } else {
            packages.push(parsed);
        }
    }

    Ok(())
}

fn install_options_from_value(value: &serde_json::Value) -> InstallOptions {
    fn strings(value: &serde_json::Value, key: &str) -> Vec<String> {
        value
            .get(key)
            .and_then(|value| value.as_array())
            .map(|array| {
                array
                    .iter()
                    .filter_map(|value| value.as_str().map(str::to_owned))
                    .collect()
            })
            .unwrap_or_default()
    }

    InstallOptions {
        features: strings(value, "features"),
        all_features: value
            .get("all_features")
            .and_then(serde_json::Value::as_bool)
            .unwrap_or(false),
        no_default_features: value
            .get("no_default_features")
            .and_then(serde_json::Value::as_bool)
            .unwrap_or(false),
        bins: strings(value, "bins"),
        profile: value
            .get("profile")
            .and_then(serde_json::Value::as_str)
            .map(str::to_owned),
        target: value
            .get("target")
            .and_then(serde_json::Value::as_str)
            .filter(|target| !target.is_empty())
            .map(str::to_owned),
        version_req: value
            .get("version_req")
            .and_then(serde_json::Value::as_str)
            .map(str::to_owned),
    }
}

/// Query configured registries for newer versions of installed crates.
pub fn available_updates() -> Result<Vec<PackageUpdate>, Error> {
    let packages = installed_packages()?;
    available_updates_for(packages)
}

fn available_updates_for(packages: Vec<InstalledPackage>) -> Result<Vec<PackageUpdate>, Error> {
    if packages.is_empty() {
        return Ok(Vec::new());
    }

    let skipped = packages
        .iter()
        .filter(|package| !package.source.is_registry())
        .count();
    if skipped > 0 {
        eprintln!(
            "warning: skipped {skipped} package{} not from a registry (git/path)",
            if skipped == 1 { "" } else { "s" }
        );
    }

    let home = registry::cargo_home()?;
    let registries = Registries::load()?;
    let cache = IndexCache::new(&home);

    let mut targets = Vec::new();
    let mut problems = Vec::new();

    for package in packages {
        let PackageSource::Registry { raw } = &package.source else {
            continue;
        };
        match registries.resolve(raw) {
            Ok(registry) => targets.push((package, registry)),
            Err(error) => problems.push(format!("{}: {error}", package.name)),
        }
    }

    // Nothing left to ask about means every registry package failed to resolve,
    // which is a failed run rather than an empty one.
    if targets.is_empty() {
        return match problems.first() {
            Some(problem) => Err(Error::Registry(problem.clone())),
            None => Ok(Vec::new()),
        };
    }

    let mut updates = fetch(&registries, &cache, targets, &mut problems)?;

    for problem in &problems {
        eprintln!("warning: {problem}");
    }
    if !problems.is_empty() {
        eprintln!();
    }

    updates.sort_by(|left, right| left.name.cmp(&right.name));
    Ok(updates)
}

fn fetch(
    registries: &Registries,
    cache: &IndexCache,
    targets: Vec<(InstalledPackage, Registry)>,
    problems: &mut Vec<String>,
) -> Result<Vec<PackageUpdate>, Error> {
    if targets.is_empty() {
        return Ok(Vec::new());
    }

    let workers = targets.len().clamp(1, MAX_WORKERS);
    let mut buckets: Vec<Vec<(InstalledPackage, Registry)>> =
        (0..workers).map(|_| Vec::new()).collect();
    for (index, target) in targets.into_iter().enumerate() {
        buckets[index % workers].push(target);
    }

    let agent = registry::agent();

    let results = thread::scope(|scope| -> Result<Vec<_>, Error> {
        let mut handles = Vec::new();

        for bucket in buckets {
            let agent = &agent;
            handles.push(scope.spawn(move || {
                let mut local = Vec::new();
                for (package, registry) in bucket {
                    let prerelease = !package.version.pre.is_empty();
                    let token = registries.token_for(&registry);
                    let result = registry::latest_version(
                        agent,
                        &registry,
                        &package.name,
                        prerelease,
                        token,
                        cache,
                    );
                    local.push((package, registry, result));
                }
                local
            }));
        }

        let mut results = Vec::new();
        for handle in handles {
            match handle.join() {
                Ok(chunk) => results.extend(chunk),
                Err(_) => {
                    return Err(Error::Registry("an update worker panicked".into()));
                }
            }
        }
        Ok(results)
    })?;

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
                install: package.install,
            }),
            Ok(_) => {}
            Err(error) => failures.push(format!("{}: {error}", package.name)),
        }
    }

    // A single unreachable crate is a warning, but if every lookup we attempted
    // failed then the network did, and we should say so rather than claim
    // everything is up to date.
    if failures.len() == attempted {
        return Err(Error::Registry(
            failures
                .into_iter()
                .next()
                .unwrap_or_else(|| "could not reach any registry".to_string()),
        ));
    }

    problems.append(&mut failures);
    Ok(updates)
}

/// Print the upgrade list, or a "up to date" line.
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
        let flags = update.install.cargo_args();
        if flags.is_empty() {
            println!(
                "  {:width$}  {} -> {}",
                update.name,
                update.installed,
                update.available,
                width = width,
            );
        } else {
            println!(
                "  {:width$}  {} -> {}  [{}]",
                update.name,
                update.installed,
                update.available,
                flags.join(" "),
                width = width,
            );
        }
    }
}

/// Arguments that would be passed to `cargo install` to apply `update`.
pub fn upgrade_args(update: &PackageUpdate, locked: bool) -> Vec<String> {
    let mut args = vec![
        "install".into(),
        "--force".into(),
        "--version".into(),
        format!("={}", update.available),
    ];
    if locked {
        args.push("--locked".into());
    }
    if let Some(registry) = &update.registry {
        args.push("--registry".into());
        args.push(registry.clone());
    }
    args.extend(update.install.cargo_args());
    args.push(update.name.clone());
    args
}

/// Upgrade every outdated crate. Failures do not stop the rest of the batch;
/// they are collected and returned at the end.
pub fn upgrade_all(options: UpgradeOptions) -> Result<(), Error> {
    println!("Checking for Cargo package updates...");
    println!();

    let updates = available_updates()?;

    if updates.is_empty() {
        println!("All Cargo packages are up to date.");
        return Ok(());
    }

    print_updates(&updates);
    println!();

    if options.dry_run {
        println!("Dry run; no packages were upgraded.");
        return Ok(());
    }

    let registries = Registries::load()?;
    let install_root = crates_dir(&registries)?;

    let mut succeeded = 0;
    let mut failed = Vec::new();

    for update in &updates {
        println!(
            ":: upgrading {} {} -> {}",
            update.name, update.installed, update.available
        );

        match upgrade_one(update, &install_root, options.locked) {
            Ok(()) => {
                succeeded += 1;
                println!();
            }
            Err(error) => {
                eprintln!("error: {error}");
                eprintln!();
                failed.push(update.name.clone());
            }
        }
    }

    if failed.is_empty() {
        println!(
            "Upgraded {succeeded} package{}.",
            if succeeded == 1 { "" } else { "s" }
        );
        Ok(())
    } else {
        if succeeded > 0 {
            println!(
                "Upgraded {succeeded} package{}; {} failed.",
                if succeeded == 1 { "" } else { "s" },
                failed.len(),
            );
        }
        Err(Error::Upgrade { succeeded, failed })
    }
}

fn upgrade_one(update: &PackageUpdate, install_root: &Path, locked: bool) -> Result<(), Error> {
    let mut command = Command::new("cargo");
    command.args(upgrade_args(update, locked));
    command.arg("--root").arg(install_root);

    let status = command.status().map_err(|error| Error::Cargo {
        command: "install".into(),
        detail: format!("failed to start while upgrading {}: {error}", update.name),
    })?;

    if status.success() {
        Ok(())
    } else {
        Err(Error::Cargo {
            command: "install".into(),
            detail: format!("failed to upgrade {}", update.name),
        })
    }
}

/// Query registries and print available upgrades.
pub fn check_updates() -> Result<Vec<PackageUpdate>, Error> {
    println!("Checking Cargo registries...");
    println!();

    let updates = available_updates()?;
    print_updates(&updates);
    Ok(updates)
}

#[cfg(test)]
mod tests {
    use super::*;

    const CRATES_TOML: &str = r#"
[v1]
"ripgrep 14.1.0 (registry+https://github.com/rust-lang/crates.io-index)" = ["rg"]
"foo 0.1.0 (git+https://github.com/x/foo#abc)" = ["foo"]
"bar 0.2.0 (path+file:///tmp/bar)" = ["bar"]
"#;

    const CRATES2: &str = r#"
{
  "installs": {
    "ripgrep 14.1.0 (registry+https://github.com/rust-lang/crates.io-index)": {
      "version_req": null,
      "bins": ["rg"],
      "features": ["pcre2"],
      "all_features": false,
      "no_default_features": true,
      "profile": "release",
      "target": null,
      "rustc": "1.86.0"
    }
  }
}
"#;

    #[test]
    fn parse_registry_git_and_path() {
        let packages = parse_crates_toml(CRATES_TOML).unwrap();
        assert_eq!(packages.len(), 3);
        assert!(
            packages
                .iter()
                .any(|package| { package.name == "ripgrep" && package.source.is_registry() })
        );
        assert!(packages.iter().any(|package| {
            package.name == "foo" && matches!(package.source, PackageSource::Git { .. })
        }));
        assert!(packages.iter().any(|package| {
            package.name == "bar" && matches!(package.source, PackageSource::Path { .. })
        }));
    }

    #[test]
    fn crates2_overlay_preserves_features() {
        let mut packages = parse_crates_toml(CRATES_TOML).unwrap();
        overlay_crates2(&mut packages, CRATES2).unwrap();
        let ripgrep = packages
            .iter()
            .find(|package| package.name == "ripgrep")
            .unwrap();
        assert!(ripgrep.install.no_default_features);
        assert_eq!(ripgrep.install.features, ["pcre2"]);
        assert_eq!(ripgrep.install.bins, ["rg"]);
    }

    #[test]
    fn upgrade_args_replay_install_flags() {
        let update = PackageUpdate {
            name: "ripgrep".into(),
            installed: Version::parse("14.1.0").unwrap(),
            available: Version::parse("14.1.1").unwrap(),
            registry: None,
            install: InstallOptions {
                features: vec!["pcre2".into()],
                no_default_features: true,
                bins: vec!["rg".into()],
                ..InstallOptions::default()
            },
        };
        let args = upgrade_args(&update, true);
        assert_eq!(
            args,
            [
                "install",
                "--force",
                "--version",
                "=14.1.1",
                "--locked",
                "--no-default-features",
                "--features",
                "pcre2",
                "--bin",
                "rg",
                "ripgrep",
            ]
        );
    }

    #[test]
    fn list_tag_for_non_registry() {
        assert_eq!(
            PackageSource::Git {
                raw: "git+https://example.com".into()
            }
            .list_tag(),
            Some("git")
        );
        assert_eq!(
            PackageSource::Registry {
                raw: "registry+https://example.com".into()
            }
            .list_tag(),
            None
        );
    }

    #[test]
    fn installed_packages_in_temp_root() {
        let dir = tempfile::tempdir().unwrap();
        fs::write(dir.path().join(".crates.toml"), CRATES_TOML).unwrap();
        fs::write(dir.path().join(".crates2.json"), CRATES2).unwrap();
        let packages = installed_packages_in(dir.path()).unwrap();
        assert_eq!(packages.len(), 3);
        let ripgrep = packages
            .iter()
            .find(|package| package.name == "ripgrep")
            .unwrap();
        assert_eq!(ripgrep.install.features, ["pcre2"]);
    }
}
