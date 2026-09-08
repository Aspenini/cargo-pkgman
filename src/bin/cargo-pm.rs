use std::process::ExitCode;

fn main() -> ExitCode {
    cargo_pkgman::run(cargo_pkgman::Dialect::Native)
}
