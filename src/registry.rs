//! Sparse registry index client.
//!
//! Cargo has used the sparse protocol for crates.io since Rust 1.70, so an
//! index lookup is a plain HTTPS GET and we do not need a git client.
//!
//! Configuration is loaded the way Cargo does: `$CARGO_HOME/config.toml`,
//! then `.cargo/config.toml` walking from the current directory up, then
//! `CARGO_*` environment variables (highest priority). Private registries
//! authenticate with tokens from `credentials.toml` or `CARGO_REGISTRIES_*_TOKEN`.

use std::collections::BTreeMap;
use std::env;
use std::fs;
use std::path::{Path, PathBuf};
use std::thread;
use std::time::Duration;

use semver::Version;

use crate::error::Error;

const CRATES_IO_GIT: &str = "https://github.com/rust-lang/crates.io-index";
const CRATES_IO_SPARSE: &str = "https://index.crates.io/";
/// The name Cargo uses for crates.io in config and `--registry`.
pub const CRATES_IO_NAME: &str = "crates-io";

const USER_AGENT: &str = concat!("cargo-pkgman/", env!("CARGO_PKG_VERSION"));

const MAX_ATTEMPTS: u32 = 3;
const RETRY_BASE: Duration = Duration::from_millis(200);

/// Directory Cargo uses for configuration, git checkouts, and `cargo install`
/// records, honouring `CARGO_HOME`.
pub fn cargo_home() -> Result<PathBuf, Error> {
    if let Some(path) = env::var_os("CARGO_HOME") {
        return Ok(PathBuf::from(path));
    }

    env::home_dir()
        .map(|home| home.join(".cargo"))
        .ok_or_else(|| Error::Config("could not determine Cargo home directory".into()))
}

/// A registry we know how to query.
///
/// `name` is what `cargo install --registry` expects, and is `None` for
/// crates.io so that we leave Cargo on its default.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Registry {
    /// Registry name for `--registry`, or `None` for crates.io.
    pub name: Option<String>,
    /// Sparse index base URL, including a trailing slash.
    pub index: String,
}

impl Registry {
    fn crates_io() -> Self {
        Self {
            name: None,
            index: CRATES_IO_SPARSE.to_string(),
        }
    }

    /// Token lookup key: `crates-io` when [`Self::name`] is `None`.
    pub fn token_name(&self) -> &str {
        self.name.as_deref().unwrap_or(CRATES_IO_NAME)
    }
}

/// The parts of Cargo's configuration that decide which index answers for a
/// given package: alternate registries, source replacement, install root, and
/// registry tokens.
#[derive(Debug, Default)]
pub struct Registries {
    /// Registry name -> index URL, still carrying any `sparse+` prefix.
    indices: BTreeMap<String, String>,
    /// Source name -> `replace-with` target.
    replacements: BTreeMap<String, String>,
    /// Index URL without prefix -> source name.
    sources: BTreeMap<String, String>,
    /// Registry name -> Authorization header value.
    tokens: BTreeMap<String, String>,
    /// `[registry] default`.
    default_registry: Option<String>,
    /// `registries.crates-io.protocol`, `sparse` or `git`.
    crates_io_protocol: Option<String>,
    /// `[install] root` / `CARGO_INSTALL_ROOT`.
    install_root: Option<PathBuf>,
}

impl Registries {
    /// Load configuration from Cargo home, the current directory walk, and the
    /// environment.
    pub fn load() -> Result<Self, Error> {
        let home = cargo_home()?;
        let cwd = env::current_dir().ok();
        load_internal(&home, cwd.as_deref(), None, true)
    }

    /// Load configuration from an explicit Cargo home, without reading process
    /// environment or walking the real current directory.
    pub fn load_from_home(cargo_home: &Path) -> Result<Self, Error> {
        load_internal(cargo_home, None, None, false)
    }

    /// The `[registry] default` value, if configured.
    pub fn default_registry(&self) -> Option<&str> {
        self.default_registry.as_deref()
    }

    /// Directory that holds `.crates.toml` / `.crates2.json` and is passed as
    /// `--root` to `cargo install`.
    pub fn install_root(&self, cargo_home: &Path) -> PathBuf {
        match &self.install_root {
            Some(root) if root.is_absolute() => root.clone(),
            Some(root) => cargo_home.join(root),
            None => cargo_home.to_path_buf(),
        }
    }

    /// Authorization token for `registry`, if one is configured.
    ///
    /// crates.io's public sparse index is queried without a token.
    pub fn token_for(&self, registry: &Registry) -> Option<&str> {
        registry.name.as_ref()?;
        self.tokens.get(registry.token_name()).map(String::as_str)
    }

    /// Resolve the source recorded in `.crates.toml`, such as
    /// `registry+https://github.com/rust-lang/crates.io-index`, into the index
    /// we should ask about updates.
    pub fn resolve(&self, source: &str) -> Result<Registry, Error> {
        let url = trim_protocol(source);

        let name = self
            .sources
            .get(url)
            .cloned()
            .unwrap_or_else(|| url.to_string());

        let name = self.follow_replacements(name)?;

        if name == CRATES_IO_NAME {
            if self.crates_io_protocol.as_deref() == Some("git") {
                return Err(Error::Registry(
                    "registry 'crates-io' uses a git index, which cargo-pkgman cannot query".into(),
                ));
            }
            return Ok(Registry::crates_io());
        }

        let Some(index) = self.indices.get(&name) else {
            // An unreplaced source we have no configuration for. If it is
            // already sparse we can still query it directly.
            return sparse_registry(Some(name.clone()), &name, url);
        };

        sparse_registry(Some(name), index, url)
    }

    fn follow_replacements(&self, mut name: String) -> Result<String, Error> {
        // Replacement chains are short in practice; the bound is only here so
        // a cycle in the config cannot hang us.
        for _ in 0..16 {
            match self.replacements.get(&name) {
                Some(target) => name = target.clone(),
                None => return Ok(name),
            }
        }

        Err(Error::Config(format!(
            "source replacement for '{name}' is circular"
        )))
    }

    fn apply_toml(&mut self, text: &str, config_dir: Option<&Path>) -> Result<(), Error> {
        let config: toml::Table = text
            .parse()
            .map_err(|error| Error::Config(format!("could not parse Cargo config: {error}")))?;

        if let Some(table) = config.get("registries").and_then(toml::Value::as_table) {
            for (name, entry) in table {
                if let Some(index) = entry.get("index").and_then(toml::Value::as_str) {
                    self.indices.insert(name.clone(), index.to_string());
                    self.sources
                        .insert(trim_protocol(index).to_string(), name.clone());
                }
                if name == CRATES_IO_NAME {
                    if let Some(protocol) = entry.get("protocol").and_then(toml::Value::as_str) {
                        self.crates_io_protocol = Some(protocol.to_string());
                    }
                }
                if let Some(token) = entry.get("token").and_then(toml::Value::as_str) {
                    self.tokens.insert(name.clone(), token.to_string());
                }
            }
        }

        if let Some(table) = config.get("source").and_then(toml::Value::as_table) {
            for (name, entry) in table {
                if let Some(target) = entry.get("replace-with").and_then(toml::Value::as_str) {
                    self.replacements.insert(name.clone(), target.to_string());
                }

                if let Some(index) = entry.get("registry").and_then(toml::Value::as_str) {
                    self.indices.insert(name.clone(), index.to_string());
                    self.sources
                        .insert(trim_protocol(index).to_string(), name.clone());
                }
            }
        }

        if let Some(table) = config.get("registry").and_then(toml::Value::as_table) {
            if let Some(default) = table.get("default").and_then(toml::Value::as_str) {
                self.default_registry = Some(default.to_string());
            }
            if let Some(token) = table.get("token").and_then(toml::Value::as_str) {
                self.tokens
                    .insert(CRATES_IO_NAME.to_string(), token.to_string());
            }
        }

        if let Some(table) = config.get("install").and_then(toml::Value::as_table) {
            if let Some(root) = table.get("root").and_then(toml::Value::as_str) {
                let path = PathBuf::from(root);
                self.install_root = Some(match (path.is_absolute(), config_dir) {
                    (false, Some(dir)) => dir.join(path),
                    _ => path,
                });
            }
        }

        Ok(())
    }

    fn apply_config_dir(&mut self, dir: &Path) -> Result<(), Error> {
        // Cargo prefers the extensionless file when both exist.
        for name in ["config", "config.toml"] {
            let path = dir.join(name);
            if path.is_file() {
                let text = fs::read_to_string(&path).map_err(|error| {
                    Error::io(format!("could not read {}", path.display()), error)
                })?;
                return self.apply_toml(&text, Some(dir));
            }
        }
        Ok(())
    }

    fn apply_credentials(&mut self, cargo_home: &Path) -> Result<(), Error> {
        for name in ["credentials.toml", "credentials"] {
            let path = cargo_home.join(name);
            if !path.is_file() {
                continue;
            }
            let text = fs::read_to_string(&path)
                .map_err(|error| Error::io(format!("could not read {}", path.display()), error))?;
            self.apply_toml(&text, Some(cargo_home))?;
            break;
        }
        Ok(())
    }

    fn apply_env(&mut self) {
        for (key, value) in env::vars() {
            match key.as_str() {
                "CARGO_REGISTRIES_CRATES_IO_PROTOCOL" => {
                    self.crates_io_protocol = Some(value);
                    continue;
                }
                "CARGO_REGISTRY_DEFAULT" => {
                    self.default_registry = Some(value);
                    continue;
                }
                "CARGO_REGISTRY_TOKEN" => {
                    self.tokens.insert(CRATES_IO_NAME.to_string(), value);
                    continue;
                }
                "CARGO_INSTALL_ROOT" => {
                    self.install_root = Some(PathBuf::from(value));
                    continue;
                }
                _ => {}
            }

            if let Some(rest) = key.strip_prefix("CARGO_REGISTRIES_") {
                if let Some(name) = rest.strip_suffix("_INDEX") {
                    let name = env_key_to_name(name);
                    self.sources
                        .insert(trim_protocol(&value).to_string(), name.clone());
                    self.indices.insert(name, value);
                    continue;
                }
                if let Some(name) = rest.strip_suffix("_TOKEN") {
                    self.tokens.insert(env_key_to_name(name), value);
                    continue;
                }
            }

            if let Some(rest) = key.strip_prefix("CARGO_SOURCE_") {
                if let Some(name) = rest.strip_suffix("_REPLACE_WITH") {
                    self.replacements.insert(env_key_to_name(name), value);
                } else if let Some(name) = rest.strip_suffix("_REGISTRY") {
                    let name = env_key_to_name(name);
                    self.sources
                        .insert(trim_protocol(&value).to_string(), name.clone());
                    self.indices.insert(name, value);
                }
            }
        }
    }
}

fn load_internal(
    cargo_home: &Path,
    cwd: Option<&Path>,
    walk_stop: Option<&Path>,
    environ: bool,
) -> Result<Registries, Error> {
    let mut registries = Registries::default();
    registries
        .sources
        .insert(CRATES_IO_GIT.to_string(), CRATES_IO_NAME.to_string());
    registries.sources.insert(
        trim_protocol(CRATES_IO_SPARSE).to_string(),
        CRATES_IO_NAME.to_string(),
    );

    // Lowest priority: $CARGO_HOME/config.toml.
    registries.apply_config_dir(cargo_home)?;

    // Then `.cargo/config.toml` from filesystem root (or `walk_stop`) toward cwd.
    if let Some(cwd) = cwd {
        let mut dirs = Vec::new();
        let mut dir = cwd.to_path_buf();
        loop {
            dirs.push(dir.clone());
            if walk_stop.is_some_and(|stop| dir == stop) {
                break;
            }
            if !dir.pop() {
                break;
            }
        }
        for dir in dirs.iter().rev() {
            registries.apply_config_dir(&dir.join(".cargo"))?;
        }
    }

    registries.apply_credentials(cargo_home)?;

    if environ {
        registries.apply_env();
    }

    Ok(registries)
}

fn env_key_to_name(key: &str) -> String {
    key.to_ascii_lowercase().replace('_', "-")
}

fn sparse_registry(name: Option<String>, index: &str, source: &str) -> Result<Registry, Error> {
    if trim_protocol(index) == CRATES_IO_GIT
        || trim_protocol(index) == trim_protocol(CRATES_IO_SPARSE)
    {
        return Ok(Registry::crates_io());
    }

    if !index.starts_with("sparse+") {
        return Err(Error::Registry(format!(
            "registry '{source}' uses a git index, which cargo-pkgman cannot query"
        )));
    }

    let mut index = trim_protocol(index).to_string();
    if !index.ends_with('/') {
        index.push('/');
    }

    Ok(Registry { name, index })
}

/// Strip a leading `registry+` or `sparse+` protocol prefix.
pub fn trim_protocol(url: &str) -> &str {
    url.strip_prefix("registry+")
        .or_else(|| url.strip_prefix("sparse+"))
        .unwrap_or(url)
}

/// Shared HTTP agent: one connection pool for every index lookup in a run.
pub fn agent() -> ureq::Agent {
    ureq::Agent::config_builder()
        .timeout_global(Some(Duration::from_secs(30)))
        .user_agent(USER_AGENT)
        .build()
        .into()
}

/// Index files are laid out by name length:
///
/// ```text
/// ju/st/just, 3/b/bat, 2/ab, 1/a
/// ```
///
/// Crate names are ASCII, so byte offsets are character offsets. Anything else
/// cannot name a real crate; the fetch is allowed to 404.
pub fn index_path(name: &str) -> String {
    let name = name.to_lowercase();

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

/// On-disk ETag cache under `$CARGO_HOME/pkgman/cache`.
pub struct IndexCache {
    dir: PathBuf,
}

struct Cached {
    etag: Option<String>,
    body: String,
}

impl IndexCache {
    /// Open (but do not necessarily create) the cache directory.
    pub fn new(cargo_home: &Path) -> Self {
        Self {
            dir: cargo_home.join("pkgman").join("cache"),
        }
    }

    fn path_for(&self, url: &str) -> PathBuf {
        self.dir.join(format!("{:016x}.json", fnv1a64(url)))
    }

    fn get(&self, url: &str) -> Option<Cached> {
        let text = fs::read_to_string(self.path_for(url)).ok()?;
        let value: serde_json::Value = serde_json::from_str(&text).ok()?;
        if value["url"].as_str()? != url {
            return None;
        }
        Some(Cached {
            etag: value["etag"].as_str().map(str::to_owned),
            body: value["body"].as_str()?.to_owned(),
        })
    }

    fn put(&self, url: &str, etag: &str, body: &str) {
        if fs::create_dir_all(&self.dir).is_err() {
            return;
        }
        let document = serde_json::json!({
            "url": url,
            "etag": etag,
            "body": body,
        });
        let _ = fs::write(self.path_for(url), document.to_string());
    }
}

fn fnv1a64(data: &str) -> u64 {
    let mut hash = 0xcbf29ce484222325;
    for byte in data.as_bytes() {
        hash ^= u64::from(*byte);
        hash = hash.wrapping_mul(0x100_0000_01b3);
    }
    hash
}

/// Highest publishable version of a crate, or `None` when the registry does
/// not have it.
///
/// Prereleases are only considered when the installed version is itself a
/// prerelease, matching what Cargo does on upgrade.
pub fn latest_version(
    agent: &ureq::Agent,
    registry: &Registry,
    name: &str,
    prerelease: bool,
    token: Option<&str>,
    cache: &IndexCache,
) -> Result<Option<Version>, Error> {
    let url = format!("{}{}", registry.index, index_path(name));
    let Some(body) = get_index_body(agent, &url, token, cache)? else {
        return Ok(None);
    };
    parse_index_body(&body, prerelease)
}

fn parse_index_body(body: &str, prerelease: bool) -> Result<Option<Version>, Error> {
    let mut best: Option<Version> = None;

    for line in body.lines() {
        if line.trim().is_empty() {
            continue;
        }

        let entry: serde_json::Value = serde_json::from_str(line)
            .map_err(|error| Error::Registry(format!("malformed index entry: {error}")))?;

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

fn get_index_body(
    agent: &ureq::Agent,
    url: &str,
    token: Option<&str>,
    cache: &IndexCache,
) -> Result<Option<String>, Error> {
    let cached = cache.get(url);
    let mut last_error = None;

    for attempt in 0..MAX_ATTEMPTS {
        if attempt > 0 {
            thread::sleep(RETRY_BASE.saturating_mul(2u32.pow(attempt - 1)));
        }

        let mut request = agent.get(url).header("Accept", "text/plain");
        if let Some(token) = token {
            request = request.header("Authorization", token);
        }
        if let Some(etag) = cached.as_ref().and_then(|cached| cached.etag.as_deref()) {
            request = request.header("If-None-Match", etag);
        }

        match request.call() {
            Ok(mut response) => {
                let etag = response
                    .headers()
                    .get("ETag")
                    .and_then(|value| value.to_str().ok())
                    .map(str::to_owned);
                let body = response
                    .body_mut()
                    .read_to_string()
                    .map_err(|error| Error::Registry(error.to_string()))?;
                if let Some(etag) = etag.as_deref() {
                    cache.put(url, etag, &body);
                }
                return Ok(Some(body));
            }
            Err(ureq::Error::StatusCode(304)) => {
                if let Some(cached) = cached {
                    return Ok(Some(cached.body));
                }
                last_error = Some(Error::Registry(
                    "received HTTP 304 without a cache entry".into(),
                ));
            }
            Err(ureq::Error::StatusCode(404)) => return Ok(None),
            Err(ureq::Error::StatusCode(401 | 403)) => {
                return Err(Error::Registry(format!(
                    "registry authentication failed for {url}"
                )));
            }
            Err(ureq::Error::StatusCode(code)) if is_retryable_status(code) => {
                last_error = Some(Error::Registry(format!("HTTP {code} from {url}")));
            }
            Err(ureq::Error::StatusCode(code)) => {
                return Err(Error::Registry(format!("HTTP {code} from {url}")));
            }
            Err(error) => {
                last_error = Some(Error::Registry(error.to_string()));
            }
        }
    }

    if let Some(cached) = cached {
        if let Some(error) = &last_error {
            eprintln!("warning: {error}; using cached index");
        }
        return Ok(Some(cached.body));
    }

    Err(last_error.unwrap_or_else(|| Error::Registry(format!("could not reach {url}"))))
}

fn is_retryable_status(code: u16) -> bool {
    matches!(code, 408 | 429 | 500 | 502 | 503 | 504)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Write;

    fn registries_from_toml(text: &str) -> Registries {
        let mut registries = Registries::default();
        registries
            .sources
            .insert(CRATES_IO_GIT.to_string(), CRATES_IO_NAME.to_string());
        registries.sources.insert(
            trim_protocol(CRATES_IO_SPARSE).to_string(),
            CRATES_IO_NAME.to_string(),
        );
        registries.apply_toml(text, None).expect("toml");
        registries
    }

    #[test]
    fn index_path_layout() {
        assert_eq!(index_path("a"), "1/a");
        assert_eq!(index_path("ab"), "2/ab");
        assert_eq!(index_path("bat"), "3/b/bat");
        assert_eq!(index_path("serde"), "se/rd/serde");
        assert_eq!(index_path("RipGrep"), "ri/pg/ripgrep");
    }

    #[test]
    fn trim_protocol_prefixes() {
        assert_eq!(
            trim_protocol("registry+https://github.com/rust-lang/crates.io-index"),
            "https://github.com/rust-lang/crates.io-index"
        );
        assert_eq!(
            trim_protocol("sparse+https://index.crates.io/"),
            "https://index.crates.io/"
        );
        assert_eq!(
            trim_protocol("https://example.com/"),
            "https://example.com/"
        );
    }

    #[test]
    fn crates_io_source_uses_sparse_index() {
        let registries = registries_from_toml("");
        let registry = registries
            .resolve("registry+https://github.com/rust-lang/crates.io-index")
            .unwrap();
        assert_eq!(registry, Registry::crates_io());
    }

    #[test]
    fn source_replacement_to_sparse_mirror() {
        let registries = registries_from_toml(
            r#"
            [source.crates-io]
            replace-with = "mirror"
            [source.mirror]
            registry = "sparse+https://mirror.example/index/"
            "#,
        );
        let registry = registries
            .resolve("registry+https://github.com/rust-lang/crates.io-index")
            .unwrap();
        assert_eq!(registry.index, "https://mirror.example/index/");
        assert_eq!(registry.name.as_deref(), Some("mirror"));
    }

    #[test]
    fn git_index_is_rejected() {
        let registries = registries_from_toml(
            r#"
            [registries.private]
            index = "https://github.com/org/index"
            "#,
        );
        let error = registries
            .resolve("registry+https://github.com/org/index")
            .unwrap_err();
        assert!(error.to_string().contains("git index"));
    }

    #[test]
    fn circular_replacement_is_an_error() {
        let registries = registries_from_toml(
            r#"
            [source.a]
            replace-with = "b"
            [source.b]
            replace-with = "a"
            "#,
        );
        let error = registries.resolve("a").unwrap_err();
        assert!(error.to_string().contains("circular"));
    }

    #[test]
    fn parse_index_skips_yanked_and_prerelease() {
        let body = r#"
{"vers":"1.0.0","yanked":false}
{"vers":"1.1.0","yanked":true}
{"vers":"2.0.0-beta.1","yanked":false}
{"vers":"1.0.1","yanked":false}
"#;
        let version = parse_index_body(body, false).unwrap().unwrap();
        assert_eq!(version, Version::parse("1.0.1").unwrap());

        let version = parse_index_body(body, true).unwrap().unwrap();
        assert_eq!(version, Version::parse("2.0.0-beta.1").unwrap());
    }

    #[test]
    fn cache_roundtrip() {
        let dir = tempfile::tempdir().unwrap();
        let cache = IndexCache::new(dir.path());
        cache.put("https://index.example/ab/cd/abcd", "\"etag1\"", "body");
        let cached = cache.get("https://index.example/ab/cd/abcd").unwrap();
        assert_eq!(cached.etag.as_deref(), Some("\"etag1\""));
        assert_eq!(cached.body, "body");
        assert!(cache.get("https://other.example/crate").is_none());
    }

    #[test]
    fn directory_walk_overrides_home() {
        let root = tempfile::tempdir().unwrap();
        let home = root.path().join("home");
        let project = root.path().join("project");
        fs::create_dir_all(home.join(".unused")).unwrap();
        fs::create_dir_all(project.join(".cargo")).unwrap();

        fs::write(
            home.join("config.toml"),
            r#"
            [registries.foo]
            index = "sparse+https://home.example/index/"
            "#,
        )
        .unwrap();
        fs::write(
            project.join(".cargo").join("config.toml"),
            r#"
            [registries.foo]
            index = "sparse+https://project.example/index/"
            "#,
        )
        .unwrap();

        let registries = load_internal(&home, Some(&project), Some(root.path()), false).unwrap();
        let registry = registries
            .resolve("sparse+https://project.example/index/")
            .unwrap();
        assert_eq!(registry.index, "https://project.example/index/");
    }

    #[test]
    fn credentials_and_install_root() {
        let home = tempfile::tempdir().unwrap();
        fs::write(
            home.path().join("config.toml"),
            r#"
            [install]
            root = "install-root"
            "#,
        )
        .unwrap();
        let mut credentials = fs::File::create(home.path().join("credentials.toml")).unwrap();
        writeln!(
            credentials,
            "[registries.private]\ntoken = \"Bearer secret\"\n"
        )
        .unwrap();

        let registries = Registries::load_from_home(home.path()).unwrap();
        assert_eq!(
            registries.install_root(home.path()),
            home.path().join("install-root")
        );
        assert_eq!(
            registries.tokens.get("private").map(String::as_str),
            Some("Bearer secret")
        );
    }
}
