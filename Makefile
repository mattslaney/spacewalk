.DEFAULT_GOAL := help

CARGO ?= cargo
PYTHON ?= python3
ARGS ?=
INSTALL_ARGS ?=

.PHONY: help build run install check fmt fmt-check lint test installer-check clean

help:
	@printf '%s\n' \
	  'Spacewalk development tasks:' \
	  '  make build       Build the release binary' \
	  '  make run         Run in release mode (ARGS="..." passes CLI arguments)' \
	  '  make install     Install into Cargo bin directory (INSTALL_ARGS="...")' \
	  '  make check       Run formatting, lint, tests, and release build checks' \
	  '  make fmt         Format Rust source' \
	  '  make fmt-check   Check Rust formatting' \
	  '  make lint        Run Clippy with warnings treated as errors' \
	  '  make test        Run the test suite' \
	  '  make installer-check  Check installer syntax and offline installation tests' \
	  '  make clean       Remove build artifacts'

build:
	$(CARGO) build --release --locked

run:
	$(CARGO) run --release --locked -- $(ARGS)

install:
	$(CARGO) install --path . --locked $(INSTALL_ARGS)

check: fmt-check lint test installer-check build

fmt:
	$(CARGO) fmt

fmt-check:
	$(CARGO) fmt --check

lint:
	$(CARGO) clippy --locked --all-targets -- -D warnings

test:
	$(CARGO) test --locked

installer-check:
	sh -n install.sh
	$(PYTHON) -m unittest discover -s tests -p 'test_installer.py' -v

clean:
	$(CARGO) clean
