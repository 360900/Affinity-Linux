# Affinity Linux

One-click installer, launcher and uninstaller for Affinity v3 on Linux, built in
Rust with a Slint interface. Clean-room implementation of the Wine pipeline.

## Status

| Milestone | Scope | State |
| --- | --- | --- |
| M1 | Workspace, config, manifest, runner, distro/GPU/dependency check, GUI shell | done |
| M2 | Dependency installation for Arch, Ubuntu-family, Fedora, openSUSE | pending |
| M3 | Wine bootstrap, winetricks, dotnet48 verification | pending |
| M4 | Affinity installer, MSI fallback, patches, PluginLoader, facades, WebView2 | pending |
| M5 | Launch environment, corruption guard, uninstall, full GUI wiring | pending |

## Build

```sh
cargo build --workspace
```

## Run

```sh
cargo run -p affinity-linux -- doctor
cargo run -p affinity-linux -- doctor --json
cargo run -p affinity-linux-gui
```

## Layout

- `crates/core`: headless engine (no GUI dependencies)
- `crates/cli`: `affinity-linux` CLI, also used by the desktop entry
- `crates/app`: Slint GUI
- `ui/main.slint`: window layout

State lives in `~/.config/affinity-linux/config.toml` and inside the Wine prefix
(`~/.AffinityLinux` by default).
