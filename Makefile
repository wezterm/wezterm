.PHONY: all fmt build check test docs servedocs build-release app install-app deb rpm clean

UNAME := $(shell uname)
TAG_NAME ?= $(shell git -c core.abbrev=8 show -s --format=%cd-%h --date=format:%Y%m%d-%H%M%S)
TARGET_DIR ?= target
INSTALL_DIR ?= /Applications

RELEASE_BINS := wezterm wezterm-gui wezterm-mux-server strip-ansi-escapes

all: build

test:
	cargo nextest run
	cargo nextest run -p wezterm-escape-parser # no_std by default

check:
	cargo check
	cargo check -p wezterm-escape-parser
	cargo check -p wezterm-cell
	cargo check -p wezterm-surface
	cargo check -p wezterm-ssh

build:
	cargo build $(BUILD_OPTS) -p wezterm
	cargo build $(BUILD_OPTS) -p wezterm-gui
	cargo build $(BUILD_OPTS) -p wezterm-mux-server
	cargo build $(BUILD_OPTS) -p strip-ansi-escapes

build-release:
	cargo build --release -p wezterm -p wezterm-gui -p wezterm-mux-server -p strip-ansi-escapes

# Assemble dist/WezTerm.app from the release binaries (macOS)
app: build-release
	rm -rf dist
	mkdir -p dist
	cp -R assets/macos/WezTerm.app dist/WezTerm.app
	rm -f dist/WezTerm.app/*.dylib
	mkdir -p dist/WezTerm.app/Contents/MacOS dist/WezTerm.app/Contents/Resources
	cp -R assets/shell-integration/* dist/WezTerm.app/Contents/Resources/
	cp -R assets/shell-completion dist/WezTerm.app/Contents/Resources/
	tic -xe wezterm -o dist/WezTerm.app/Contents/Resources/terminfo termwiz/data/wezterm.terminfo
	for bin in $(RELEASE_BINS); do \
		cp $(TARGET_DIR)/release/$$bin dist/WezTerm.app/Contents/MacOS/$$bin || exit 1; \
	done
	codesign --force --deep --sign - dist/WezTerm.app
	@echo "Built dist/WezTerm.app ($(TAG_NAME))"

# Install the built app into /Applications (macOS)
install-app: app
	rm -rf $(INSTALL_DIR)/WezTerm.app
	cp -R dist/WezTerm.app $(INSTALL_DIR)/WezTerm.app
	@echo "Installed $(INSTALL_DIR)/WezTerm.app"

# Build the .deb package (Debian/Ubuntu Linux only — the package
# must contain Linux ELF binaries, so this cannot run on macOS)
deb:
	@test "$(UNAME)" = "Linux" || { \
		echo "make deb: requires Linux (the package must contain Linux ELF binaries)."; \
		echo "Run this on a Debian/Ubuntu host or container, e.g.:"; \
		echo "  docker run --rm -v \$$PWD:/src -w /src ubuntu:24.04 bash -c \"; apt-get update && apt-get install -y build-essential curl pkg-config libfontconfig1-dev cmake fakeroot dpkg-dev file && make deb\""; \
		exit 1; }
	@test -x /usr/bin/dpkg-deb || { echo "make deb: dpkg-deb not found. Install: sudo apt install dpkg-dev fakeroot"; exit 1; }
	./ci/deploy.sh $(TARGET_DIR)

# Build the .rpm package (Fedora/RHEL/openSUSE Linux only)
rpm:
	@test "$(UNAME)" = "Linux" || { \
		echo "make rpm: requires Linux (the package must contain Linux ELF binaries)."; \
		echo "Run this on a Fedora/RHEL host or container."; \
		exit 1; }
	@test -x /usr/bin/rpmbuild || { echo "make rpm: rpmbuild not found. Install: sudo dnf install rpm-build"; exit 1; }
	./ci/deploy.sh $(TARGET_DIR)

clean:
	cargo clean
	rm -rf dist

fmt:
	cargo +nightly fmt

docs:
	ci/build-docs.sh

servedocs:
	ci/build-docs.sh serve
