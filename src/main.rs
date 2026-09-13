//! `cargo-pkgman` — shared executable for every package-manager frontend.

use std::process::ExitCode;

fn main() -> ExitCode {
    cargo_pkgman::run()
}
