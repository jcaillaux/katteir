# catnap developer entry points. `make help` lists them.
#
# `make run` builds and launches the app. For now that's the AV1 video spike
# (spikes/av1-video), the only runnable part; point these targets at the real
# crate once M0 exists.
#
# No sudo needed. Everything native is built into .deps/ on first use:
# dav1d (static, 8-bit only), plus the tools to build it (meson and ninja from
# PyPI in a virtualenv, nasm from a checksummed source tarball).
# Needs: cargo, git, python3 with venv, a C compiler, make, curl, tar,
# sha256sum, pkg-config.

SPIKE     := spikes/av1-video
BIN       := $(SPIKE)/target/release/av1-video-spike
DEPS      := $(CURDIR)/.deps
TOOLS     := $(DEPS)/bin
VENV      := $(DEPS)/venv

DAV1D_TAG := 1.5.4
DAV1D_SRC := $(DEPS)/src/dav1d-$(DAV1D_TAG)
DAV1D     := $(DEPS)/dav1d-$(DAV1D_TAG)
DAV1D_LIB := $(DAV1D)/lib/libdav1d.a

MESON_VERSION := 1.12.0
NINJA_VERSION := 1.13.2
NASM_VERSION  := 2.16.03
# nasm.us publishes no checksum file; this is the SHA-256 of the tarball as
# first downloaded on 2026-09-14.
NASM_SHA256   := 1412a1c760bbd05db026b6c0d1657affd6631cd0a63cddb6f73cc6d4aa616148
NASM_SRC      := $(DEPS)/src/nasm-$(NASM_VERSION)

# Clips are local-only test material (dev-assets/ is gitignored, CLAUDE.md §7).
CLIPS   := dev-assets/derived
ENTRY   ?= $(CLIPS)/entry_720p30.ivf
LOOP    ?= $(CLIPS)/loop_720p15.ivf
SECS    ?= 120
THREADS ?= 1
# Window switches, 1 = on. ON_TOP does nothing on Wayland.
SEE_THROUGH ?= 0
ON_TOP      ?= 0
FULLSCREEN  ?= 0

# The .deps/ tools come first on PATH; dav1d is linked statically from the
# local build, and anything already on PKG_CONFIG_PATH is kept after it.
export PATH := $(TOOLS):$(PATH)
export PKG_CONFIG_PATH := $(DAV1D)/lib/pkgconfig$(if $(PKG_CONFIG_PATH),:$(PKG_CONFIG_PATH))
export SYSTEM_DEPS_DAV1D_LINK := static

.PHONY: help run run-break build test deps check-tools clean clean-deps

help:
	@echo "make run          build and launch the app (1280x720 window)"
	@echo "make run-break    same, fullscreen and see-through: the cat over the desktop"
	@echo "make build        release build (builds dav1d first if needed)"
	@echo "make test         unit tests"
	@echo "make deps         build dav1d $(DAV1D_TAG) and its build tools into .deps/"
	@echo "make check-tools  list missing prerequisites"
	@echo "make clean        remove the spike's build output"
	@echo "make clean-deps   remove .deps/"
	@echo "Variables: ENTRY LOOP SECS=$(SECS) THREADS=$(THREADS) SEE_THROUGH ON_TOP FULLSCREEN"
	@echo "The window closes after SECS, or when its button is held for 5 s."

run: build $(ENTRY) $(LOOP)
	SPIKE_SEE_THROUGH=$(SEE_THROUGH) SPIKE_ON_TOP=$(ON_TOP) SPIKE_FULLSCREEN=$(FULLSCREEN) \
		$(BIN) $(ENTRY) $(LOOP) $(SECS) $(THREADS)

run-break:
	@$(MAKE) --no-print-directory run SEE_THROUGH=1 FULLSCREEN=1

build: check-tools $(DAV1D_LIB)
	cargo build --release --manifest-path $(SPIKE)/Cargo.toml

test: check-tools $(DAV1D_LIB)
	cargo test --release --manifest-path $(SPIKE)/Cargo.toml

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

check-tools:
	@missing=""; \
	for tool in cargo git python3 cc make curl tar sha256sum pkg-config; do \
		command -v $$tool >/dev/null 2>&1 || missing="$$missing $$tool"; \
	done; \
	python3 -c 'import venv, ensurepip' 2>/dev/null || missing="$$missing python3-venv"; \
	if [ -n "$$missing" ]; then \
		echo "missing:$$missing" >&2; \
		echo "Debian/Ubuntu: sudo apt install git python3-venv build-essential curl pkg-config; Rust via https://rustup.rs" >&2; \
		exit 1; \
	fi

$(CLIPS)/%.ivf:
	@echo "missing clip $@: encode it as in $(SPIKE)/README.md (Clip format)" >&2; exit 1

clean:
	cargo clean --manifest-path $(SPIKE)/Cargo.toml

clean-deps:
	rm -rf $(DEPS)
