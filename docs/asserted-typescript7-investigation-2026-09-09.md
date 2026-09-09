# Why TypeScript 7 does not work with this analyzer yet

Historical investigation. The [subsequent native frontend implementation and
calibration](asserted-typescript7-frontend-2026-09-09.md) supersede the open
implementation decision below, while retaining the API incompatibility diagnosis.

Investigated against the locally installed **7.0.2**, 2026-09-09. No application
compiler was changed and no native-API adapter was implemented.

## Diagnosis

TypeScript 7 can compile TypeScript and its new programmatic API can read our
fixture. The failure is our dependency on the **old compiler API**, not a corrupt
installation or unsupported TypeScript source syntax.

The installed package exports `typescript` as `lib/version.cjs`, providing only
`version` and `versionMajorMinor`. Supercov expects the older API there:
`createProgram`, config parsing, traversal helpers, module resolution and more.
The guard rejects this incompatibility before returning misleading empty facts.

Microsoft's [TypeScript 7 release announcement](https://devblogs.microsoft.com/typescript/announcing-typescript-7-0/)
describes the native Go port and the absence of a stable compatible programmatic
API. It explicitly recommends side-by-side TypeScript 6 for existing tools and
provides `@typescript/typescript6` and npm-alias guidance. A new API was planned
for 7.1. The installed 7.0.2 has experimental `typescript/unstable/*` entry points;
these are not the old API at a different import path.

The API is still evolving: Microsoft's [September 3 snapshot-state design issue](https://github.com/microsoft/TypeScript/issues/64154)
proposes further changes to snapshot/program lifetime and creation. That proposal
is not assumed to describe the already-installed 7.0.2 implementation.

## Local evidence

An AST-based inventory of runtime `ts.*` / `compiler.*` accesses in `analyze.ts`
and `pragmas.ts` found 80 distinct top-level member names. Of those, 79 are absent
from the 7.0.2 root entry. Even combining `unstable/ast`, `unstable/sync` and the
root version module leaves 16 names absent:

```text
canHaveModifiers, createProgram, flattenDiagnosticMessageText, forEachChild,
getModifiers, isFunctionLike, isIterationStatement, isMethodSignature,
isParameter, isStringLiteralLike, isTypeAssertionExpression,
parseJsonConfigFileContent, readConfigFile, resolveModuleName, sys, transpileModule
```

These are **name-level compatibility checks**, not a claim of 16 missing
capabilities or an effort estimate. Some have renamed equivalents. Others require
different orchestration or emit/configuration paths. Similar names also do not
prove identical semantics or syntax-kind values.

A read-only smoke probe using the actual installed native backend succeeded:

```js
import { API } from "typescript/unstable/sync";
const api = new API({ cwd: fixtureRoot, collectTiming: true });
let snapshot;
try {
  const config = api.parseConfigFile(`${fixtureRoot}/tsconfig.json`);
  snapshot = api.updateSnapshot({
    openProjects: [`${fixtureRoot}/tsconfig.json`],
  });
  const project = snapshot.getProjects()[0];
  const source = project.program.getSourceFile(`${fixtureRoot}/src/core.ts`);
  // One project, 493 source characters, five top-level statements.
} finally {
  snapshot?.dispose();
  api.close();
}
```

Fixture: `analyzers/typescript/tests/fixtures/basic`. The observed one-shot cost
was about 74 ms locally, including startup and reads; this is **not** a full
analyzer benchmark. The source nodes retain `getStart`, `getText` and
`getSourceFile`, so that portion of the old usage has a possible migration path.
`program.getSourceFiles()` and `program.getTypeChecker()` are absent; this API
instead exposes `getSourceFileNames()`, individual reads and `project.checker`.
Its synchronous interface talks to the native compiler process through a
request/response channel. It is not all in-process JavaScript.

The native checker was also exercised, not just queried for its method names:
`project.checker.getTypeAtLocation` and `typeToString` returned the expected
number-to-number signatures for `increment` and `wrapper`, the two string
literals for `choose`, and a void-returning callback signature for `notify`.

There is a concrete **semantic-default difference** even in this tiny fixture:
the same `cleanup` source is `(timer: Timeout) => Timeout` under 5.8.3 and
`(timer: number) => number` under the native 7.0.2 API. The native project did not
load `@types/node`; this is consistent with the new default `types: []` described
in [Microsoft's TypeScript 6 release notes](https://devblogs.microsoft.com/typescript/announcing-typescript-6-0/).
This is a type-environment difference, not evidence that the runtime test changed
behavior. It illustrates why importing a legacy compiler silently is not an
accuracy-preserving substitute for using the project's compiler/configuration.

## Implications and next choice

1. **Smallest compatibility investigation:** test Microsoft's TypeScript 6
   compatibility package alongside a TypeScript 7 build. This could let users
   retain their build compiler, without implementing the new backend. It must
   be an explicit analyzer/compiler selection with recorded provenance, not a
   silent fallback or a claim that TypeScript 6 exactly models all TypeScript 7
   semantics. It has not been calibrated or shipped here.
2. **Native TypeScript 7 integration:** separate program/config/module-resolution
   access from AST traversal, adapt the actual 7.0.2 methods, and calibrate on the
   same fixed cohorts plus changed compiler defaults and syntax. Include native
   binary identity and process cleanup in the provenance/lifecycle design.
3. **Current release:** retain the explicit experimental limit. A real native
   adapter would run after tests; there is no reason to add test-time probes for
   it. Its speed and classification accuracy still need measurement.

Do not remove the compatibility guard or merely spread the new exports into a
fake old `ts` object. That would get past a shallow check without establishing
the semantics the analyzer relies on.
