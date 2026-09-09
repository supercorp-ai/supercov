# JS/TS assertion analyzer (internal)

This private package is the post-run analysis component used by Supercov's
`runs <run> assertions` command. It is bundled in the npm distribution, not
published as a separate CLI or SDK.

See [Assertion evidence](../../docs/assertion-evidence.md) for public commands,
pragma syntax, report interpretation and supported scope.

## Product boundary

- `bin/query.mjs` is the CLI-to-analyzer JSON transport. It accepts an ordinary
  archive representation prepared by Rust, validates identities and returns facts.
- `bin/identity.mjs` checks the analyzer source/build identity; its direct
  invocation writes that identity during the build.
- `bin/compiler-identity.mjs` fingerprints the project's compiler implementation.
- `src/archive.ts` validates the archive protocol, source positions and attempt
  ownership, then constructs query-local evidence in memory.
- `src/analyze.ts` extracts source-flow facts and exact assertion witnesses.
  The Rust engine joins those facts into candidate classifications.
- `src/mock-counts.ts` models bounded synchronous console-mock count histories,
  keeping count evidence separate from general value-protection claims.
- `src/frontend.ts` and `src/native-frontend.ts` implement the legacy and native
  TypeScript compiler boundaries. `src/pragmas.ts` validates optional hints.

There is no converted-input command, on-disk research input, V8 remapping,
mutation runner or diagnostic-ablation option in the product interface.
Source files ship to support build-identity verification; tests and research
tools do not ship. Assertion candidates remain unverified, not safety proofs.
Only executable JavaScript and its build identity ship from `dist`; there is no
public SDK declaration surface or generated source-map payload.

## Build and test

From this directory:

```sh
npm ci --ignore-scripts
npm test
```

The build uses pinned TypeScript 5.8.3 explicitly. Project analysis uses the
project's compiler; native 7.0.2 has a separate frontend and identity checks.
Native compiler processes close after both successful and failed queries.

From the repository root, exercise real Node/Vitest archives and the installed
npm package:

```sh
npm run test:asserted-integration
npm run test:asserted-package
```

Controlled unit fixtures test parser, flow and witness rules. The ordinary-archive
integration tests execute real suites and explicit behavioral counterexamples.
Compiler-path and identity regressions run across supported CI platforms.
Historical external-cohort research is not part of the default suite or product.
