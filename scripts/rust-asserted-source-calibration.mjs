// New adversarial fixture, independent of the frozen first runtime oracle.
// With SUPERCOV_CARGO_MUTANTS set, check every fresh native outcome against the
// source-hashed oracle. Explicit --record prints a candidate oracle for review;
// it never silently updates the checked-in oracle or treats generated labels
// as expected results.
import assert from "node:assert/strict";
import { createHash } from "node:crypto";
import { readFileSync, writeFileSync } from "node:fs";
import { join } from "node:path";
import { captureFixture, root } from "./rust-asserted-calibration-harness.mjs";

const record = process.argv.includes("--record");
const aliases = process.argv.includes("--aliases");
const identities = process.argv.includes("--identities");
assert.ok(!(aliases && identities), "choose one fixture mode");
const fixture = identities
  ? "rust-asserted-identity"
  : aliases
    ? "rust-asserted-alias"
    : "rust-asserted-source";
const expectedSites = identities ? 2 : aliases ? 11 : 13;
const expectedBoundaries = identities ? 1 : 4;
const expectedCalls = identities
  ? ["compute(21)", "compute(21)"]
  : aliases
    ? [
        "alias_replacement(2)",
        "boolean_chain(1)",
        "chain(21)",
        "preserved_source(21)",
      ]
    : [
        "aliased(21)",
        "asserted_source_fixture::reversed(-2)",
        "local_boolean(1)",
        "zero_exact(0)",
      ];
const oraclePath = join(
  root,
  `crates/supercov-engine/tests/fixtures/${fixture}-oracle.json`,
);
const saved = record ? null : JSON.parse(readFileSync(oraclePath, "utf8"));
const files = {};
// The only new inputs are Cargo-tracked compile-time cfg and manifest data.
// Compare the same fixture with collection disabled before enabling it.
const originalFlags = process.env.RUSTFLAGS;
assert.ok(
  !identities || !originalFlags?.includes("supercov_assertion_identities"),
  "identity mode controls its own off/on flag",
);
const withoutIdentities = identities ? captureFixture(fixture) : null;
if (identities)
  process.env.RUSTFLAGS =
    `${originalFlags ?? ""} --cfg supercov_assertion_identities`.trim();
const { temporary, project, archive, report, run, timings } = captureFixture(
  fixture,
  (project) => {
    for (const file of ["Cargo.toml", "src/lib.rs", "tests/contract.rs"]) {
      files[file] = createHash("sha256")
        .update(readFileSync(join(project, file)))
        .digest("hex");
    }
    if (saved)
      assert.deepEqual(
        files,
        saved.files,
        "source changed: regenerate oracle explicitly",
      );
  },
);
if (identities) {
  if (originalFlags === undefined) delete process.env.RUSTFLAGS;
  else process.env.RUSTFLAGS = originalFlags;
  const before = withoutIdentities.report;
  assert.equal(before.compilerAssertionIdentities, null);
  assert.equal(before.sourceAnalysis.witnesses.length, 0);
  assert.deepEqual(report.coverageInventory, before.coverageInventory);
  assert.deepEqual(report.analysis, before.analysis);
  const real = report.points.find((p) => p.source.includes("fn real("));
  const decoy = report.points.find((p) => p.source.includes("fn compute("));
  assert.ok(
    report.sourceAnalysis.witnesses.every(
      (w) => w.compilerResolved && w.point === real.id,
    ),
  );
  const records = report.compilerAssertionIdentities.records;
  assert.equal(
    records.filter(
      (r) =>
        r.kind === "call" &&
        r.file === "tests/contract.rs" &&
        r.target === real.id,
    ).length,
    2,
  );
  assert.equal(
    records.filter((r) => r.kind === "macro" && r.standardEquality).length,
    2,
  );
  assert.ok(
    records.some(
      (r) => r.kind === "call" && r.owner === real.id && r.target === decoy.id,
    ),
  );
  const direct = report.sourceAnalysis.witnesses.find((w) => !w.viaLocal);
  assert.ok(
    report.analysis.executionLinks.some(
      (l) => l.point === decoy.id && l.phase === direct.phase,
    ),
  );
}
const points = new Map(report.points.map((p) => [p.id, p]));
const witnesses = report.sourceAnalysis.witnesses;
assert.deepEqual(witnesses.map((w) => w.call).sort(), expectedCalls);
assert.equal(
  witnesses.filter((w) => w.viaLocal).length,
  identities ? 1 : aliases ? 4 : 2,
);
const expectedBindings = identities
  ? {}
  : aliases
    ? {
        "alias_replacement(2)": ["alias_replacement(2)"],
        "boolean_chain(1)": ["boolean_chain(1)", "result"],
        "chain(21)": ["chain(21)", "result", "first", "first"],
        "preserved_source(21)": ["preserved_source(21)", "result", "saved"],
      }
    : { "aliased(21)": ["aliased(21)", "result"] };
for (const [call, initializers] of Object.entries(expectedBindings)) {
  const witness = witnesses.find((w) => w.call === call);
  assert.deepEqual(
    witness.bindings.map((b) => b.initializer),
    initializers,
    call,
  );
  assert.equal(witness.producer, witness.bindings[0].at);
  assert.equal(
    new Set(witness.bindings.map((b) => b.at)).size,
    witness.bindings.length,
  );
}
assert.equal(
  report.analysis.facts.tests.length,
  identities ? 2 : aliases ? 8 : 11,
);
assert.equal(report.analysis.facts.sites.length, expectedSites);
assert.equal(report.sourceAnalysis.facts.sites.length, expectedSites);
assert.equal(
  report.sourceResolutions.filter((r) => r.status === "evident").length,
  expectedBoundaries,
);
assert.equal(
  report.sourceResolutions.filter((r) => r.status === "unresolved").length,
  expectedSites - expectedBoundaries,
);
assert.equal(report.analysis.unsettledAssertions.length, 0);
assert.ok(report.resolutions.every((r) => r.status === "unresolved"));
if (!aliases && !identities) {
  assert.equal(
    report.sourceResolutions.find((r) =>
      points.get(r.site).source.includes("fn eq("),
    )?.status,
    "unresolved",
  );
} else if (aliases) {
  const never = report.sourceResolutions.find((r) =>
    points.get(r.site).source.includes("fn never_called("),
  );
  assert.equal(never.status, "unresolved");
  assert.equal(never.reason.kind, "gap:not-reached");
}

const mutants = process.env.SUPERCOV_CARGO_MUTANTS;
let outcomes = saved?.outcomes;
let totals = saved?.totals;
if (mutants) {
  assert.match(
    run("mutants-version", mutants, ["mutants", "--version"]).stdout,
    /cargo-mutants 27\.1\.0\b/,
  );
  run(
    "mutants",
    mutants,
    [
      "mutants",
      "--dir",
      project,
      "--file",
      "src/lib.rs",
      "--jobs",
      "1",
      "--no-shuffle",
      "--timeout",
      "15",
      "--",
      "--offline",
    ],
    { allowedStatuses: [2] },
  );
  const raw = JSON.parse(
    readFileSync(join(project, "mutants.out/outcomes.json"), "utf8"),
  );
  assert.equal(raw.outcomes[0].scenario, "Baseline");
  assert.equal(raw.outcomes[0].summary, "Success");
  writeFileSync(
    join(temporary, "mutants-outcomes.json"),
    JSON.stringify(raw, null, 2),
  );
  outcomes = raw.outcomes.slice(1).map((o) => ({
    name: o.scenario.Mutant.name,
    function: o.scenario.Mutant.function.function_name,
    location: o.scenario.Mutant.function.span.start,
    status: o.summary,
  }));
  totals = {
    mutants: raw.total_mutants,
    caught: raw.caught,
    survived: raw.missed,
    unviable: raw.unviable,
    timeouts: raw.timeout,
  };
  assert.equal(outcomes.length, raw.total_mutants, "no discarded outcomes");
  if (saved) {
    assert.deepEqual(
      outcomes,
      saved.outcomes,
      "fresh complete mutation oracle changed",
    );
    assert.deepEqual(totals, saved.totals);
  }
  if (record)
    writeFileSync(
      join(temporary, "candidate-oracle.json"),
      JSON.stringify(
        {
          schema: 1,
          cargoMutantsVersion: "27.1.0",
          files,
          totals,
          wallSeconds: timings.mutants / 1000,
          outcomes,
        },
        null,
        2,
      ) + "\n",
    );
}
assert.ok(
  outcomes,
  "--record requires SUPERCOV_CARGO_MUTANTS; no invented oracle",
);
const perFunction = {};
for (const outcome of outcomes) {
  // Match Cargo's function span (one-based column) to the compiler inventory,
  // including trait methods. Do not silently ignore an unmapped function.
  const point = report.points.find(
    (p) =>
      p.line === outcome.location.line &&
      p.column + 1 === outcome.location.column,
  );
  assert.ok(point, `unmapped mutant: ${outcome.name}`);
  const row = (perFunction[outcome.function] ??= {
    exactReturnWitness: witnesses.some((w) => w.point === point.id),
    outcomes: {},
  });
  row.outcomes[outcome.status] = (row.outcomes[outcome.status] ?? 0) + 1;
}
// This is a scope counterexample, not a false mutation prediction: the exact
// result at input zero is observed, but changes preserving zero still survive.
if (identities) {
  assert.equal(perFunction.real.exactReturnWitness, true);
  assert.ok(perFunction.real.outcomes.CaughtMutant > 0);
  assert.equal(perFunction.compute.exactReturnWitness, false);
  assert.ok(perFunction.compute.outcomes.MissedMutant > 0);
} else if (aliases) {
  assert.equal(perFunction.boolean_chain.exactReturnWitness, true);
  assert.ok(perFunction.boolean_chain.outcomes.MissedMutant > 0);
  for (const name of ["chain", "preserved_source", "alias_replacement"]) {
    assert.equal(perFunction[name].exactReturnWitness, true, name);
    assert.ok(perFunction[name].outcomes.CaughtMutant > 0, name);
  }
  for (const name of [
    "source_replacement",
    "old_alias",
    "constant_shadow",
    "mutable_alias",
    "never_called",
  ]) {
    assert.equal(perFunction[name].exactReturnWitness, false, name);
    assert.ok(perFunction[name].outcomes.MissedMutant > 0, name);
  }
  assert.equal(perFunction.nested_alias.exactReturnWitness, false);
  assert.ok(perFunction.nested_alias.outcomes.CaughtMutant > 0);
} else {
  assert.equal(perFunction.zero_exact.exactReturnWitness, true);
  assert.ok(perFunction.zero_exact.outcomes.MissedMutant > 0);
  assert.equal(perFunction.aliased.exactReturnWitness, true);
  assert.ok(perFunction.aliased.outcomes.CaughtMutant > 0);
}
const comparison = {
  fixture,
  oracle: mutants
    ? "fresh cargo-mutants 27.1.0"
    : "saved cargo-mutants 27.1.0, source hashes verified",
  totals,
  unmapped: 0,
  perFunction,
  implementedAnalysis: {
    exactReturnWitnesses: witnesses.length,
    unresolvedFunctions: expectedSites - expectedBoundaries,
    mutationVerdicts: 0,
    globalAssertionScore: null,
  },
  sourceReadVerifyAnalyzeMs: report.sourceReadVerifyAnalyzeMs,
  identityOffOn: withoutIdentities
    ? {
        offArtifacts: withoutIdentities.temporary,
        sameCoverageInventoryAndRuntimeAnalysis: true,
        offTimings: withoutIdentities.timings,
        offSourceReadVerifyAnalyzeMs:
          withoutIdentities.report.sourceReadVerifyAnalyzeMs,
      }
    : null,
};
writeFileSync(
  join(temporary, "comparison.json"),
  JSON.stringify(comparison, null, 2),
);
writeFileSync(
  join(temporary, "timings.json"),
  JSON.stringify(timings, null, 2),
);
console.log(
  JSON.stringify({ temporary, archive, comparison, timings }, null, 2),
);
