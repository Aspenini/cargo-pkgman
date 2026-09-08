use std::{
    env,
    io,
    process::{Command, ExitCode, Stdio},
};

#[derive(Debug, Clone, Copy)]
pub enum Dialect {
    Native,
    Pacman,
    Apt,
    Pkg,
    Dnf,
}

#[derive(Debug)]
enum Operation {
    Install(Vec<String>),
    Remove(Vec<String>),
    UpgradeAll,
    Refresh,
    Search(String),
    Info(String),
    List,
    Outdated,
    Help,
}

pub fn run(dialect: Dialect) -> ExitCode {
    let mut args: Vec<String> = env::args().skip(1).collect();

    // Cargo external subcommands may receive the subcommand name itself
    // as argv[1]. Strip it if present.
    let expected = match dialect {
        Dialect::Native => "pm",
        Dialect::Pacman => "pacman",
        Dialect::Apt => "apt",
        Dialect::Pkg => "pkg",
        Dialect::Dnf => "dnf",
    };

    if args.first().map(String::as_str) == Some(expected) {
        args.remove(0);
    }

    let operation = match parse(dialect, &args) {
        Ok(op) => op,
        Err(err) => {
            eprintln!("error: {err}");
            eprintln!();
            print_help(dialect);
            return ExitCode::from(2);
        }
    };

    match execute(operation) {
        Ok(code) => ExitCode::from(code),
        Err(err) => {
            eprintln!("error: {err}");
            ExitCode::FAILURE
        }
    }
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

        "upgrade" | "update-all" => Ok(Operation::UpgradeAll),
        "update" | "refresh" => Ok(Operation::Refresh),

        "search" => one_arg(args, "search", Operation::Search),
        "info" | "show" => one_arg(args, "info", Operation::Info),

        "list" => Ok(Operation::List),
        "outdated" => Ok(Operation::Outdated),

        "help" | "-h" | "--help" => Ok(Operation::Help),

        other => Err(format!("unknown command '{other}'")),
    }
}

fn parse_pacman(args: &[String]) -> Result<Operation, String> {
    let flag = args[0].as_str();

    match flag {
        "-Syu" | "-Suy" => Ok(Operation::UpgradeAll),

        "-Sy" => Ok(Operation::Refresh),
        "-Su" => Ok(Operation::UpgradeAll),

        "-S" => packages(args, 1, Operation::Install),

        "-R" | "-Rs" | "-Rns" | "-Rn" => {
            packages(args, 1, Operation::Remove)
        }

        "-Ss" => one_arg(args, "-Ss", Operation::Search),

        "-Si" | "-Qi" => one_arg(args, flag, Operation::Info),

        "-Q" | "-Qe" => Ok(Operation::List),

        "-Qu" => Ok(Operation::Outdated),

        "-h" | "--help" => Ok(Operation::Help),

        other => Err(format!("unsupported pacman operation '{other}'")),
    }
}

fn parse_apt(args: &[String]) -> Result<Operation, String> {
    match args[0].as_str() {
        "update" => Ok(Operation::Refresh),

        "upgrade" | "full-upgrade" | "dist-upgrade" => {
            Ok(Operation::UpgradeAll)
        }

        "install" => packages(args, 1, Operation::Install),

        "remove" | "purge" => {
            packages(args, 1, Operation::Remove)
        }

        "search" => one_arg(args, "search", Operation::Search),

        "show" => one_arg(args, "show", Operation::Info),

        "list" => {
            if args.len() == 1 || args.iter().any(|arg| arg == "--installed") {
                Ok(Operation::List)
            } else if args.iter().any(|arg| arg == "--upgradable") {
                Ok(Operation::Outdated)
            } else {
                Err("unsupported apt list option".into())
            }
        }

        "help" | "-h" | "--help" => Ok(Operation::Help),

        other => Err(format!("unsupported apt operation '{other}'")),
    }
}

fn parse_pkg(args: &[String]) -> Result<Operation, String> {
    match args[0].as_str() {
        "install" | "add" => packages(args, 1, Operation::Install),

        "delete" | "remove" => packages(args, 1, Operation::Remove),

        "upgrade" => Ok(Operation::UpgradeAll),

        "update" => Ok(Operation::Refresh),

        "search" => one_arg(args, "search", Operation::Search),

        "info" => {
            if args.len() == 1 {
                Ok(Operation::List)
            } else {
                one_arg(args, "info", Operation::Info)
            }
        }

        "version" => Ok(Operation::Outdated),

        "help" | "-h" | "--help" => Ok(Operation::Help),

        other => Err(format!("unsupported pkg operation '{other}'")),
    }
}

fn parse_dnf(args: &[String]) -> Result<Operation, String> {
    match args[0].as_str() {
        "install" => packages(args, 1, Operation::Install),

        "remove" | "erase" => packages(args, 1, Operation::Remove),

        "upgrade" | "update" => Ok(Operation::UpgradeAll),

        "makecache" | "check-update" => Ok(Operation::Refresh),

        "search" => one_arg(args, "search", Operation::Search),

        "info" => one_arg(args, "info", Operation::Info),

        "list" => {
            if args.iter().any(|x| x == "updates") {
                Ok(Operation::Outdated)
            } else {
                Ok(Operation::List)
            }
        }

        "help" | "-h" | "--help" => Ok(Operation::Help),

        other => Err(format!("unsupported dnf operation '{other}'")),
    }
}

fn packages<F>(
    args: &[String],
    start: usize,
    make: F,
) -> Result<Operation, String>
where
    F: FnOnce(Vec<String>) -> Operation,
{
    let packages: Vec<String> = args[start..]
        .iter()
        .filter(|arg| !arg.starts_with('-'))
        .cloned()
        .collect();

    if packages.is_empty() {
        return Err("no packages specified".into());
    }

    Ok(make(packages))
}

fn one_arg<F>(
    args: &[String],
    command: &str,
    make: F,
) -> Result<Operation, String>
where
    F: FnOnce(String) -> Operation,
{
    if args.len() < 2 {
        return Err(format!("{command} requires an argument"));
    }

    Ok(make(args[1..].join(" ")))
}

fn execute(operation: Operation) -> io::Result<u8> {
    match operation {
        Operation::Install(packages) => {
            cargo_with_packages("install", &packages)
        }

        Operation::Remove(packages) => {
            cargo_with_packages("uninstall", &packages)
        }

        Operation::UpgradeAll => upgrade_all(),

        Operation::Refresh => refresh(),

        Operation::Search(query) => {
            run_cargo(&["search", &query])
        }

        Operation::Info(package) => {
            run_cargo(&["info", &package])
        }

        Operation::List => {
            run_cargo(&["install", "--list"])
        }

        Operation::Outdated => outdated(),

        Operation::Help => {
            println!("cargo-pkgman");
            println!("Run the selected frontend with --help.");
            Ok(0)
        }
    }
}

fn cargo_with_packages(
    subcommand: &str,
    packages: &[String],
) -> io::Result<u8> {
    let mut cmd = Command::new("cargo");

    cmd.arg(subcommand);

    for package in packages {
        cmd.arg(package);
    }

    status_code(cmd.status()?)
}

fn run_cargo(args: &[&str]) -> io::Result<u8> {
    let status = Command::new("cargo")
        .args(args)
        .status()?;

    status_code(status)
}

fn upgrade_all() -> io::Result<u8> {
    println!(":: Checking Cargo-installed packages for upgrades...");

    let status = Command::new("cargo")
        .args(["install-update", "-a"])
        .status()?;

    status_code(status)
}

fn outdated() -> io::Result<u8> {
    let status = Command::new("cargo")
        .args(["install-update", "-a", "-l"])
        .status()?;

    status_code(status)
}

fn refresh() -> io::Result<u8> {
    // Modern Cargo's sparse registry is fetched on demand, so there isn't
    // really an apt-style package-list database that needs refreshing.
    //
    // Probe crates.io quietly to make the operation useful.
    println!(":: Checking Cargo registry...");

    let status = Command::new("cargo")
        .args([
            "search",
            "cargo-pkgman",
            "--limit",
            "1",
        ])
        .stdout(Stdio::null())
        .status()?;

    if status.success() {
        println!(":: Registry is reachable.");
    }

    status_code(status)
}

fn status_code(status: std::process::ExitStatus) -> io::Result<u8> {
    Ok(status.code().unwrap_or(1).clamp(0, 255) as u8)
}

fn print_help(dialect: Dialect) {
    match dialect {
        Dialect::Native => {
            println!(
r#"cargo pm - Cargo application package manager

Usage:
  cargo pm install <package...>
  cargo pm remove <package...>
  cargo pm update
  cargo pm upgrade
  cargo pm search <query>
  cargo pm info <package>
  cargo pm list
  cargo pm outdated"#
            );
        }

        Dialect::Pacman => {
            println!(
r#"cargo pacman - pacman-style Cargo package management

Usage:
  cargo pacman -S <package...>    Install
  cargo pacman -R <package...>    Remove
  cargo pacman -Sy                Refresh registry
  cargo pacman -Su                Upgrade
  cargo pacman -Syu               Refresh + upgrade
  cargo pacman -Ss <query>        Search
  cargo pacman -Si <package>      Package info
  cargo pacman -Q                 Installed packages
  cargo pacman -Qu                Available upgrades"#
            );
        }

        Dialect::Apt => {
            println!(
r#"cargo apt - APT-style Cargo package management

Usage:
  cargo apt update
  cargo apt upgrade
  cargo apt install <package...>
  cargo apt remove <package...>
  cargo apt search <query>
  cargo apt show <package>
  cargo apt list --installed
  cargo apt list --upgradable"#
            );
        }

        Dialect::Pkg => {
            println!(
r#"cargo pkg - FreeBSD pkg-style Cargo package management

Usage:
  cargo pkg update
  cargo pkg upgrade
  cargo pkg install <package...>
  cargo pkg delete <package...>
  cargo pkg search <query>
  cargo pkg info [package]
  cargo pkg version"#
            );
        }

        Dialect::Dnf => {
            println!(
r#"cargo dnf - DNF-style Cargo package management

Usage:
  cargo dnf upgrade
  cargo dnf install <package...>
  cargo dnf remove <package...>
  cargo dnf search <query>
  cargo dnf info <package>
  cargo dnf list installed
  cargo dnf list updates"#
            );
        }
    }
}
