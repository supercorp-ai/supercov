#!/bin/sh
# Cargo for cargo-mutants (CARGO=fake-cargo.sh, run in the crate's
# directory): `cargo test` runs the JavaScript specs over the crate's wasm
# (`npm test` beside it), the rest is Cargo's.
if [ "$1" = "test" ]; then cd .. && exec npm test; fi
exec cargo "$@"
