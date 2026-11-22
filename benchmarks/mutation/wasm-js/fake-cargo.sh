#!/bin/sh
# Cargo for cargo-mutants (CARGO=fake-cargo.sh): `cargo test` runs the
# JavaScript specs over the crate's wasm (`npm test`), the rest is Cargo's.
if [ "$1" = "test" ]; then exec npm test; fi
exec cargo "$@"
