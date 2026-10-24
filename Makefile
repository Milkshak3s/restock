# Build scripts for Restock. Requires GNU Make (`winget install ezwinports.make`).
# Recipes only call cargo and powershell so they behave the same whether make
# runs them through cmd.exe or a POSIX sh (e.g. from Git Bash).
#
#   make            debug build (runs without elevation)
#   make run        build and launch the debug app
#   make release    optimized build; the exe requests admin at launch
#   make dist       release build copied to dist/restock.exe
#   make test       unit tests
#   make fmt        format the code
#   make lint       clippy, warnings are errors
#   make check      fmt-check + lint + test (run before committing)
#   make clean      remove target/ and dist/

BIN  := restock.exe
DIST := dist

.DEFAULT_GOAL := build
.PHONY: build run release dist test fmt fmt-check lint check clean

build:
	cargo build

run:
	cargo run

release:
	cargo build --release

dist: release
	powershell -NoProfile -Command "New-Item -ItemType Directory -Force $(DIST) | Out-Null; Copy-Item -Force target/release/$(BIN) $(DIST)/"

test:
	cargo test

fmt:
	cargo fmt

fmt-check:
	cargo fmt --check

lint:
	cargo clippy --all-targets -- -D warnings

check: fmt-check lint test

clean:
	cargo clean
	powershell -NoProfile -Command "Remove-Item -Recurse -Force -ErrorAction SilentlyContinue $(DIST)"
