#!/usr/bin/env bash
# Developer workflow commands for this repository.
# Read by AI coding agents and humans alike.

export RUSTFLAGS="-Dwarnings"

# Build all workspace crates (debug)
b()  { cargo build --workspace; }

# Run all tests (unit + integration + doc tests)
t()  { cargo test --workspace; }

# Run a single test by name filter, e.g. `tt parser` or `tt http1::`
tt() { cargo test --workspace --lib "$1"; }

# Lint with clippy (deny warnings)
l()  { cargo clippy --workspace --all-targets --all-features; }

# Format check (no edits) — run `cargo fmt` to apply
f()  { cargo fmt --all -- --check; }

# Format (apply)
fmt() { cargo fmt --all; }

# Full pre-commit gate: fmt + clippy + test
check() { cargo fmt --all -- --check && cargo clippy --workspace --all-targets --all-features && cargo test --workspace; }

# Run the CLI (dev build)
run() { cargo run -p aegis-cli -- "$@"; }

# Build in release mode (production binary)
rel() { cargo build --release -p aegis-cli; }

# Benchmark suite (criterion)
bench() { cargo bench --workspace; }

# Fuzz a target, e.g. `fz http1_parser`
fz()   { cargo +nightly fuzz run "$1"; }
