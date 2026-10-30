#!/usr/bin/env bash
# Builds the rapo-ui wasm crate and generates JS bindings into ui-wasm/pkg.
# Requires: rustup target wasm32-unknown-unknown and wasm-bindgen-cli 0.2.130.
set -euo pipefail

cd "$(dirname "$0")/.."

cargo build --offline -p rapo-ui --target wasm32-unknown-unknown --release
wasm-bindgen --target web --out-dir ui-wasm/pkg \
    target/wasm32-unknown-unknown/release/rapo_ui.wasm
