//! Frontend dialects and argument parsing.

use crate::error::Error;

/// A package-manager frontend.
///
/// Each frontend is a Cargo subcommand (`cargo pacman`, `cargo apt`, …)
/// whose arguments are translated into a single [`Operation`].
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Dialect {
    /// `cargo pm` / `cargo pkgman`.
    Native,
    /// Arch Linux `pacman`-style clustered flags.
    Pacman,
    /// Debian/Ubuntu `apt`-style commands.
    Apt,
    /// Fedora `dnf`-style commands.
    Dnf,
}

impl Dialect {
    /// The name a launcher passes to `--frontend`, which is also the Cargo
    /// subcommand that frontend is invoked as.
    pub fn from_name(name: &str) -> Option<Self> {
        match name {
            "pm" | "pkgman" => Some(Self::Native),
            "pacman" => Some(Self::Pacman),
            "apt" => Some(Self::Apt),
            "dnf" => Some(Self::Dnf),
            _ => None,
        }
    }

    /// Cargo subcommand / `--frontend` value for this dialect.
    pub fn subcommand(self) -> &'static str {
        match self {
            Self::Native => "pm",
            Self::Pacman => "pacman",
            Self::Apt => "apt",
            Self::Dnf => "dnf",
        }
    }
}

/// A parsed package-manager operation.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Operation {
    /// Install one or more crates, forwarding extra arguments to `cargo install`.
    Install {
        /// Crate names or `name@version` specs.
        packages: Vec<String>,
        /// Extra arguments forwarded to `cargo install`.
        cargo_args: Vec<String>,
        /// Print the cargo command instead of running it.
        dry_run: bool,
    },
    /// Uninstall one or more crates.
    Remove {
        /// Crate names to uninstall.
        packages: Vec<String>,
        /// Extra arguments forwarded to `cargo uninstall`.
        cargo_args: Vec<String>,
        /// Print the cargo command instead of running it.
        dry_run: bool,
    },
    /// Query registries and print available upgrades.
    CheckUpdates,
    /// Upgrade every outdated crate, optionally installing extra packages first
    /// (pacman `-Syu pkg`).
    Upgrade {
        /// Print planned upgrades without running `cargo install`.
        dry_run: bool,
        /// Pass `--locked` to `cargo install`.
        locked: bool,
        /// Extra crates to install after the upgrade (pacman `-Syu pkg`).
        also_install: Vec<String>,
        /// Extra arguments for those extra installs.
        install_args: Vec<String>,
    },
    /// `cargo search`.
    Search(String),
    /// `cargo info` for one or more packages.
    Info(Vec<String>),
    /// List installed packages, optionally restricted to named crates.
    List {
        /// Exact package names to show, as in `pacman -Q foo bar`.
        query: Option<Vec<String>>,
    },
    /// Print frontend-specific help.
    Help,
}

#[derive(Default)]
struct Globals {
    dry_run: bool,
    locked: bool,
    help: bool,
}

/// Strip the Cargo subcommand name, a leading `--frontend`, and the matching
/// dialect subcommand so the remaining arguments can be parsed.
pub fn preprocess(mut args: Vec<String>) -> Result<(Dialect, Vec<String>), Error> {
    // `cargo pkgman list` may invoke `cargo-pkgman pkgman list`.
    if args.first().map(String::as_str) == Some("pkgman") {
        args.remove(0);
    }

    let dialect = take_frontend(&mut args)?;

    // A launcher forwards its own arguments verbatim, so the frontend
    // subcommand can still be leading:
    //
    //     cargo apt upgrade
    //       -> cargo-apt apt upgrade
    //       -> cargo-pkgman --frontend apt -- apt upgrade
    if args.first().map(String::as_str) == Some(dialect.subcommand()) {
        args.remove(0);
    }

    Ok((dialect, args))
}

/// Take a leading `--frontend <name>` or `--frontend=<name>` along with the
/// `--` separator that follows it.
///
/// Running cargo-pkgman without one behaves like the native frontend.
pub fn take_frontend(args: &mut Vec<String>) -> Result<Dialect, Error> {
    const FLAG: &str = "--frontend";

    let name = match args.first().map(String::as_str) {
        Some(FLAG) => {
            let Some(name) = args.get(1).cloned() else {
                return Err(Error::Usage(format!("{FLAG} requires a frontend name")));
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

    Dialect::from_name(&name).ok_or_else(|| Error::Usage(format!("unknown frontend '{name}'")))
}

/// Parse frontend arguments into an [`Operation`].
pub fn parse(dialect: Dialect, args: &[String]) -> Result<Operation, Error> {
    let (globals, rest) = take_globals(args);

    if globals.help {
        return Ok(Operation::Help);
    }

    if rest.is_empty() {
        return Ok(Operation::Help);
    }

    let mut operation = match dialect {
        Dialect::Native => parse_native(&rest)?,
        Dialect::Pacman => parse_pacman(&rest)?,
        Dialect::Apt => parse_apt(&rest)?,
        Dialect::Dnf => parse_dnf(&rest)?,
    };

    apply_globals(&mut operation, &globals);
    Ok(operation)
}

fn take_globals(args: &[String]) -> (Globals, Vec<String>) {
    let mut globals = Globals::default();
    let mut rest = Vec::new();

    for arg in args {
        match arg.as_str() {
            "--help" | "-h" => globals.help = true,
            "--dry-run" => globals.dry_run = true,
            "--locked" => globals.locked = true,
            _ => rest.push(arg.clone()),
        }
    }

    (globals, rest)
}

fn apply_globals(operation: &mut Operation, globals: &Globals) {
    match operation {
        Operation::Install {
            cargo_args,
            dry_run,
            ..
        } => {
            *dry_run = *dry_run || globals.dry_run;
            if globals.locked && !cargo_args.iter().any(|a| a == "--locked") {
                cargo_args.insert(0, "--locked".to_string());
            }
        }
        Operation::Remove { dry_run, .. } => {
            *dry_run = *dry_run || globals.dry_run;
        }
        Operation::Upgrade {
            dry_run, locked, ..
        } => {
            *dry_run = *dry_run || globals.dry_run;
            *locked = *locked || globals.locked;
        }
        _ => {}
    }
}

fn parse_native(args: &[String]) -> Result<Operation, Error> {
    let (command, rest) = split_leading_command(args, NATIVE_COMMANDS)?;

    match command {
        "install" | "add" => install_op(rest, false),
        "remove" | "uninstall" => remove_op(rest, false),
        "update" | "check" | "outdated" => Ok(Operation::CheckUpdates),
        "upgrade" => Ok(upgrade_op(rest, false, false)),
        "search" => search_op(rest, "search"),
        "info" | "show" => info_op(rest, "info"),
        "list" => Ok(Operation::List { query: None }),
        "help" => Ok(Operation::Help),
        other => Err(Error::Usage(format!("unknown command '{other}'"))),
    }
}

const NATIVE_COMMANDS: &[&str] = &[
    "install",
    "add",
    "remove",
    "uninstall",
    "update",
    "check",
    "outdated",
    "upgrade",
    "search",
    "info",
    "show",
    "list",
    "help",
];

fn parse_pacman(args: &[String]) -> Result<Operation, Error> {
    let mut flags = PacmanFlags::default();
    let mut positionals = Vec::new();

    for arg in args {
        if arg == "--" {
            continue;
        }

        if let Some(long) = arg.strip_prefix("--") {
            let name = long.split_once('=').map(|(n, _)| n).unwrap_or(long);
            match name {
                "sync" => flags.sync = true,
                "remove" => flags.remove = true,
                "query" => flags.query = true,
                "refresh" => flags.refresh = true,
                "sysupgrade" => flags.sysupgrade = true,
                "search" => flags.search = true,
                "info" => flags.info = true,
                "explicit" => flags.explicit = true,
                "print" | "print-format" => flags.dry_run = true,
                "noconfirm" | "needed" | "quiet" | "verbose" => {}
                "help" => return Ok(Operation::Help),
                other => {
                    return Err(Error::Usage(format!(
                        "unsupported pacman option '--{other}'"
                    )));
                }
            }
            continue;
        }

        if let Some(cluster) = arg.strip_prefix('-') {
            if cluster.is_empty() {
                positionals.push(arg.clone());
                continue;
            }

            for ch in cluster.chars() {
                match ch {
                    'S' => flags.sync = true,
                    'R' => flags.remove = true,
                    'Q' => flags.query = true,
                    'y' => flags.refresh = true,
                    'u' => flags.sysupgrade = true,
                    's' => flags.search = true,
                    'i' => flags.info = true,
                    'e' => flags.explicit = true,
                    'p' => flags.dry_run = true,
                    'h' => return Ok(Operation::Help),
                    // No-ops that real pacman accepts next to the operations we map.
                    'n' | 'c' | 'd' | 'w' | 'q' | 'v' | 'a' | 'l' | 'o' | 'k' | 't' | 'm' => {}
                    other => {
                        return Err(Error::Usage(format!(
                            "unsupported pacman option '-{other}'"
                        )));
                    }
                }
            }
            continue;
        }

        positionals.push(arg.clone());
    }

    let operations = [flags.sync, flags.remove, flags.query]
        .into_iter()
        .filter(|set| *set)
        .count();

    if operations > 1 {
        return Err(Error::Usage(
            "conflicting pacman operations (use only one of -S, -R, -Q)".into(),
        ));
    }

    if flags.sync {
        return parse_pacman_sync(flags, positionals);
    }
    if flags.remove {
        return remove_op(&positionals, flags.dry_run);
    }
    if flags.query {
        return parse_pacman_query(flags, positionals);
    }

    Err(Error::Usage(
        "no pacman operation specified (expected -S, -R, or -Q)".into(),
    ))
}

#[derive(Default, Clone, Copy)]
struct PacmanFlags {
    sync: bool,
    remove: bool,
    query: bool,
    refresh: bool,
    sysupgrade: bool,
    search: bool,
    info: bool,
    #[allow(dead_code)]
    explicit: bool,
    dry_run: bool,
}

fn parse_pacman_sync(flags: PacmanFlags, positionals: Vec<String>) -> Result<Operation, Error> {
    if flags.search {
        return search_op(&positionals, "-Ss");
    }
    if flags.info {
        return info_op(&positionals, "-Si");
    }
    if flags.sysupgrade {
        let (packages, install_args) = split_packages_and_flags(&positionals)?;
        return Ok(Operation::Upgrade {
            dry_run: flags.dry_run,
            locked: false,
            also_install: packages,
            install_args,
        });
    }
    if flags.refresh && positionals.is_empty() {
        return Ok(Operation::CheckUpdates);
    }
    install_op(&positionals, flags.dry_run)
}

fn parse_pacman_query(flags: PacmanFlags, positionals: Vec<String>) -> Result<Operation, Error> {
    if flags.info {
        return info_op(&positionals, "-Qi");
    }
    if flags.sysupgrade {
        return Ok(Operation::CheckUpdates);
    }
    if flags.search {
        return search_op(&positionals, "-Qs");
    }
    if positionals.is_empty() {
        Ok(Operation::List { query: None })
    } else {
        Ok(Operation::List {
            query: Some(positionals),
        })
    }
}

fn parse_apt(args: &[String]) -> Result<Operation, Error> {
    let mut dry_run = false;
    let mut installed = false;
    let mut upgradable = false;
    let mut filtered = Vec::new();

    for arg in args {
        match arg.as_str() {
            "-s" | "--simulate" | "--just-print" | "--recon" | "--dry-run" => dry_run = true,
            "-y" | "--yes" | "--assume-yes" => {}
            "--installed" => installed = true,
            "--upgradable" | "--upgradeable" => upgradable = true,
            _ => filtered.push(arg.clone()),
        }
    }

    if filtered.is_empty() {
        return Ok(Operation::Help);
    }

    let (command, rest) = split_leading_command(&filtered, APT_COMMANDS)?;

    match command {
        "update" => Ok(Operation::CheckUpdates),
        "upgrade" | "full-upgrade" | "dist-upgrade" => Ok(upgrade_op(rest, dry_run, false)),
        "install" => install_op(rest, dry_run),
        "remove" | "purge" => remove_op(rest, dry_run),
        "search" => search_op(rest, "search"),
        "show" => info_op(rest, "show"),
        "list" => {
            if upgradable
                || rest
                    .iter()
                    .any(|a| a == "--upgradable" || a == "--upgradeable")
            {
                Ok(Operation::CheckUpdates)
            } else if installed || rest.iter().any(|a| a == "--installed") || rest.is_empty() {
                Ok(Operation::List { query: None })
            } else {
                Ok(Operation::List {
                    query: Some(rest.to_vec()),
                })
            }
        }
        "help" => Ok(Operation::Help),
        other => Err(Error::Usage(format!("unsupported apt operation '{other}'"))),
    }
}

const APT_COMMANDS: &[&str] = &[
    "update",
    "upgrade",
    "full-upgrade",
    "dist-upgrade",
    "install",
    "remove",
    "purge",
    "search",
    "show",
    "list",
    "help",
];

fn parse_dnf(args: &[String]) -> Result<Operation, Error> {
    let mut dry_run = false;
    let mut filtered = Vec::new();

    for arg in args {
        match arg.as_str() {
            "-y" | "--assumeyes" | "--assume-yes" => {}
            "-n" | "--assumeno" => dry_run = true,
            "--installed" => filtered.push("installed".to_string()),
            "--updates" | "--upgradable" | "--upgradeable" => {
                filtered.push("updates".to_string());
            }
            _ => filtered.push(arg.clone()),
        }
    }

    if filtered.is_empty() {
        return Ok(Operation::Help);
    }

    let (command, rest) = split_leading_command(&filtered, DNF_COMMANDS)?;

    match command {
        "install" => install_op(rest, dry_run),
        "remove" | "erase" => remove_op(rest, dry_run),
        "upgrade" | "update" => Ok(upgrade_op(rest, dry_run, false)),
        "check-update" | "makecache" => Ok(Operation::CheckUpdates),
        "search" => search_op(rest, "search"),
        "info" => info_op(rest, "info"),
        "list" => {
            if rest.iter().any(|a| {
                matches!(
                    a.as_str(),
                    "updates" | "upgrades" | "--updates" | "--upgradable"
                )
            }) {
                Ok(Operation::CheckUpdates)
            } else if rest.iter().any(|a| a == "installed" || a == "--installed") || rest.is_empty()
            {
                Ok(Operation::List { query: None })
            } else {
                Ok(Operation::List {
                    query: Some(rest.to_vec()),
                })
            }
        }
        "help" => Ok(Operation::Help),
        other => Err(Error::Usage(format!("unsupported dnf operation '{other}'"))),
    }
}

const DNF_COMMANDS: &[&str] = &[
    "install",
    "remove",
    "erase",
    "upgrade",
    "update",
    "check-update",
    "makecache",
    "search",
    "info",
    "list",
    "help",
];

/// Locate the first recognised command word, allowing flags before it
/// (`apt -y install foo`).
fn split_leading_command<'a>(
    args: &'a [String],
    commands: &[&str],
) -> Result<(&'a str, &'a [String]), Error> {
    for (index, arg) in args.iter().enumerate() {
        if commands.contains(&arg.as_str()) {
            return Ok((arg.as_str(), &args[index + 1..]));
        }
    }

    // Native: flags after the command are the supported form. If the first
    // token is not a command, it is an error.
    match args.first() {
        Some(arg) => Err(Error::Usage(format!("unknown command '{arg}'"))),
        None => Ok(("help", args)),
    }
}

fn install_op(args: &[String], dry_run: bool) -> Result<Operation, Error> {
    let (packages, cargo_args) = split_packages_and_flags(args)?;
    if packages.is_empty() && !has_source_flag(&cargo_args) {
        return Err(Error::Usage("no packages specified".into()));
    }
    Ok(Operation::Install {
        packages,
        cargo_args,
        dry_run,
    })
}

fn remove_op(args: &[String], dry_run: bool) -> Result<Operation, Error> {
    let (packages, cargo_args) = split_packages_and_flags(args)?;
    if packages.is_empty() {
        return Err(Error::Usage("no packages specified".into()));
    }
    Ok(Operation::Remove {
        packages,
        cargo_args,
        dry_run,
    })
}

fn upgrade_op(args: &[String], dry_run: bool, locked: bool) -> Operation {
    let also_install = args
        .iter()
        .filter(|arg| !arg.starts_with('-'))
        .cloned()
        .collect();
    Operation::Upgrade {
        dry_run,
        locked,
        also_install,
        install_args: args
            .iter()
            .filter(|arg| arg.starts_with('-'))
            .cloned()
            .collect(),
    }
}

fn search_op(args: &[String], command: &str) -> Result<Operation, Error> {
    let query: Vec<&String> = args.iter().filter(|arg| !arg.starts_with('-')).collect();
    if query.is_empty() {
        return Err(Error::Usage(format!("{command} requires an argument")));
    }
    Ok(Operation::Search(
        query
            .into_iter()
            .map(String::as_str)
            .collect::<Vec<_>>()
            .join(" "),
    ))
}

fn info_op(args: &[String], command: &str) -> Result<Operation, Error> {
    let packages: Vec<String> = args
        .iter()
        .filter(|arg| !arg.starts_with('-'))
        .cloned()
        .collect();
    if packages.is_empty() {
        return Err(Error::Usage(format!("{command} requires an argument")));
    }
    Ok(Operation::Info(packages))
}

/// Flags that consume the following argument for `cargo install` / `uninstall`.
const VALUE_FLAGS: &[&str] = &[
    "--features",
    "-F",
    "--bin",
    "--example",
    "--profile",
    "--target",
    "--root",
    "--git",
    "--branch",
    "--tag",
    "--rev",
    "--path",
    "--registry",
    "--index",
    "--version",
    "--vers",
    "--color",
    "--jobs",
    "-j",
    "-Z",
];

fn split_packages_and_flags(args: &[String]) -> Result<(Vec<String>, Vec<String>), Error> {
    let mut packages = Vec::new();
    let mut flags = Vec::new();
    let mut index = 0;

    while index < args.len() {
        let arg = &args[index];

        if arg == "--" {
            packages.extend(args[index + 1..].iter().cloned());
            break;
        }

        if is_flag(arg) {
            flags.push(arg.clone());
            if is_value_flag(arg) {
                index += 1;
                let Some(value) = args.get(index) else {
                    return Err(Error::Usage(format!("{arg} requires a value")));
                };
                flags.push(value.clone());
            }
            index += 1;
            continue;
        }

        packages.push(arg.clone());
        index += 1;
    }

    Ok((packages, flags))
}

fn is_flag(arg: &str) -> bool {
    arg.starts_with('-') && arg != "-"
}

fn is_value_flag(arg: &str) -> bool {
    VALUE_FLAGS.contains(&arg)
}

fn has_source_flag(flags: &[String]) -> bool {
    flags.iter().any(|flag| {
        flag == "--git"
            || flag == "--path"
            || flag.starts_with("--git=")
            || flag.starts_with("--path=")
    })
}

/// Print frontend-specific help to stdout.
pub fn print_help(dialect: Dialect) {
    match dialect {
        Dialect::Native => println!(
            "\
cargo pm - package manager for Cargo-installed binaries

Usage:
  cargo pm install [cargo-install-flags] <package...>
  cargo pm remove [cargo-uninstall-flags] <package...>
  cargo pm update
  cargo pm upgrade [--dry-run] [--locked]
  cargo pm search <query>
  cargo pm info <package...>
  cargo pm list

Aliases: add, uninstall, check, outdated, show

Install arguments are forwarded to `cargo install`. Version specs
(`ripgrep@14`), --locked, --features, --git, and --path are accepted.

--dry-run   print the planned cargo command without running it
--locked    pass --locked to cargo install on upgrade or install"
        ),

        Dialect::Pacman => println!(
            "\
cargo pacman

  cargo pacman -S <package...>          install
  cargo pacman -R <package...>          remove
  cargo pacman -Sy                      check for updates
  cargo pacman -Su                      upgrade all
  cargo pacman -Syu                     upgrade all
  cargo pacman -Syu <package...>        upgrade all, then install
  cargo pacman -Ss <query>              search
  cargo pacman -Si <package...>         info
  cargo pacman -Q                       list installed
  cargo pacman -Q <package...>          list named packages
  cargo pacman -Qu                      check for updates

Clustered flags such as -Sy -u and -Syyu are accepted.
-p / --print / --dry-run prints the plan without installing."
        ),

        Dialect::Apt => println!(
            "\
cargo apt

  cargo apt update
  cargo apt upgrade [--dry-run]
  cargo apt install [-y] <package...>
  cargo apt remove <package...>
  cargo apt search <query>
  cargo apt show <package...>
  cargo apt list --installed
  cargo apt list --upgradable

-y is accepted and ignored (cargo install does not prompt).
-s / --simulate / --dry-run prints the plan without installing."
        ),

        Dialect::Dnf => println!(
            "\
cargo dnf

  cargo dnf check-update
  cargo dnf upgrade [--dry-run]
  cargo dnf install [-y] <package...>
  cargo dnf remove <package...>
  cargo dnf search <query>
  cargo dnf info <package...>
  cargo dnf list installed
  cargo dnf list updates

-y is accepted and ignored (cargo install does not prompt).
-n / --assumeno / --dry-run prints the plan without installing.
check-update exits with status 100 when upgrades are available."
        ),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn s(line: &str) -> Vec<String> {
        if line.is_empty() {
            Vec::new()
        } else {
            line.split_whitespace().map(String::from).collect()
        }
    }

    fn parse_line(dialect: Dialect, line: &str) -> Operation {
        parse(dialect, &s(line)).unwrap_or_else(|error| panic!("{line:?}: {error}"))
    }

    #[test]
    fn empty_is_help() {
        assert_eq!(parse_line(Dialect::Native, ""), Operation::Help);
        assert_eq!(parse_line(Dialect::Apt, "-y"), Operation::Help);
        assert_eq!(parse_line(Dialect::Dnf, "-y"), Operation::Help);
    }

    #[test]
    fn native_install_version_spec_and_features() {
        let Operation::Install {
            packages,
            cargo_args,
            dry_run,
        } = parse_line(Dialect::Native, "install --features pcre2 ripgrep@14.1.1")
        else {
            panic!("expected install");
        };
        assert_eq!(packages, ["ripgrep@14.1.1"]);
        assert_eq!(cargo_args, ["--features", "pcre2"]);
        assert!(!dry_run);
    }

    #[test]
    fn native_upgrade_dry_run_locked() {
        assert_eq!(
            parse_line(Dialect::Native, "upgrade --dry-run --locked"),
            Operation::Upgrade {
                dry_run: true,
                locked: true,
                also_install: vec![],
                install_args: vec![],
            }
        );
        assert_eq!(
            parse_line(Dialect::Native, "--dry-run upgrade"),
            Operation::Upgrade {
                dry_run: true,
                locked: false,
                also_install: vec![],
                install_args: vec![],
            }
        );
    }

    #[test]
    fn native_git_install_without_package_name() {
        let Operation::Install {
            packages,
            cargo_args,
            ..
        } = parse_line(Dialect::Native, "install --git https://github.com/foo/bar")
        else {
            panic!("expected install");
        };
        assert!(packages.is_empty());
        assert_eq!(cargo_args, ["--git", "https://github.com/foo/bar"]);
    }

    #[test]
    fn pacman_clustered_and_split_flags() {
        let upgrade = Operation::Upgrade {
            dry_run: false,
            locked: false,
            also_install: vec![],
            install_args: vec![],
        };
        assert_eq!(parse_line(Dialect::Pacman, "-Syu"), upgrade);
        assert_eq!(parse_line(Dialect::Pacman, "-Suy"), upgrade);
        assert_eq!(parse_line(Dialect::Pacman, "-Sy -u"), upgrade);
        assert_eq!(parse_line(Dialect::Pacman, "-Syyu"), upgrade);
        assert_eq!(
            parse_line(Dialect::Pacman, "--sync --refresh --sysupgrade"),
            upgrade
        );
    }

    #[test]
    fn pacman_syu_with_package() {
        let Operation::Upgrade {
            also_install,
            dry_run,
            ..
        } = parse_line(Dialect::Pacman, "-Syu ripgrep")
        else {
            panic!("expected upgrade");
        };
        assert_eq!(also_install, ["ripgrep"]);
        assert!(!dry_run);
    }

    #[test]
    fn pacman_query_named_and_outdated() {
        assert_eq!(
            parse_line(Dialect::Pacman, "-Q ripgrep bat"),
            Operation::List {
                query: Some(vec!["ripgrep".into(), "bat".into()]),
            }
        );
        assert_eq!(parse_line(Dialect::Pacman, "-Qu"), Operation::CheckUpdates);
        assert_eq!(
            parse_line(Dialect::Pacman, "-Qe"),
            Operation::List { query: None }
        );
        assert_eq!(parse_line(Dialect::Pacman, "-Sy"), Operation::CheckUpdates);
    }

    #[test]
    fn pacman_print_is_dry_run() {
        let Operation::Upgrade { dry_run, .. } = parse_line(Dialect::Pacman, "-Syup") else {
            panic!("expected upgrade");
        };
        assert!(dry_run);
    }

    #[test]
    fn apt_flags_before_command() {
        let Operation::Install {
            packages, dry_run, ..
        } = parse_line(Dialect::Apt, "-y install ripgrep bat")
        else {
            panic!("expected install");
        };
        assert_eq!(packages, ["ripgrep", "bat"]);
        assert!(!dry_run);
    }

    #[test]
    fn apt_list_variants() {
        assert_eq!(
            parse_line(Dialect::Apt, "list --installed"),
            Operation::List { query: None }
        );
        assert_eq!(
            parse_line(Dialect::Apt, "list --upgradable"),
            Operation::CheckUpdates
        );
        assert_eq!(
            parse_line(Dialect::Apt, "list --upgradeable"),
            Operation::CheckUpdates
        );
        assert_eq!(
            parse_line(Dialect::Apt, "-s upgrade"),
            Operation::Upgrade {
                dry_run: true,
                locked: false,
                also_install: vec![],
                install_args: vec![],
            }
        );
    }

    #[test]
    fn dnf_list_and_check_update() {
        assert_eq!(
            parse_line(Dialect::Dnf, "list installed"),
            Operation::List { query: None }
        );
        assert_eq!(
            parse_line(Dialect::Dnf, "list updates"),
            Operation::CheckUpdates
        );
        assert_eq!(
            parse_line(Dialect::Dnf, "check-update"),
            Operation::CheckUpdates
        );
        assert_eq!(
            parse_line(Dialect::Dnf, "update"),
            Operation::Upgrade {
                dry_run: false,
                locked: false,
                also_install: vec![],
                install_args: vec![],
            }
        );
        assert_eq!(
            parse_line(Dialect::Dnf, "-n upgrade"),
            Operation::Upgrade {
                dry_run: true,
                locked: false,
                also_install: vec![],
                install_args: vec![],
            }
        );
    }

    #[test]
    fn frontend_flag_and_subcommand_strip() {
        let (dialect, rest) =
            preprocess(s("--frontend apt -- apt -y install ripgrep")).expect("preprocess");
        assert_eq!(dialect, Dialect::Apt);
        assert_eq!(rest, ["-y", "install", "ripgrep"]);
    }

    #[test]
    fn pkgman_subcommand_is_native() {
        let (dialect, rest) = preprocess(s("pkgman list")).expect("preprocess");
        assert_eq!(dialect, Dialect::Native);
        assert_eq!(rest, ["list"]);
    }

    #[test]
    fn unknown_frontend() {
        let error = preprocess(s("--frontend pkg -- list")).unwrap_err();
        assert!(matches!(error, Error::Usage(_)));
    }

    #[test]
    fn install_requires_packages() {
        assert!(parse(Dialect::Native, &s("install")).is_err());
        assert!(parse(Dialect::Pacman, &s("-S")).is_err());
    }

    #[test]
    fn value_flag_without_value_is_usage() {
        let error = parse(Dialect::Native, &s("install --features")).unwrap_err();
        assert!(matches!(error, Error::Usage(_)));
    }
}
