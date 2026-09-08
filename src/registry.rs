/*
 * Sparse registry index client.
 *
 * Cargo has used the sparse protocol for crates.io since Rust 1.70, so
 * an index lookup is a plain HTTPS GET and we do not need a git client.
 */

use std::{
    collections::BTreeMap,
    env, fs,
    path::{Path, PathBuf},
    time::Duration,
};

use semver::Version;

const CRATES_IO_GIT: &str = "https://github.com/rust-lang/crates.io-index";
const CRATES_IO_SPARSE: &str = "https://index.crates.io/";
const CRATES_IO_NAME: &str = "crates-io";

const USER_AGENT: &str = concat!("cargo-pkgman/", env!("CARGO_PKG_VERSION"));

pub fn cargo_home() -> Result<PathBuf, String> {
    if let Some(path) = env::var_os("CARGO_HOME") {
        return Ok(PathBuf::from(path));
    }

    home::home_dir()
        .map(|home| home.join(".cargo"))
        .ok_or_else(|| "could not determine Cargo home directory".to_string())
}

/*
 * A registry we know how to query.
 *
 * `name` is what `cargo install --registry` expects, and is None for
 * crates.io so that we leave Cargo on its default.
 */
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Registry {
    pub name: Option<String>,
    pub index: String,
}

impl Registry {
    fn crates_io() -> Self {
        Self {
            name: None,
            index: CRATES_IO_SPARSE.to_string(),
        }
    }
}

/*
 * The parts of Cargo's configuration that decide which index answers
 * for a given package: alternate registries and source replacement.
 */
#[derive(Debug, Default)]
pub struct Registries {
    /* registry name -> index URL, still carrying any sparse+ prefix */
    indices: BTreeMap<String, String>,

    /* source name -> replace-with target */
    replacements: BTreeMap<String, String>,

    /* index URL without prefix -> source name */
    sources: BTreeMap<String, String>,
}

impl Registries {
    pub fn load(cargo_home: &Path) -> Result<Self, String> {
        let mut registries = Self::default();

        registries
            .sources
            .insert(CRATES_IO_GIT.to_string(), CRATES_IO_NAME.to_string());

        registries.sources.insert(
            trim_protocol(CRATES_IO_SPARSE).to_string(),
            CRATES_IO_NAME.to_string(),
        );

        /*
         * Cargo accepts config.toml and the older extensionless config.
         */
        let Some(text) = read_config(cargo_home)? else {
            return Ok(registries);
        };

        let config: toml::Table = text
            .parse()
            .map_err(|error| format!("could not parse Cargo config: {error}"))?;

        if let Some(table) = config.get("registries").and_then(toml::Value::as_table) {
            for (name, entry) in table {
                let Some(index) = entry.get("index").and_then(toml::Value::as_str) else {
                    continue;
                };

                registries.indices.insert(name.clone(), index.to_string());

                registries
                    .sources
                    .insert(trim_protocol(index).to_string(), name.clone());
            }
        }

        if let Some(table) = config.get("source").and_then(toml::Value::as_table) {
            for (name, entry) in table {
                if let Some(target) = entry.get("replace-with").and_then(toml::Value::as_str) {
                    registries
                        .replacements
                        .insert(name.clone(), target.to_string());
                }

                if let Some(index) = entry.get("registry").and_then(toml::Value::as_str) {
                    registries.indices.insert(name.clone(), index.to_string());

                    registries
                        .sources
                        .insert(trim_protocol(index).to_string(), name.clone());
                }
            }
        }

        Ok(registries)
    }

    /*
     * Resolve the source recorded in .crates.toml, such as
     *
     *     registry+https://github.com/rust-lang/crates.io-index
     *
     * into the index we should ask about updates.
     */
    pub fn resolve(&self, source: &str) -> Result<Registry, String> {
        let url = trim_protocol(source);

        let name = self
            .sources
            .get(url)
            .cloned()
            .unwrap_or_else(|| url.to_string());

        let name = self.follow_replacements(name)?;

        if name == CRATES_IO_NAME {
            return Ok(Registry::crates_io());
        }

        let Some(index) = self.indices.get(&name) else {
            /*
             * An unreplaced source we have no configuration for. If it
             * is already sparse we can still query it directly.
             */
            return sparse_registry(Some(name.clone()), &name, url);
        };

        sparse_registry(Some(name), index, url)
    }

    fn follow_replacements(&self, mut name: String) -> Result<String, String> {
        /*
         * Replacement chains are short in practice; the bound is only
         * here so a cycle in the config cannot hang us.
         */
        for _ in 0..16 {
            match self.replacements.get(&name) {
                Some(target) => name = target.clone(),
                None => return Ok(name),
            }
        }

        Err(format!("source replacement for '{name}' is circular"))
    }
}

fn sparse_registry(name: Option<String>, index: &str, source: &str) -> Result<Registry, String> {
    if trim_protocol(index) == CRATES_IO_GIT
        || trim_protocol(index) == trim_protocol(CRATES_IO_SPARSE)
    {
        return Ok(Registry::crates_io());
    }

    if !index.starts_with("sparse+") {
        return Err(format!(
            "registry '{source}' uses a git index, which cargo-pkgman cannot query"
        ));
    }

    let mut index = trim_protocol(index).to_string();

    if !index.ends_with('/') {
        index.push('/');
    }

    Ok(Registry { name, index })
}

fn trim_protocol(url: &str) -> &str {
    url.strip_prefix("registry+")
        .or_else(|| url.strip_prefix("sparse+"))
        .unwrap_or(url)
}

fn read_config(cargo_home: &Path) -> Result<Option<String>, String> {
    for name in ["config.toml", "config"] {
        let path = cargo_home.join(name);

        if !path.is_file() {
            continue;
        }

        return fs::read_to_string(&path)
            .map(Some)
            .map_err(|error| format!("could not read {}: {error}", path.display()));
    }

    Ok(None)
}

pub fn agent() -> ureq::Agent {
    ureq::Agent::config_builder()
        .timeout_global(Some(Duration::from_secs(30)))
        .user_agent(USER_AGENT)
        .build()
        .into()
}

/*
 * Index files are laid out by name length:
 *
 *     ju/st/just, 3/b/bat, 2/ab, 1/a
 */
fn index_path(name: &str) -> String {
    let name = name.to_lowercase();

    /*
     * Crate names are ASCII, so byte offsets are character offsets.
     * Anything else cannot name a real crate; let the fetch 404.
     */
    if !name.is_ascii() {
        return name;
    }

    match name.len() {
        1 => format!("1/{name}"),
        2 => format!("2/{name}"),
        3 => format!("3/{}/{}", &name[..1], name),
        _ => format!("{}/{}/{}", &name[..2], &name[2..4], name),
    }
}

/*
 * Highest publishable version of a crate, or None when the registry
 * does not have it.
 *
 * Prereleases are only considered when the installed version is itself
 * a prerelease, matching what Cargo does on upgrade.
 */
pub fn latest_version(
    agent: &ureq::Agent,
    registry: &Registry,
    name: &str,
    prerelease: bool,
) -> Result<Option<Version>, String> {
    let url = format!("{}{}", registry.index, index_path(name));

    let mut response = match agent.get(&url).call() {
        Ok(response) => response,

        Err(ureq::Error::StatusCode(404)) => return Ok(None),

        Err(error) => return Err(format!("{error}")),
    };

    let body = response
        .body_mut()
        .read_to_string()
        .map_err(|error| format!("{error}"))?;

    let mut best: Option<Version> = None;

    for line in body.lines() {
        if line.trim().is_empty() {
            continue;
        }

        let entry: serde_json::Value = serde_json::from_str(line)
            .map_err(|error| format!("malformed index entry: {error}"))?;

        if entry["yanked"].as_bool().unwrap_or(false) {
            continue;
        }

        let Some(raw) = entry["vers"].as_str() else {
            continue;
        };

        let Ok(version) = Version::parse(raw) else {
            continue;
        };

        if !version.pre.is_empty() && !prerelease {
            continue;
        }

        if best.as_ref().is_none_or(|best| version > *best) {
            best = Some(version);
        }
    }

    Ok(best)
}
