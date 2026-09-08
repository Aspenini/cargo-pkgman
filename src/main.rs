mod backend;
mod registry;

use std::{
    env,
    process::{Command, ExitCode},
};

/*
 * All package-management logic lives in this single executable.
 *
 * The cargo-pacman, cargo-apt, cargo-pkg, cargo-dnf and cargo-pm
 * binaries are tiny launchers that re-invoke this program with
 * `--frontend <name>` instead of statically linking a second copy of
 * the registry and update machinery.
 */
#[derive(Debug, Clone, Copy)]
enum Dialect {
    Native,
    Pacman,
    Apt,
    Pkg,
    Dnf,
}

impl Dialect {
    /*
     * The name a launcher passes to --frontend, which is also the Cargo
     * subcommand that frontend is invoked as.
     */
    fn from_name(name: &str) -> Option<Self> {
        match name {
            "pm" => Some(Self::Native),
            "pacman" => Some(Self::Pacman),
            "apt" => Some(Self::Apt),
            "pkg" => Some(Self::Pkg),
            "dnf" => Some(Self::Dnf),

            _ => None,
        }
    }

    fn subcommand(self) -> &'static str {
        match self {
            Self::Native => "pm",
            Self::Pacman => "pacman",
            Self::Apt => "apt",
            Self::Pkg => "pkg",
            Self::Dnf => "dnf",
        }
    }
}

#[derive(Debug)]
enum Operation {
    Install(Vec<String>),
    Remove(Vec<String>),

    CheckUpdates,
    UpgradeAll,

    Search(String),
    Info(String),

    List,
    Help,
}

fn main() -> ExitCode {
    let mut args: Vec<String> = env::args().skip(1).collect();

    /*
     * Cargo external subcommands can pass the subcommand itself.
     *
     * cargo pkgman list
     *
     * may invoke:
     *
     * cargo-pkgman pkgman list
     */
    if args.first().map(String::as_str) == Some("pkgman") {
        args.remove(0);
    }

    let dialect = match take_frontend(&mut args) {
        Ok(dialect) => dialect,

        Err(error) => {
            eprintln!("error: {error}");
            return ExitCode::from(2);
        }
    };

    /*
     * A launcher forwards its own arguments verbatim, so the frontend
     * subcommand can still be leading:
     *
     * cargo apt upgrade
     *   -> cargo-apt apt upgrade
     *   -> cargo-pkgman --frontend apt -- apt upgrade
     */
    if args.first().map(String::as_str) == Some(dialect.subcommand()) {
        args.remove(0);
    }

    let operation = match parse(dialect, &args) {
        Ok(operation) => operation,

        Err(error) => {
            eprintln!("error: {error}");
            eprintln!();

            print_help(dialect);

            return ExitCode::from(2);
        }
    };

    match execute(dialect, operation) {
        Ok(()) => ExitCode::SUCCESS,

        Err(error) => {
            eprintln!("error: {error}");
            ExitCode::FAILURE
        }
    }
}

/*
 * Take a leading `--frontend <name>` or `--frontend=<name>` along with
 * the `--` separator that follows it.
 *
 * Running cargo-pkgman without one behaves like the native frontend.
 */
fn take_frontend(args: &mut Vec<String>) -> Result<Dialect, String> {
    const FLAG: &str = "--frontend";

    let name = match args.first().map(String::as_str) {
        Some(FLAG) => {
            let Some(name) = args.get(1).cloned() else {
                return Err(format!("{FLAG} requires a frontend name"));
            };

            args.drain(..2);
            name
        }

        Some(argument) if argument.starts_with("--frontend=") => {
            let name = argument[FLAG.len() + 1..].to_string();

            args.remove(0);
            name
        }

        _ => return Ok(Dialect::Native),
    };

    if args.first().map(String::as_str) == Some("--") {
        args.remove(0);
    }

    Dialect::from_name(&name).ok_or_else(|| format!("unknown frontend '{name}'"))
}

fn parse(dialect: Dialect, args: &[String]) -> Result<Operation, String> {
    if args.is_empty() {
        return Ok(Operation::Help);
    }

    match dialect {
        Dialect::Native => parse_native(args),
        Dialect::Pacman => parse_pacman(args),
        Dialect::Apt => parse_apt(args),
        Dialect::Pkg => parse_pkg(args),
        Dialect::Dnf => parse_dnf(args),
    }
}

fn parse_native(args: &[String]) -> Result<Operation, String> {
    match args[0].as_str() {
        "install" | "add" => packages(args, 1, Operation::Install),

        "remove" | "uninstall" => packages(args, 1, Operation::Remove),

        "update" | "check" | "outdated" => Ok(Operation::CheckUpdates),

        "upgrade" => Ok(Operation::UpgradeAll),

        "search" => argument(args, "search", Operation::Search),

        "info" | "show" => argument(args, "info", Operation::Info),

        "list" => Ok(Operation::List),

        "help" | "-h" | "--help" => Ok(Operation::Help),

        command => Err(format!("unknown command '{command}'")),
    }
}

fn parse_pacman(args: &[String]) -> Result<Operation, String> {
    match args[0].as_str() {
        "-Syu" | "-Suy" => Ok(Operation::UpgradeAll),

        "-Sy" => Ok(Operation::CheckUpdates),

        "-Su" => Ok(Operation::UpgradeAll),

        "-S" => packages(args, 1, Operation::Install),

        "-R" | "-Rn" | "-Rs" | "-Rns" => packages(args, 1, Operation::Remove),

        "-Ss" => argument(args, "-Ss", Operation::Search),

        "-Si" | "-Qi" => argument(args, "-Si", Operation::Info),

        "-Q" | "-Qe" => Ok(Operation::List),

        "-Qu" => Ok(Operation::CheckUpdates),

        "-h" | "--help" => Ok(Operation::Help),

        operation => Err(format!("unsupported pacman operation '{operation}'")),
    }
}

fn parse_apt(args: &[String]) -> Result<Operation, String> {
    match args[0].as_str() {
        /*
         * Unlike real APT we don't need a permanent local package
         * database, so "update" means refresh registries and report
         * available Cargo upgrades.
         */
        "update" => Ok(Operation::CheckUpdates),

        "upgrade" | "full-upgrade" | "dist-upgrade" => Ok(Operation::UpgradeAll),

        "install" => packages(args, 1, Operation::Install),

        "remove" | "purge" => packages(args, 1, Operation::Remove),

        "search" => argument(args, "search", Operation::Search),

        "show" => argument(args, "show", Operation::Info),

        "list" => {
            if args.iter().any(|argument| argument == "--upgradable") {
                Ok(Operation::CheckUpdates)
            } else {
                Ok(Operation::List)
            }
        }

        "help" | "-h" | "--help" => Ok(Operation::Help),

        operation => Err(format!("unsupported apt operation '{operation}'")),
    }
}

fn parse_pkg(args: &[String]) -> Result<Operation, String> {
    match args[0].as_str() {
        "install" | "add" => packages(args, 1, Operation::Install),

        "delete" | "remove" => packages(args, 1, Operation::Remove),

        "update" => Ok(Operation::CheckUpdates),

        "upgrade" => Ok(Operation::UpgradeAll),

        "search" => argument(args, "search", Operation::Search),

        "info" => {
            if args.len() == 1 {
                Ok(Operation::List)
            } else {
                argument(args, "info", Operation::Info)
            }
        }

        "version" => Ok(Operation::CheckUpdates),

        "help" | "-h" | "--help" => Ok(Operation::Help),

        operation => Err(format!("unsupported pkg operation '{operation}'")),
    }
}

fn parse_dnf(args: &[String]) -> Result<Operation, String> {
    match args[0].as_str() {
        "install" => packages(args, 1, Operation::Install),

        "remove" | "erase" => packages(args, 1, Operation::Remove),

        "upgrade" => Ok(Operation::UpgradeAll),

        "update" => Ok(Operation::UpgradeAll),

        "check-update" | "makecache" => Ok(Operation::CheckUpdates),

        "search" => argument(args, "search", Operation::Search),

        "info" => argument(args, "info", Operation::Info),

        "list" => {
            if args.iter().any(|argument| argument == "updates") {
                Ok(Operation::CheckUpdates)
            } else {
                Ok(Operation::List)
            }
        }

        "help" | "-h" | "--help" => Ok(Operation::Help),

        operation => Err(format!("unsupported dnf operation '{operation}'")),
    }
}

fn packages<F>(args: &[String], start: usize, make: F) -> Result<Operation, String>
where
    F: FnOnce(Vec<String>) -> Operation,
{
    let packages: Vec<String> = args[start..]
        .iter()
        .filter(|arg| !arg.starts_with('-'))
        .cloned()
        .collect();

    if packages.is_empty() {
        return Err("no packages specified".to_string());
    }

    Ok(make(packages))
}

fn argument<F>(args: &[String], command: &str, make: F) -> Result<Operation, String>
where
    F: FnOnce(String) -> Operation,
{
    if args.len() < 2 {
        return Err(format!("{command} requires an argument"));
    }

    Ok(make(args[1..].join(" ")))
}

fn execute(dialect: Dialect, operation: Operation) -> Result<(), String> {
    match operation {
        Operation::Install(packages) => run_package_command("install", &packages),

        Operation::Remove(packages) => run_package_command("uninstall", &packages),

        Operation::CheckUpdates => backend::check_updates(),

        Operation::UpgradeAll => backend::upgrade_all(),

        Operation::Search(query) => run_cargo(&["search", &query]),

        Operation::Info(package) => run_cargo(&["info", &package]),

        Operation::List => list_packages(),

        Operation::Help => {
            print_help(dialect);
            Ok(())
        }
    }
}

fn run_package_command(command: &str, packages: &[String]) -> Result<(), String> {
    for package in packages {
        let status = Command::new("cargo")
            .arg(command)
            .arg(package)
            .status()
            .map_err(|error| format!("failed to run cargo {command}: {error}"))?;

        if !status.success() {
            return Err(format!("cargo {command} failed for {package}"));
        }
    }

    Ok(())
}

fn run_cargo(arguments: &[&str]) -> Result<(), String> {
    let status = Command::new("cargo")
        .args(arguments)
        .status()
        .map_err(|error| format!("failed to run Cargo: {error}"))?;

    if !status.success() {
        return Err("Cargo command failed".to_string());
    }

    Ok(())
}

fn list_packages() -> Result<(), String> {
    /*
     * Read Cargo's own .crates.toml instead of spawning
     * `cargo install --list`.
     */
    for package in backend::installed_packages()? {
        println!("{} {}", package.name, package.version);
    }

    Ok(())
}

fn print_help(dialect: Dialect) {
    match dialect {
        Dialect::Native => println!(
            r#"cargo pm - package manager for Cargo applications

Usage:
  cargo pm install <package...>
  cargo pm remove <package...>
  cargo pm update
  cargo pm upgrade
  cargo pm search <query>
  cargo pm info <package>
  cargo pm list"#
        ),

        Dialect::Pacman => println!(
            r#"cargo pacman

  cargo pacman -S <package...>
  cargo pacman -R <package...>
  cargo pacman -Sy
  cargo pacman -Su
  cargo pacman -Syu
  cargo pacman -Ss <query>
  cargo pacman -Si <package>
  cargo pacman -Q
  cargo pacman -Qu"#
        ),

        Dialect::Apt => println!(
            r#"cargo apt

  cargo apt update
  cargo apt upgrade
  cargo apt install <package...>
  cargo apt remove <package...>
  cargo apt search <query>
  cargo apt show <package>
  cargo apt list --installed
  cargo apt list --upgradable"#
        ),

        Dialect::Pkg => println!(
            r#"cargo pkg

  cargo pkg update
  cargo pkg upgrade
  cargo pkg install <package...>
  cargo pkg delete <package...>
  cargo pkg search <query>
  cargo pkg info [package]
  cargo pkg version"#
        ),

        Dialect::Dnf => println!(
            r#"cargo dnf

  cargo dnf check-update
  cargo dnf upgrade
  cargo dnf install <package...>
  cargo dnf remove <package...>
  cargo dnf search <query>
  cargo dnf info <package>
  cargo dnf list installed
  cargo dnf list updates"#
        ),
    }
}
