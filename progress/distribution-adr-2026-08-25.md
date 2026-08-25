# ADR: one Rust engine, many distribution wrappers (2026-08-25)

## Decision

Supercov will build one Rust CLI source tree and distribute the resulting
target binaries through thin, registry-native wrappers. No registry package
may contain a second analyzer or coverage model.

For npm, the primary `supercov` package remains the `npx supercov` entrypoint
and selects an exact-version platform package. The initial matrix is macOS
arm64/x64, Linux arm64/x64 with glibc and musl, and Windows arm64/x64. Platform
packages are unscoped (`supercov-<platform>-<arch>[-<libc>]`) because the
existing npm ownership is the unscoped `supercov` package and no npm scope is
currently authenticated. All eight names were unclaimed when checked on
2026-08-25. They are generated from `npm/native-targets.json`; binary files are
never committed.

The loader rejects a platform package whose name or version differs from the
primary package. Missing optional dependencies produce an actionable error.
There is no silent network download, postinstall compilation, or fallback to
the JavaScript engine after atomic cutover. A local `target/debug` fallback is
available only in a source checkout containing `Cargo.toml`, and
`SUPERCOV_RUST_BINARY` remains a development/conformance override.

WASI is not a sound fallback for the full CLI: the engine owns arbitrary child
processes, signals, filesystem transactions, and memory-mapped indexes, which
the Node WASI environment cannot provide equivalently. Unsupported platforms
must receive an honest unsupported-target error until they have a native build.

GitHub Releases are the binary source of record. npm platform tarballs, PyPI
binary wheels, Homebrew, cargo-binstall metadata, installers, opam, and future
C-compatible packages wrap the same checksummed release artifacts. PyPI uses
maturin `bindings = "bin"`; it does not rebuild the analyzer in Python. A
future cargo-dist-versus-handwritten release-pipeline choice may change release
orchestration, not artifact identity or package contracts.

The initial artifact workflow is manual-only so ordinary commits consume no
eight-platform build minutes. It uses GitHub's native macOS arm64/x64, Linux
arm64/x64, and Windows arm64/x64 hosted runners; each Linux architecture also
builds its musl target. This avoids treating cross-compilation as native runtime
proof. GitHub documents `ubuntu-24.04-arm`, `windows-11-arm`, `macos-15`, and
their x64 counterparts as standard hosted-runner labels, including for private
repositories: <https://docs.github.com/en/actions/reference/runners/github-hosted-runners>.
Each tarball is signed with GitHub artifact-attestation build provenance using
the workflow's short-lived OIDC identity; private repositories use GitHub's
private Sigstore instance. Verification is therefore tied to repository,
workflow, commit and triggering event rather than to a long-lived signing key:
<https://docs.github.com/en/actions/concepts/security/artifact-attestations>.

## Gates

- Every target cross-compiles with warnings denied and runs on a native CI host
  before it is published.
- A packed-install test installs the primary and matching platform tarballs in
  a clean project with lifecycle scripts disabled, then completes a real run.
- Wrong version, missing package, unsupported target, wrong executable type,
  and disabled optional dependencies have deterministic tests.
- Release binaries include checksums and provenance; registry artifacts are
  constructed only from those binaries.
- Platform-package publication and the primary-package publication are one
  coordinated release. A primary version is never published before all its
  mandatory platform packages.
- The npm loader is unavoidable exec-only glue and survives TypeScript-engine
  deletion; it contains no instrumentation or analysis behavior.
