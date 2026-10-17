# Affinity Linux

One-click installer, launcher, repair, update and uninstall app for Affinity v3
on Linux, built in Rust with a Slint interface. Clean-room implementation of the
Wine pipeline.

## Status

| Milestone | Scope | State |
| --- | --- | --- |
| M1 | Workspace, config, manifest, runner, distro/GPU/dependency check, GUI shell | done |
| M2 | Dependency installation for Arch, Ubuntu-family, Fedora, openSUSE | done |
| M3 | Wine bootstrap, winetricks, dotnet48 verification | done |
| M4 | Affinity installer, MSI fallback, patches, PluginLoader, facades, WebView2 | done |
| M5 | GPU backends, launch environment, corruption guard, uninstall, full GUI wiring | done |
| M6 | One-click UI redesign, repair and update flows, advanced panel | done |
| M7 | Native Fluent UI (light/dark), compact layout, visible errors, live log | done |

## Build

```sh
cargo build --workspace
```

## Run

```sh
cargo run -p affinity-linux -- install --dry-run
cargo run -p affinity-linux -- install
cargo run -p affinity-linux -- update --dry-run
cargo run -p affinity-linux -- update
cargo run -p affinity-linux -- repair --dry-run
cargo run -p affinity-linux -- repair
cargo run -p affinity-linux -- launch --dry-run
cargo run -p affinity-linux -- launch
cargo run -p affinity-linux -- uninstall --dry-run
cargo run -p affinity-linux -- uninstall
cargo run -p affinity-linux-gui
```

Diagnostics remain available but hidden from the main help:

```sh
cargo run -p affinity-linux -- doctor
cargo run -p affinity-linux -- deps --dry-run
cargo run -p affinity-linux -- wine-setup --dry-run
```

## Layout

- `crates/core`: headless engine (no GUI dependencies)
- `crates/cli`: `affinity-linux` CLI, also used by the desktop entry
- `crates/app`: Slint GUI
- `ui/main.slint`: window layout

State lives in `~/.config/affinity-linux/config.toml` and inside the Wine prefix
(`~/.AffinityLinux` by default).
