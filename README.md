# cargo-pkgman

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

All of the logic — argument parsing, registry handling, update
detection and the `cargo-update` integration — lives in a single
`cargo-pkgman` executable.

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

## License

MIT
