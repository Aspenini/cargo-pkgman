use std::process::ExitCode;

#[path = "../launcher.rs"]
mod launcher;

fn main() -> ExitCode {
    launcher::launch("apt")
}
