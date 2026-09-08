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
cargo pkg install ripgrep
cargo dnf remove bat
```

## Install

```bash
cargo install cargo-pkgman
```

## Supported frontends

```text
cargo pacman
cargo apt
cargo pkg
cargo dnf
cargo pm
cargo pkgman
```

## Examples

```bash
cargo pacman -S zarust
cargo pacman -Syu

cargo apt install bat
cargo apt upgrade

cargo pkg install just

cargo dnf search eza

cargo pm list
```

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

## Registries

cargo-pkgman targets crates.io. Cargo serves it over the sparse index
protocol, so checking for updates is an ordinary HTTPS request per
crate and no git client is linked in. Lookups run concurrently over a
shared connection.

Alternate registries work as well when their index is configured with
a `sparse+` prefix, and Cargo's source replacement is honoured, so
mirrors are followed.

A registry whose index is served over git cannot be queried. Those
packages are reported with a warning and skipped; everything else in
the same run is still checked. Packages installed from a path or with
`cargo install --git` have no registry to ask, and are left out.

## License

MIT
