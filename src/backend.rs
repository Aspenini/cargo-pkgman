use cargo_update::ops::{self, CargoConfig, RegistryPackage};

use std::{collections::BTreeMap, env, io, path::PathBuf, process::Command};

#[derive(Debug, Clone)]
pub struct PackageUpdate {
    pub name: String,
    pub installed: String,
    pub available: String,
    pub registry_name: String,
}

fn cargo_home() -> Result<PathBuf, String> {
    if let Some(path) = env::var_os("CARGO_HOME") {
        return Ok(PathBuf::from(path));
    }

    home::home_dir()
        .map(|home| home.join(".cargo"))
        .ok_or_else(|| "could not determine Cargo home directory".to_string())
}

fn crates_file() -> Result<PathBuf, String> {
    let cargo_home = cargo_home()?;
    Ok(ops::crates_file_in(&cargo_home))
}

pub fn installed_packages() -> Result<Vec<RegistryPackage>, String> {
    let crates_file = crates_file()?;
    Ok(ops::installed_registry_packages(&crates_file))
}

pub fn available_updates() -> Result<Vec<PackageUpdate>, String> {
    let cargo_home = cargo_home()?;
    let crates_file = ops::crates_file_in(&cargo_home);

    let cargo_config = CargoConfig::load(&crates_file);
    let proxy = ops::find_proxy(&crates_file);

    let mut packages = ops::installed_registry_packages(&crates_file);

    if packages.is_empty() {
        return Ok(Vec::new());
    }

    /*
     * Group installed packages by their source registry.
     *
     * cargo-update can handle both the traditional git index and
     * Cargo's sparse registry protocol.
     */
    let mut groups: BTreeMap<String, Vec<usize>> = BTreeMap::new();

    for (index, package) in packages.iter().enumerate() {
        groups
            .entry(package.registry.to_string())
            .or_default()
            .push(index);
    }

    /*
     * Store the Cargo registry name corresponding to each package.
     *
     * get_index_url() resolves Cargo source replacements and gives us:
     *
     *     (registry URL, sparse?, Cargo registry name)
     */
    let mut registry_names: Vec<Option<String>> = vec![None; packages.len()];

    for (registry, package_indices) in groups {
        let (registry_url, sparse, registry_name) = ops::get_index_url(
            &crates_file,
            &registry,
            cargo_config.registries_crates_io_protocol_sparse,
        )
        .map_err(|error| format!("could not resolve registry '{registry}': {error}"))?;

        let index_path =
            ops::assert_index_path(&cargo_home, &registry_url, sparse).map_err(|error| {
                format!(
                    "could not locate registry index '{}': {error}",
                    registry_url
                )
            })?;

        let mut repo = ops::open_index_repository(&index_path, sparse)
            .map_err(|(_, error)| format!("could not open registry '{}': {error}", registry_url))?;

        let package_names: Vec<String> = package_indices
            .iter()
            .map(|&index| packages[index].name.clone())
            .collect();

        /*
         * Refresh the registry.
         *
         * For sparse registries cargo-update only fetches information
         * for the package names we give it.
         *
         * Authentication is None for 0.2.0, so public registries work.
         * Authenticated private registries can come later.
         */
        ops::update_index(
            &mut repo,
            &registry_url,
            package_names.iter(),
            proxy.as_deref(),
            cargo_config.net_git_fetch_with_cli,
            &cargo_config.http,
            None,
            &mut io::sink(),
        )
        .map_err(|error| format!("failed to update registry '{}': {error}", registry_url))?;

        let tree = ops::parse_registry_head(&repo)
            .map_err(|error| format!("failed to read registry '{}': {error}", registry_url))?;

        for &index in &package_indices {
            let package = &mut packages[index];

            let found = package.pull_version(&tree, &repo, None, None);

            if found {
                registry_names[index] = Some(registry_name.to_string());
            }
        }
    }

    let mut updates = Vec::new();

    for (index, package) in packages.into_iter().enumerate() {
        if !package.needs_update(None, None, false) {
            continue;
        }

        let Some(installed) = package.version.as_ref() else {
            continue;
        };

        let Some(available) = package.update_to_version() else {
            continue;
        };

        updates.push(PackageUpdate {
            name: package.name.clone(),
            installed: installed.to_string(),
            available: available.to_string(),

            registry_name: registry_names[index]
                .clone()
                .unwrap_or_else(|| "crates-io".to_string()),
        });
    }

    updates.sort_by(|a, b| a.name.cmp(&b.name));

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

    for update in &updates {
        println!(
            ":: upgrading {} {} -> {}",
            update.name, update.installed, update.available
        );

        let crates_file = crates_file()?;

        let install_root = crates_file
            .parent()
            .ok_or_else(|| "could not determine Cargo install root".to_string())?;

        let status = Command::new("cargo")
            .arg("install")
            .arg("--root")
            .arg(install_root)
            .arg("--force")
            .arg("--version")
            .arg(format!("={}", update.available))
            .arg("--registry")
            .arg(&update.registry_name)
            .arg(&update.name)
            .status()
            .map_err(|error| {
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
