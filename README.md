# cargo-pkgman

Package-manager style commands for Cargo-installed applications.

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

## License

MIT
