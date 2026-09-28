# navico-neon-extender. Machine-specific targets (deploying to a particular Pi) go in Makefile.local,
# which is gitignored and loaded below when present.

.PHONY: all build release test clippy fmt check

all: build

build:
	cargo build

release:
	cargo build --release

test:
	cargo test

clippy:
	cargo clippy --all-targets -- -D warnings

fmt:
	cargo fmt --all

# What CI checks.
check:
	cargo fmt --all --check
	cargo clippy --all-targets -- -D warnings
	cargo test

-include Makefile.local
