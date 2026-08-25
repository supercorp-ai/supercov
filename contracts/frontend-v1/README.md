# Language-frontend protocol v1

Status: **frozen**. This is the boundary between a language-specific producer
and the single Rust Supercov engine. It does not create a Python, LLVM, Go, or
OCaml analyzer. A frontend discovers and transforms source, connects to its
runtime/compiler/test runner where unavoidable, and contributes normalized
facts. Rust owns every verdict and product policy after that boundary.

## Per-run declaration

Every frontend contribution has one strict `FrontendRunDeclaration`:

```json
{
  "protocolVersion": 1,
  "frontendId": "python",
  "frontendVersion": "python-v1",
  "language": "python",
  "structuralSource": "native-import",
  "runners": [{
    "runner": "pytest-xdist",
    "executionModel": "parallel-context-propagated",
    "attribution": {
      "run": "exact",
      "worker": "exact",
      "test": "exact",
      "retry": "exact",
      "phase": "exact",
      "action": "unavailable",
      "assertion": "exact"
    },
    "limitations": [{
      "id": "python-action-linkage",
      "scopes": ["action"],
      "reason": "pytest exposes assertion rewriting but no general action lifecycle"
    }]
  }],
  "structuralLimitations": []
}
```

Attribution is declared for every runner actually observed in the run, not
once for an entire language. A single command may contribute multiple runner
declarations—for example Vitest and Playwright over one JavaScript manifest.
`exact` means records carry causal identity on that
axis. `aggregate` means observations are deliberately pooled. `unavailable`
means the frontend cannot observe that axis. Every non-exact axis requires a
named limitation under that runner. `structuralLimitations` contains IDs from
the contribution's manifest; it cannot invent a second limitation shape or
hide the source location. A frontend may never infer causal action/assertion
linkage from timestamps.

## Required contributions

A frontend contributes only:

1. A complete structural manifest before test execution: source locations,
   decisions and source-ordered conditions, branch alternatives, executable
   points/functions/statements, source scope, and explicit denominator
   limitations. Dynamically generated code that cannot have a stable pre-run
   denominator is a blocking limitation, not a silently omitted file.
2. Probe observations or imported native-coverage observations. Each
   observation names an obligation from the frozen manifest. An unknown ID or
   metadata mismatch is fatal. Frontends do not add obligations after tests
   have started.
3. Run, worker, test, retry and phase identity to exactly the precision
   declared for that run. File names and process IDs are transport details and
   never establish test attribution by themselves.
4. Setup, action, assertion, teardown and background transitions. Causal
   transitions use explicit, run-unique IDs; references must resolve inside
   the same test attempt and the causal graph must be acyclic. Wall-clock
   overlap is diagnostic only.
5. Limitations describing any missing denominator or attribution capability.

The unified manifest must contain unique obligation IDs. The Rust engine owns
collision detection and assembly of contributions from mixed-language runs.
It owns evidence validation, retry/outcome selection, attribution merging,
MC/DC witness search, scoring, minimization, integrity, storage, queries,
diffs, and retention. A frontend or host shim that calculates any of those is
a second engine and violates this contract.

## Structural sources

- `owned-probes`: Supercov inserts probes ahead of execution or through a
  compiler/plugin API.
- `native-import`: a native oracle such as coverage.py, LLVM profdata, or Go
  coverage is translated into normalized obligations and observations.
- `mixed`: both sources contribute, with their boundary stated as a
  limitation when the native source cannot express the complete model.

Native import is not permission to shrink Supercov's denominator. A missing
native concept is reported as a limitation. Owned instrumentation must be
checked against the native source and the language's semantic corpus.

## Compatibility

Unknown declaration fields are rejected. A protocol change requires a new
version and explicit migration. Evidence archive framing remains independently
versioned; this protocol specifies producer meaning, not a second archive
format. The first Python and LLVM adapters must pass these same declaration
tests before either can become supported.
