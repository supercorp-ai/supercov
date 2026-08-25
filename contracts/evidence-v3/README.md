# Evidence archive v3 candidate

V3 retains v2's canonical sorted framing, exact compact headers, deterministic
gzip envelope, path rules, JSONL rules and atomic publication. It changes the
magic to `SUPERCOV-EVIDENCE-3\n` and makes two language-neutral entries
mandatory in addition to `manifest.json`:

- `frontend.json`: one strict frontend-protocol-v2 run declaration;
- `coverage-model.json`: schema version 1 plus the report variant, model name,
  completeness meaning, measured surfaces and unmeasured surfaces.

This prevents a reader from silently applying JavaScript semantics to Python,
Rust, C/C++, Go or OCaml evidence. The frontend limitation references must still
exactly match manifest limitation IDs and the runner declarations must exactly
match normalized evidence before analysis.

Product archives contain only Supercov-owned probe evidence. The frozen
frontend protocol can also frame native-import facts inside compile-gated
development oracle tests, but such archives are not accepted as user-run
measurement and are never a fallback product mode.

V2 remains frozen and readable. V3 writers are private until dual-read,
corruption, archive-query and lifecycle tests pass; the current public writer
continues emitting v2.
