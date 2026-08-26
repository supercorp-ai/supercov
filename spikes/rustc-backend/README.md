# rustc backend feasibility spike

This development-only crate tests whether an exact rustc-versioned compiler
bridge can see the expanded HIR/MIR and source provenance required by
`rust-source-v1`. It is not linked into the Supercov product and is not a user
dependency.

Build it only with the repository's pinned toolchain and matching development
components:

```sh
rustup component add rustc-dev llvm-tools rust-src --toolchain 1.95.0
RUSTC_BOOTSTRAP=1 cargo build --manifest-path spikes/rustc-backend/Cargo.toml
```

`npm run test:rustc-backend-spike` runs the checked-in
macro/generated/const/doctest fixture through the resulting binary as
`RUSTC_WRAPPER`, validates compiler provenance, and proves that an optimized
MIR query replacement reaches emitted code. The companion injects its probe
runtime as an in-memory crate item, calls it from MIR, proves the observations
arrive, compares values/errors/panics/drop order/stdout/stderr to an ordinary
build, and verifies the fixture source hash is unchanged.

The same executable also overrides `mir_for_ctfe` for a const function. It
inserts in-memory block markers and concurrency-safe edge markers into split
CTFE edges, then observes only those markers through an in-process rustc
interpreter event subscriber. The fixture evaluates both sides at compile
time; the test requires both edge observations, all original blocks, identical
const values and byte-identical program stdout/stderr. The subscriber has no
formatting/output layer, so compiler-internal events do not leak into user
output.

This is a feasibility proof, not the production const implementation. Stable
manifest identities, every const/static/inline-const surface, crash-safe
publication, ordinary `RUSTC_LOG` coexistence and performance remain explicit
gates. External LLVM/rustc coverage is not used. All fixture targets and
observations live in a temporary directory and are deleted after the spike.
