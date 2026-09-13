# Changelog

## 0.4.0

### Breaking

- Removed the FreeBSD `pkg` frontend (`cargo-pkg`). The name collided with
  the existing [`cargo-pkg`](https://crates.io/crates/cargo-pkg) crate.

### Added

- Upgrades replay `.crates2.json` install flags (features, bins, profile,
  target, `--no-default-features`).
- `--dry-run` (pacman `-p`, apt `-s`, dnf `-n`) prints the plan without
  installing.
- `--locked` is forwarded to `cargo install` on upgrade and install.
- Pacman clustered flags (`-Sy -u`, `-Syyu`) and `-Syu <pkg>`.
- `pacman -Q <pkg>` lists named packages; missing names are an error.
- Apt/DNF flags before the command (`apt -y install foo`).
- `apt list --installed` / `--upgradable` and `dnf list installed` /
  `updates`.
- Git and path installs appear in `list` with a `(git)` / `(path)` tag.
- Sparse-index ETag cache, 429/5xx retries, and stale-cache fallback.
- Cargo config walk, `CARGO_*` environment variables, and
  `credentials.toml` tokens.
- `CARGO_INSTALL_ROOT` / `[install] root` for the crates file location.
- `dnf check-update` exits 100 when upgrades are available.

### Changed

- Failed upgrades no longer abort the rest of the batch.
- Install and remove pass every crate to a single `cargo` invocation and
  forward `cargo install` flags (`--features`, `--git`, `name@version`).
- Registry lookups use two workers instead of eight.
