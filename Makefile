# Build order matters: the Rust staticlib must exist before swift build links.
# Run everything from the repo root. Build outputs can be moved to a local
# volume when the checkout filesystem has slow or unreliable metadata I/O.
# Example: `CARGO_TARGET_DIR=/tmp/tokenbar-cargo-target SWIFT_BUILD_PATH=/tmp/tokenbar-swift-build make build`.
CARGO_TARGET_DIR ?= target
SWIFT_BUILD_PATH ?= .build
TOKENBAR_RUST_LIBRARY_DIR ?= $(CARGO_TARGET_DIR)/release
export TOKENBAR_RUST_LIBRARY_DIR

.PHONY: all rust build run clean check-docs

all: build

check-docs:
	python3 scripts/check_knowledge.py

rust:
	cargo build --release

build: rust
	@$(call relink_if_stale,debug)
	swift build --build-path "$(SWIFT_BUILD_PATH)"

run: rust
	@$(call relink_if_stale,debug)
	swift run --build-path "$(SWIFT_BUILD_PATH)" TokenBar

clean:
	cargo clean
	swift package clean

bundle: rust
	@$(call relink_if_stale,release)
	swift build -c release --build-path "$(SWIFT_BUILD_PATH)"
	scripts/bundle.sh

# SwiftPM does not track the Rust staticlib as a dependency: with no Swift
# source changes it reuses the cached executable and silently ships stale
# Rust code. Drop the executable whenever the staticlib is newer.
define relink_if_stale
	if [ "$(TOKENBAR_RUST_LIBRARY_DIR)/libtb_core_ffi.a" -nt "$(SWIFT_BUILD_PATH)/$(1)/TokenBar" ]; then \
		rm -f "$(SWIFT_BUILD_PATH)/$(1)/TokenBar"; \
	fi
endef
