#!/bin/sh
# Builds Affinity_Linux-<version>-x86_64.AppImage from target/release.
# Only libc, libgcc and fontconfig are linked, all present on any desktop, so nothing is bundled.
set -eu

root="$(cd "$(dirname "$0")/../.." && pwd)"
cd "$root"
version="$(sed -n 's/^version = "\(.*\)"/\1/p' Cargo.toml | head -n1)"
appdir="target/appimage/AppDir"
tool="target/appimage/appimagetool"

rm -rf "$appdir"
make DESTDIR="$appdir" PREFIX=/usr install
install -Dm755 packaging/appimage/AppRun "$appdir/AppRun"
cp packaging/io.github._360900.AffinityLinux.desktop "$appdir/"
cp packaging/affinity-linux.svg "$appdir/"
ln -sf affinity-linux.svg "$appdir/.DirIcon"
# appimagetool only recognizes the legacy appdata name.
ln -sf io.github._360900.AffinityLinux.metainfo.xml \
    "$appdir/usr/share/metainfo/io.github._360900.AffinityLinux.appdata.xml"

if [ ! -x "$tool" ]; then
    curl -fsSL -o "$tool" \
        https://github.com/AppImage/appimagetool/releases/download/continuous/appimagetool-x86_64.AppImage
    chmod 755 "$tool"
fi

APPIMAGE_EXTRACT_AND_RUN=1 ARCH=x86_64 "$tool" "$appdir" \
    "target/appimage/Affinity_Linux-$version-x86_64.AppImage"
