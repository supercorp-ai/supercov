#!/bin/sh
# The crate built for wasm and bound for Node.
set -e
cd rust
cargo build -q --lib --target wasm32-unknown-unknown
wasm-bindgen --target nodejs --out-dir ../pkg target/wasm32-unknown-unknown/debug/gl_matrix_wasm.wasm
