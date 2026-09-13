//! Shared body of the frontend launchers.
//!
//! This module is included directly by each `src/bin/cargo-*.rs` with
//! `#[path]`, so a launcher never links the main crate and stays a few
//! hundred kilobytes instead of carrying its own copy of the registry and
//! update machinery.

use std::{
    env, io,
    path::PathBuf,
    process::{Command, ExitCode},
};

const EXECUTABLE: &str = "cargo-pkgman";

/// Re-invoke cargo-pkgman with the frontend selected, forwarding every
/// argument after `--` untouched so operations such as `-Syu` survive.
pub fn launch(frontend: &str) -> ExitCode {
    let mut command = Command::new(executable());

    command.arg("--frontend").arg(frontend).arg("--");
    command.args(env::args_os().skip(1));

    match run(&mut command) {
        Ok(code) => code,

        Err(error) => {
            eprintln!("error: could not run {EXECUTABLE}: {error}");
            eprintln!("note: reinstall it with `cargo install cargo-pkgman`");

            // 127 is the conventional "command not found" status.
            ExitCode::from(127)
        }
    }
}

/// Cargo installs every binary of this package into the same directory, so
/// prefer the sibling executable and only fall back to PATH.
fn executable() -> PathBuf {
    let file_name = format!("{EXECUTABLE}{}", env::consts::EXE_SUFFIX);

    if let Ok(current) = env::current_exe() {
        if let Some(directory) = current.parent() {
            let sibling = directory.join(&file_name);

            if sibling.is_file() {
                return sibling;
            }
        }
    }

    PathBuf::from(file_name)
}

#[cfg(unix)]
fn run(command: &mut Command) -> io::Result<ExitCode> {
    use std::os::unix::process::CommandExt;

    // Replace this process so signals and the exit status reach the caller
    // without a launcher sitting in between.
    //
    // exec() only returns when it fails.
    Err(command.exec())
}

#[cfg(not(unix))]
fn run(command: &mut Command) -> io::Result<ExitCode> {
    // Windows has no `exec`. The launcher waits for cargo-pkgman and forwards
    // its status. Ctrl+C is delivered to the process group, so both processes
    // may observe it; when the child exits normally we still return its code.
    let status = command.status()?;

    let code = status
        .code()
        .and_then(|code| u8::try_from(code).ok())
        .unwrap_or(1);

    Ok(ExitCode::from(code))
}
