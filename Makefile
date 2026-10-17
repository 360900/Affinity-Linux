PREFIX ?= /usr/local
DESTDIR ?=
BINDIR = $(DESTDIR)$(PREFIX)/bin
APPDIR = $(DESTDIR)$(PREFIX)/share/applications
ICONDIR = $(DESTDIR)$(PREFIX)/share/icons/hicolor/scalable/apps
METADIR = $(DESTDIR)$(PREFIX)/share/metainfo
LICDIR = $(DESTDIR)$(PREFIX)/share/licenses/affinity-linux

.PHONY: build install uninstall appimage

build:
	cargo build --release --locked --workspace

install:
	install -Dm755 target/release/affinity-linux $(BINDIR)/affinity-linux
	install -Dm755 target/release/affinity-linux-gui $(BINDIR)/affinity-linux-gui
	install -Dm644 packaging/io.github._360900.AffinityLinux.desktop $(APPDIR)/io.github._360900.AffinityLinux.desktop
	install -Dm644 packaging/affinity-linux.svg $(ICONDIR)/affinity-linux.svg
	install -Dm644 packaging/io.github._360900.AffinityLinux.metainfo.xml $(METADIR)/io.github._360900.AffinityLinux.metainfo.xml
	install -Dm644 LICENSE $(LICDIR)/LICENSE

uninstall:
	rm -f $(BINDIR)/affinity-linux $(BINDIR)/affinity-linux-gui
	rm -f $(APPDIR)/io.github._360900.AffinityLinux.desktop
	rm -f $(ICONDIR)/affinity-linux.svg
	rm -f $(METADIR)/io.github._360900.AffinityLinux.metainfo.xml
	rm -rf $(LICDIR)

appimage: build
	packaging/appimage/build.sh
