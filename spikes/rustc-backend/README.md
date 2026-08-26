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
MIR query replacement reaches emitted code. External LLVM/rustc coverage is
not used. All fixture targets and observations live in a temporary directory
and are deleted after the spike.
