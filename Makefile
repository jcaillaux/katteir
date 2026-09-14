# catnap developer entry points. `make help` lists them.
#
# `make run` builds and launches catnap. catnap and the AV1 video spike both
# need dav1d, which is built into .deps/ on first use together with the tools
# to build it (meson and ninja from PyPI in a virtualenv, nasm from a
# checksummed source tarball). No sudo needed.
#
# Needs: cargo, a C toolchain (cc), git, python3 with venv, make, curl, tar,
# sha256sum, pkg-config.

BIN       := target/release/catnap
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
	run-spike run-spike-break build-spike test-spike clean-spike \
	deps check-tools clean-deps

help:
	@echo "make run              build and launch catnap"
	@echo "make build            release build of catnap (builds dav1d first if needed)"
	@echo "make test             catnap unit tests"
	@echo "make clippy           clippy on catnap, warnings as errors"
	@echo "make clean            remove catnap's build output"
	@echo ""
	@echo "make run-spike        build and launch the AV1 video spike (1280x720 window)"
	@echo "make run-spike-break  same, fullscreen and see-through: the cat over the desktop"
	@echo "make build-spike      release build of the spike"
	@echo "make test-spike       the spike's unit tests"
	@echo "make clean-spike      remove the spike's build output"
	@echo "make deps             build dav1d $(DAV1D_TAG) and its build tools into .deps/"
	@echo "make clean-deps       remove .deps/"
	@echo "Spike variables: ENTRY LOOP SECS=$(SECS) THREADS=$(THREADS) SEE_THROUGH ON_TOP FULLSCREEN"

# ---- catnap -----------------------------------------------------------------

run: build
	$(BIN)

build: check-tools $(DAV1D_LIB)
	cargo build --release

test: check-tools $(DAV1D_LIB)
	cargo test --release

clippy: check-tools $(DAV1D_LIB)
	cargo clippy --release --all-targets -- -D warnings

clean:
	cargo clean

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
