//! Error types returned by cargo-pkgman.

use std::error::Error as StdError;
use std::fmt;
use std::io;
use std::process::ExitCode;

/// A recoverable failure from parsing, configuration, the network, or Cargo.
#[derive(Debug)]
pub enum Error {
    /// An I/O failure, with a short description of what was being done.
    Io {
        /// Human-readable context, for example `could not read ~/.cargo/.crates.toml`.
        context: String,
        /// The underlying I/O error.
        source: io::Error,
    },
    /// Invalid CLI usage. The process should exit with status 2.
    Usage(String),
    /// A command-line argument was not valid UTF-8.
    Utf8,
    /// Cargo configuration could not be read or was invalid.
    Config(String),
    /// A registry index lookup failed.
    Registry(String),
    /// A spawned `cargo` process failed to start or returned a non-success status.
    Cargo {
        /// The cargo subcommand, for example `install` or `info`.
        command: String,
        /// Extra detail (spawn error or `failed for <package>`).
        detail: String,
    },
    /// An upgrade run finished with one or more packages that failed to install.
    Upgrade {
        /// Packages that upgraded successfully.
        succeeded: usize,
        /// Package names that failed.
        failed: Vec<String>,
    },
    /// The named crate is not in the install records.
    NotInstalled(String),
}

impl Error {
    /// Wrap an I/O error with context.
    pub fn io(context: impl Into<String>, source: io::Error) -> Self {
        Self::Io {
            context: context.into(),
            source,
        }
    }

    /// Usage errors print help and exit 2; everything else exits 1.
    pub fn exit_code(&self) -> ExitCode {
        if matches!(self, Self::Usage(_) | Self::Utf8) {
            ExitCode::from(2)
        } else {
            ExitCode::FAILURE
        }
    }
}

impl fmt::Display for Error {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Io { context, source } => write!(f, "{context}: {source}"),
            Self::Usage(message) | Self::Config(message) | Self::Registry(message) => {
                f.write_str(message)
            }
            Self::Utf8 => f.write_str("argument is not valid UTF-8"),
            Self::Cargo { command, detail } => write!(f, "cargo {command}: {detail}"),
            Self::Upgrade { succeeded, failed } => {
                write!(
                    f,
                    "upgraded {succeeded} package{}; failed: {}",
                    if *succeeded == 1 { "" } else { "s" },
                    failed.join(", "),
                )
            }
            Self::NotInstalled(name) => write!(f, "package '{name}' is not installed"),
        }
    }
}

impl StdError for Error {
    fn source(&self) -> Option<&(dyn StdError + 'static)> {
        match self {
            Self::Io { source, .. } => Some(source),
            _ => None,
        }
    }
}
