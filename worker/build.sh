#!/bin/sh
set -eu

# Workers Builds does not preinstall Rust; local builds can reuse an existing toolchain.
if ! command -v cargo >/dev/null 2>&1 || ! command -v rustup >/dev/null 2>&1; then
    curl -fsSL https://sh.rustup.rs | sh -s -- -y --profile minimal
    export PATH="${CARGO_HOME:-$HOME/.cargo}/bin:$PATH"
fi

if ! echo "$PATH" | grep -q ".cargo/bin"; then
    export PATH="${CARGO_HOME:-$HOME/.cargo}/bin:$PATH"
fi

if ! command -v worker-build >/dev/null 2>&1; then
    cargo install -q 'worker-build@^0.8'
fi

rustup target add wasm32-unknown-unknown
cd "$(dirname "$0")"
worker-build --release
