# cargo-pkgman

[![crates.io](https://img.shields.io/crates/v/cargo-pkgman.svg)](https://crates.io/crates/cargo-pkgman)
[![downloads](https://img.shields.io/crates/d/cargo-pkgman.svg)](https://crates.io/crates/cargo-pkgman)
[![msrv](https://img.shields.io/crates/msrv/cargo-pkgman.svg)](Cargo.toml)
[![license](https://img.shields.io/crates/l/cargo-pkgman.svg)](LICENSE)

Package-manager style commands for Cargo-installed programs.

Use familiar package manager syntax with Cargo:

```bash
cargo pacman -Syu
cargo apt upgrade
cargo dnf remove bat
cargo pm install ripgrep
```

## Install

```bash
cargo install cargo-pkgman
```

## Supported frontends

```text
cargo pacman
cargo apt
cargo dnf
cargo pm
cargo pkgman
```

There is no FreeBSD `pkg` frontend. A `cargo-pkg` binary would clash with the
existing [`cargo-pkg`](https://crates.io/crates/cargo-pkg) crate.

## Examples

```bash
cargo pacman -S zarust
cargo pacman -Syu
cargo pacman -Sy -u
cargo pacman -Q ripgrep

cargo apt install bat
cargo apt upgrade --dry-run
cargo apt list --upgradable

cargo dnf search eza
cargo dnf check-update

cargo pm install --features pcre2 ripgrep
cargo pm upgrade --locked
cargo pm list
```

Install arguments are forwarded to `cargo install`, so version specs
(`ripgrep@14`), `--locked`, `--features`, `--git`, and `--path` work.

`--dry-run` (pacman `-p` / `--print`, apt `-s` / `--simulate`, dnf `-n` /
`--assumeno`) prints the plan without installing.

## How it works

All of the logic — argument parsing, registry handling and update
detection — lives in a single `cargo-pkgman` executable.

Each frontend is a small launcher that re-invokes it with the frontend
selected:

```text
cargo apt upgrade
  -> cargo-apt apt upgrade
  -> cargo-pkgman --frontend apt -- apt upgrade
```

Because the package management code and its dependencies are linked
only once, the launchers stay a couple of hundred kilobytes each
instead of every frontend shipping its own copy.

You can also invoke the shared executable directly:

```bash
cargo pkgman list
cargo pkgman --frontend pacman -- -Syu
```

On Unix the launcher `exec`s `cargo-pkgman`. On Windows it waits for the
child process and forwards its exit status; Ctrl+C is delivered to the
process group.

## Upgrades

An upgrade looks up each installed crate on its sparse index, then runs
`cargo install` with the original flags recorded in `.crates2.json`
(features, bins, profile, target, `--no-default-features`, and so on).

If one crate fails to rebuild, the rest of the batch still runs. The
command exits non-zero and lists the failures.

`cargo dnf check-update` exits with status 100 when upgrades are
available, matching DNF.

## Registries

cargo-pkgman targets crates.io. Cargo serves it over the sparse index
protocol, so checking for updates is an ordinary HTTPS request per
crate and no git client is linked in. Lookups run concurrently over a
shared connection, reuse ETags, retry on 429/5xx, and fall back to a
local cache if the network fails.

Alternate registries work as well when their index is configured with
a `sparse+` prefix, and Cargo's source replacement is honoured, so
mirrors are followed. Configuration is read from `$CARGO_HOME/config.toml`,
`.cargo/config.toml` walking from the current directory, and `CARGO_*`
environment variables. Private registries use tokens from
`credentials.toml` or `CARGO_REGISTRIES_<NAME>_TOKEN`.

A registry whose index is served over git cannot be queried. Those
packages are reported with a warning and skipped; everything else in
the same run is still checked.

Packages installed from a path or with `cargo install --git` are shown
by `list` with a `(git)` / `(path)` tag and are not checked for updates.

Install records are read from the Cargo install root (`CARGO_INSTALL_ROOT`
or `[install] root`, otherwise `$CARGO_HOME`).

## License

MIT
