# Affinity Linux

One-click installer and launcher for Affinity v3 on Linux. Handles system
dependencies, Wine, winetricks, .NET 4.8, GPU backends and post-install fixes,
then keeps the install healthy with repair, update and uninstall.

Written in Rust with a native Slint (Fluent) interface. Clean-room
implementation of the Wine pipeline.

## Features

- Dependency installation for Arch, Ubuntu-family, Fedora and openSUSE
- Wine prefix bootstrap with dotnet48 verification
- Automatic Wine DPI from the display's resolution and scale
- GPU acceleration via vkd3d-proton or DXVK, optional OpenCL
- Desktop entries, corruption guard, live log and visible errors
- Every action supports `--dry-run`

## Install

Packages are attached to each [release](https://github.com/360900/Affinity-Linux/releases).

Arch and derivatives:

```sh
git clone https://github.com/360900/Affinity-Linux
cd Affinity-Linux/packaging/arch && makepkg -si
```

Debian, Ubuntu and derivatives:

```sh
sudo apt install ./affinity-linux_<version>-1_amd64.deb
```

Fedora:

```sh
sudo dnf install ./affinity-linux-<version>-1.x86_64.rpm
```

openSUSE:

```sh
sudo zypper install ./affinity-linux-<version>-1.x86_64.rpm
```

Any distro, AppImage (no arguments opens the GUI, arguments go to the CLI):

```sh
chmod +x Affinity_Linux-<version>-x86_64.AppImage
./Affinity_Linux-<version>-x86_64.AppImage
./Affinity_Linux-<version>-x86_64.AppImage install --dry-run
```

Other distros, from the release tarball (or after `make build`):

```sh
sudo make install            # PREFIX=/usr/local by default
sudo make uninstall
```

## Build

Requires Rust 1.85+.

```sh
cargo build --release --workspace
```

Building the GUI needs fontconfig headers (`libfontconfig1-dev`,
`fontconfig-devel`).

## Usage

GUI:

```sh
affinity-linux-gui
```

CLI:

```sh
affinity-linux install     # --installer <setup.exe>, --gpu vkd3d|dxvk, --opencl true|false, --dpi <96-480>
affinity-linux launch      # --wayland (experimental)
affinity-linux update
affinity-linux repair      # re-apply fixes without reinstalling, --dpi <96-480>
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
| `packaging` | Desktop entry, icon, PKGBUILD, AppImage |

## Files

- Config: `~/.config/affinity-linux/config.toml` (respects `XDG_CONFIG_HOME`)
- Wine prefix: `~/.AffinityLinux` (override with `--prefix`)
- Logs: `<prefix>/logs`
- Download cache: `~/.cache/affinity-linux` (respects `XDG_CACHE_HOME`)

## License

GPL-3.0-or-later. See [LICENSE](LICENSE).
