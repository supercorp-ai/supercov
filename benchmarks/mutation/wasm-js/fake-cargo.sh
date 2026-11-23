#!/bin/sh
# Cargo for cargo-mutants (CARGO=fake-cargo.sh, run in the crate's
# directory): `cargo test --no-run`, its build phase, builds the wasm
# (build.sh beside it); `cargo test` runs the JavaScript specs over it
# (`npm test`); the rest is Cargo's.
if [ "$1" = "test" ]; then
  for argument in "$@"; do
    if [ "$argument" = "--no-run" ]; then cd .. && exec sh build.sh; fi
  done
  cd .. && exec npm test
fi
exec cargo "$@"
