# Affinity Linux

One-click installer and launcher for Affinity v3 on Linux. Handles system
dependencies, Wine, winetricks, .NET 4.8, GPU backends and post-install fixes,
then keeps the install healthy with repair, update and uninstall.

Written in Rust with a native Slint (Fluent) interface. Clean-room
implementation of the Wine pipeline.

## Features

- Dependency installation for Arch, Ubuntu-family, Fedora and openSUSE
- Wine prefix bootstrap with dotnet48 verification
- GPU acceleration via vkd3d-proton or DXVK, optional OpenCL
- Desktop entries, corruption guard, live log and visible errors
- Every action supports `--dry-run`

## Build

Requires Rust 1.85+.

```sh
cargo build --release --workspace
```

## Usage

GUI:

```sh
cargo run --release -p affinity-linux-gui
```

CLI:

```sh
affinity-linux install     # --installer <setup.exe>, --gpu vkd3d|dxvk, --opencl true|false
affinity-linux launch
affinity-linux update
affinity-linux repair      # re-apply fixes without reinstalling
affinity-linux uninstall
```

Add `--dry-run` to preview changes and `--yes` to skip prompts. Hidden
diagnostics: `doctor`, `deps`, `wine-setup`.

## Layout

| Path | Purpose |
| --- | --- |
| `crates/core` | Headless engine |
| `crates/cli` | `affinity-linux` CLI, used by the desktop entry |
| `crates/app` | Slint GUI |
| `ui/main.slint` | Window layout |

## Files

- Config: `~/.config/affinity-linux/config.toml` (respects `XDG_CONFIG_HOME`)
- Wine prefix: `~/.AffinityLinux` (override with `--prefix`)

## License

MIT
