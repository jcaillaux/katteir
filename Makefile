# Katteir developer entry points. `make help` lists them.
#
# `make run` builds and launches the app. It and the AV1 video spike both
# need dav1d, which is built into .deps/ on first use together with the tools
# to build it (meson and ninja from PyPI in a virtualenv, nasm from a
# checksummed source tarball). No sudo needed.
#
# Needs: cargo, a C toolchain (cc), git, python3 with venv, make, curl, tar,
# sha256sum, pkg-config.

# The app's names, read from Cargo.toml: the one place they're written.
CRATE     := $(shell sed -n 's/^name = "\(.*\)"$$/\1/p' Cargo.toml | head -n 1)
APP_ID    := $(shell sed -n 's/^identifier = "\(.*\)"$$/\1/p' Cargo.toml)
APP_NAME  := $(shell sed -n 's/^product-name = "\(.*\)"$$/\1/p' Cargo.toml)
BIN       := target/release/$(CRATE)
RUN_FEATURES ?= clip-fields
SPIKE     := spikes/av1-video
SPIKE_BIN := $(SPIKE)/target/release/av1-video-spike
DEPS      := $(CURDIR)/.deps
TOOLS     := $(DEPS)/bin
VENV      := $(DEPS)/venv

DAV1D_TAG := 1.5.4
DAV1D_SRC := $(DEPS)/src/dav1d-$(DAV1D_TAG)
DAV1D     := $(DEPS)/dav1d-$(DAV1D_TAG)
DAV1D_LIB := $(DAV1D)/lib/libdav1d.a

# Provisional, see CLAUDE.md §6 (the venv will be set up with uv instead).
MESON_VERSION := 1.12.0
NINJA_VERSION := 1.13.2
NASM_VERSION  := 2.16.03
# nasm.us publishes no checksum file; this is the SHA-256 of the tarball as
# first downloaded on 2026-09-14.
NASM_SHA256   := 1412a1c760bbd05db026b6c0d1657affd6631cd0a63cddb6f73cc6d4aa616148
NASM_SRC      := $(DEPS)/src/nasm-$(NASM_VERSION)

# Spike clips are local-only test material (dev-assets/ is gitignored, CLAUDE.md §7).
CLIPS   := dev-assets/derived
ENTRY   ?= $(CLIPS)/entry_720p30.ivf
LOOP    ?= $(CLIPS)/loop_720p15.ivf
SECS    ?= 120
THREADS ?= 1
# Spike window switches, 1 = on. ON_TOP does nothing on Wayland.
SEE_THROUGH ?= 0
ON_TOP      ?= 0
FULLSCREEN  ?= 0

# The .deps/ tools come first on PATH; dav1d is linked statically from the
# local build, and anything already on PKG_CONFIG_PATH is kept after it.
export PATH := $(TOOLS):$(PATH)
export PKG_CONFIG_PATH := $(DAV1D)/lib/pkgconfig$(if $(PKG_CONFIG_PATH),:$(PKG_CONFIG_PATH))
export SYSTEM_DEPS_DAV1D_LINK := static

.PHONY: help run build test clippy clean \
	test-live install-desktop uninstall-desktop refresh-desktop-caches deb check-packager packaging-tools \
	notices check-about cat-ginger \
	run-spike run-spike-break build-spike test-spike clean-spike \
	deps check-tools clean-deps

help:
	@echo "make run              build and launch $(APP_NAME), with the clip path fields (RUN_FEATURES=$(RUN_FEATURES))"
	@echo "make build            release build of $(APP_NAME), as packages have it (builds dav1d first if needed)"
	@echo "make test             $(APP_NAME) unit tests"
	@echo "make test-live        tests against the real session bus (shows nothing)"
	@echo "make clippy           clippy on $(APP_NAME), warnings as errors"
	@echo "make clean            remove $(APP_NAME)'s build output"
	@echo "make install-desktop  desktop entry + icon in ~/.local/share, so docks show $(APP_NAME)'s icon"
	@echo "make uninstall-desktop  remove them"
	@echo "make deb              Debian package in target/release (cargo-packager $(PACKAGER_VERSION))"
	@echo "make notices          third-party licence notices in $(NOTICES) (cargo-about $(ABOUT_VERSION))"
	@echo "make packaging-tools  install cargo-packager and cargo-about at those versions"
	@echo "make cat-ginger       rebuild the ginger cat's clips from its footage (uv, ffmpeg)"
	@echo ""
	@echo "make run-spike        build and launch the AV1 video spike (1280x720 window)"
	@echo "make run-spike-break  same, fullscreen and see-through: the cat over the desktop"
	@echo "make build-spike      release build of the spike"
	@echo "make test-spike       the spike's unit tests"
	@echo "make clean-spike      remove the spike's build output"
	@echo "make deps             build dav1d $(DAV1D_TAG) and its build tools into .deps/"
	@echo "make clean-deps       remove .deps/"
	@echo "Spike variables: ENTRY LOOP SECS=$(SECS) THREADS=$(THREADS) SEE_THROUGH ON_TOP FULLSCREEN"

# ---- the app ----------------------------------------------------------------

# The dev build: RUN_FEATURES turns on the settings window's clip path
# fields, which `make build` and packages leave out. `make run RUN_FEATURES=`
# shows the window as packages have it.
run: check-tools $(DAV1D_LIB)
	cargo build --release --features '$(RUN_FEATURES)'
	$(BIN)

build: check-tools $(DAV1D_LIB)
	cargo build --release

test: check-tools $(DAV1D_LIB)
	cargo test --release

# The #[ignore]d tests: D-Bus calls to this desktop's session bus and
# notification server. Read-only; nothing appears on screen.
test-live: check-tools $(DAV1D_LIB)
	cargo test --release -- --ignored --nocapture

clippy: check-tools $(DAV1D_LIB)
	cargo clippy --release --all-targets -- -D warnings

clean:
	cargo clean

# Docks and app menus find the app's icon through a desktop entry named after
# its app id. This installs one for this checkout, in your home only, from
# the assets/app.desktop template; release packages will install it
# properly (M4).
DATA_HOME    := $(or $(XDG_DATA_HOME),$(HOME)/.local/share)
DESKTOP_FILE := $(DATA_HOME)/applications/$(APP_ID).desktop
ICON_FILE    := $(DATA_HOME)/icons/hicolor/scalable/apps/$(APP_ID).svg

# The desktop entry from its template; $(1) is what Exec runs.
desktop_entry = sed -e '/^\#/d' -e 's|@NAME@|$(APP_NAME)|' -e 's|@ID@|$(APP_ID)|g' -e 's|@EXEC@|$(1)|' assets/app.desktop

install-desktop: build
	install -Dm644 assets/icons/tray.svg $(ICON_FILE)
	mkdir -p $(dir $(DESKTOP_FILE))
	$(call desktop_entry,$(CURDIR)/$(BIN)) > $(DESKTOP_FILE)
	$(MAKE) --no-print-directory refresh-desktop-caches
	@echo "Installed $(DESKTOP_FILE) and $(ICON_FILE)."
	@echo "A dock that was already running (Crystal Dock, Plank...) may need a restart to show the icon."

uninstall-desktop:
	rm -f $(DESKTOP_FILE) $(ICON_FILE)
	$(MAKE) --no-print-directory refresh-desktop-caches

# An icon cache that predates the icon hides it from GTK, so rebuild it (and
# the desktop database) when the tools exist; both are optional.
refresh-desktop-caches:
	@if command -v gtk-update-icon-cache >/dev/null; then \
		gtk-update-icon-cache -q -t -f $(DATA_HOME)/icons/hicolor; fi
	@if command -v update-desktop-database >/dev/null; then \
		update-desktop-database -q $(DATA_HOME)/applications; fi

# ---- packages ---------------------------------------------------------------

# The .deb, by cargo-packager (configured in Cargo.toml): the binary in
# /usr/bin, plus our desktop entry and icon, staged here under the app id's
# name, and the licence files in /usr/share/doc (cargo-packager doesn't put
# them in a .deb). Depends lists the libraries the app loads at run time, which dpkg
# can't see (GL/EGL, fontconfig, Wayland, X11, xkbcommon), and libc at the
# newest version the binary needs, measured on it.
PACKAGER_VERSION := 0.11.8
DEB_FILES   := target/deb-files
DEB_DEPENDS := libgcc-s1 libegl1 libgl1 libfontconfig1 libwayland-client0 libwayland-egl1 \
	libx11-6 libx11-xcb1 libxcb1 libxcursor1 libxi6 libxrender1 libxkbcommon0 libxkbcommon-x11-0

deb: build check-packager notices
	rm -rf $(DEB_FILES)
	install -Dm644 assets/icons/tray.svg $(DEB_FILES)/usr/share/icons/hicolor/scalable/apps/$(APP_ID).svg
	install -Dm644 -t $(DEB_FILES)/usr/share/doc/$(CRATE) LICENSE-MIT LICENSE-APACHE assets/LICENSE-CC-BY-NC-4.0.txt $(NOTICES)
	install -Dm644 assets/LICENSE.md $(DEB_FILES)/usr/share/doc/$(CRATE)/LICENSE-ASSETS.md
	mkdir -p $(DEB_FILES)/usr/share/applications
	$(call desktop_entry,$(CRATE)) > $(DEB_FILES)/usr/share/applications/$(APP_ID).desktop
	{ objdump -T $(BIN) | grep -o 'GLIBC_[0-9.]*' | sort -uV | tail -n 1 | sed 's/^GLIBC_/libc6 (>= /; s/$$/)/'; \
		printf '%s\n' $(DEB_DEPENDS); } > target/deb-depends
	cargo packager --release
	@ls -l target/release/*.deb

check-packager:
	@cargo packager --version 2>/dev/null | grep -qx 'cargo-packager $(PACKAGER_VERSION)' || \
		{ echo "needs cargo-packager $(PACKAGER_VERSION): cargo install cargo-packager --version $(PACKAGER_VERSION) --locked"; exit 1; }

# The packaging tools at the versions above, each installed only if it's
# missing or another version: for a fresh machine, such as CI's containers.
packaging-tools:
	cargo packager --version 2>/dev/null | grep -qx 'cargo-packager $(PACKAGER_VERSION)' || \
		cargo install cargo-packager --version $(PACKAGER_VERSION) --locked
	cargo about --version 2>/dev/null | grep -qx 'cargo-about $(ABOUT_VERSION)' || \
		cargo install cargo-about --version $(ABOUT_VERSION) --locked --features cli

# The licences of the third-party code in the binary: every crate, from
# cargo-about (tools/about.toml, tools/notices.hbs; --fail stops on a
# licence it can't place). Then what cargo-about leaves out: Slint's
# royalty-free licence, with the Slint crates as cargo tree finds them
# (cargo-about 0.9.2 drops LicenseRef texts, see tools/about.toml, and
# -L error silences its warnings about them), and dav1d, C built from
# source, which cargo-about can't see. Both read the whole dependency graph,
# every platform's crates included, offline: cargo fetch downloads what a
# build didn't (nothing, once they're cached).
ABOUT_VERSION := 0.9.2
NOTICES       := target/THIRD-PARTY-NOTICES.txt
RULE          := ------------------------------------------------------------------------

notices: check-about $(DAV1D_LIB)
	cargo fetch --locked
	cargo about -L error generate --fail --offline -c tools/about.toml tools/notices.hbs -o $(NOTICES)
	{ printf '\n%s\nLicenseRef-Slint-Royalty-free-2.0 (Slint)\n\nUsed by:\n' '$(RULE)'; \
		cargo tree --offline -e normal --target x86_64-unknown-linux-gnu --prefix none --format '{p}|{l}' \
			| grep 'LicenseRef-Slint-Royalty-free' | cut -d'|' -f1 \
			| sed 's/ (.*//; s/ v\([0-9]\)/ \1/; s/^/  /' | sort -u; \
		printf '\n'; cat patches/i-slint-core/LICENSES/LicenseRef-Slint-Royalty-free-2.0.md; \
		printf '\n%s\ndav1d %s (the AV1 decoder, C, built from source)\n\n' '$(RULE)' '$(DAV1D_TAG)'; \
		cat $(DAV1D_SRC)/COPYING; } >> $(NOTICES)

check-about:
	@cargo about --version 2>/dev/null | grep -qx 'cargo-about $(ABOUT_VERSION)' || \
		{ echo "needs cargo-about $(ABOUT_VERSION): cargo install cargo-about --version $(ABOUT_VERSION) --locked --features cli"; exit 1; }

# ---- cat footage (dev machine only) -----------------------------------------

# The ginger cat, from AI footage (assets/cats/ginger/prompt.txt). The source
# stays local, in the gitignored dev-assets/. The times were measured on it:
# the entry starts where the rest of the walk covers one screen width (the
# slide in ui/cat.slint), and the loop goes back and forth between two tops
# of a breath (14.58 and 24.83 s), where the motion turns anyway.
GINGER_SRC ?= dev-assets/seedance/cgt-20260915035359-6c5p8.mp4
GINGER_CUT := dev-assets/derived/ginger

cat-ginger:
	uv run tools/cutout.py $(GINGER_SRC) $(GINGER_CUT) --entry-start 1.583 --loop-start 14.583 --loop-end 24.833 --loop pingpong
	mkdir -p assets/cats/ginger
	tools/encode.sh $(GINGER_CUT)/entry.mkv assets/cats/ginger/entry.ivf 24
	tools/encode.sh $(GINGER_CUT)/sleep.mkv assets/cats/ginger/sleep.ivf 24

# ---- AV1 video spike --------------------------------------------------------

run-spike: build-spike $(ENTRY) $(LOOP)
	SPIKE_SEE_THROUGH=$(SEE_THROUGH) SPIKE_ON_TOP=$(ON_TOP) SPIKE_FULLSCREEN=$(FULLSCREEN) \
		$(SPIKE_BIN) $(ENTRY) $(LOOP) $(SECS) $(THREADS)

run-spike-break:
	@$(MAKE) --no-print-directory run-spike SEE_THROUGH=1 FULLSCREEN=1

build-spike: check-tools $(DAV1D_LIB)
	cargo build --release --manifest-path $(SPIKE)/Cargo.toml

test-spike: check-tools $(DAV1D_LIB)
	cargo test --release --manifest-path $(SPIKE)/Cargo.toml

clean-spike:
	cargo clean --manifest-path $(SPIKE)/Cargo.toml

# ---- dav1d and its build tools ------------------------------------------------

deps: $(DAV1D_LIB)

# Order-only prerequisites (after `|`): run the check first without forcing
# a rebuild of the file targets.
$(DAV1D_LIB): $(TOOLS)/meson $(TOOLS)/ninja $(TOOLS)/nasm | check-tools
	rm -rf $(DAV1D_SRC) $(DEPS)/build-dav1d
	git clone --quiet --depth 1 --branch $(DAV1D_TAG) https://code.videolan.org/videolan/dav1d.git $(DAV1D_SRC)
	meson setup $(DEPS)/build-dav1d $(DAV1D_SRC) --buildtype=release --default-library=static \
		-Dbitdepths=8 -Denable_tools=false -Denable_tests=false -Denable_examples=false \
		--prefix=$(DAV1D) --libdir=lib
	ninja -C $(DEPS)/build-dav1d install

$(TOOLS)/meson $(TOOLS)/ninja &: | check-tools
	python3 -m venv $(VENV)
	$(VENV)/bin/pip install --quiet --disable-pip-version-check meson==$(MESON_VERSION) ninja==$(NINJA_VERSION)
	mkdir -p $(TOOLS)
	ln -sf $(VENV)/bin/meson $(TOOLS)/meson
	ln -sf $(VENV)/bin/ninja $(TOOLS)/ninja

$(TOOLS)/nasm: | check-tools
	rm -rf $(NASM_SRC) $(DEPS)/src/nasm.tar.xz
	mkdir -p $(DEPS)/src $(TOOLS)
	curl -sSfL -o $(DEPS)/src/nasm.tar.xz https://www.nasm.us/pub/nasm/releasebuilds/$(NASM_VERSION)/nasm-$(NASM_VERSION).tar.xz
	echo "$(NASM_SHA256)  $(DEPS)/src/nasm.tar.xz" | sha256sum --check --quiet
	tar -xJf $(DEPS)/src/nasm.tar.xz -C $(DEPS)/src
	cd $(NASM_SRC) && ./configure --quiet >/dev/null && $(MAKE) --no-print-directory -s -j$$(nproc) nasm >/dev/null
	cp $(NASM_SRC)/nasm $(TOOLS)/nasm

clean-deps:
	rm -rf $(DEPS)

# ---- prerequisite check -------------------------------------------------------

check-tools:
	@missing=""; \
	for tool in cargo cc git python3 make curl tar sha256sum pkg-config; do \
		command -v $$tool >/dev/null 2>&1 || missing="$$missing $$tool"; \
	done; \
	python3 -c 'import venv, ensurepip' 2>/dev/null || missing="$$missing python3-venv"; \
	if [ -n "$$missing" ]; then \
		echo "missing:$$missing" >&2; \
		echo "Debian/Ubuntu: sudo apt install build-essential git python3-venv curl pkg-config; Rust via https://rustup.rs" >&2; \
		exit 1; \
	fi

$(CLIPS)/%.ivf:
	@echo "missing clip $@: encode it as in $(SPIKE)/README.md (Clip format)" >&2; exit 1
